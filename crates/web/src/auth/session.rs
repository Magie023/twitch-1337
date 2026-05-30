//! In-memory session table.
//!
//! Sessions sit behind a `RwLock<HashMap>` keyed by a 64-hex-char random
//! id. TTL is sliding: every successful `get_and_touch` bumps `last_seen`,
//! so an active user stays logged in indefinitely. The role-gate middleware
//! also stamps `last_role_check` so it knows when to re-verify the helix
//! moderator list.
//!
//! The TTL is **not** stored in the table — callers pass it on each
//! `get_and_touch` call. This allows the session TTL to be read live from
//! the settings handle so dashboard changes take effect on subsequent
//! requests without a restart.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use eyre::Result;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use twitch_1337_core::util::persist::atomic_save_ron_async;

use crate::auth::role::Role;
use crate::clock::Clock;

pub type SessionId = String;

/// Inputs for [`SessionTable::insert`]. Named fields keep the call sites
/// readable instead of relying on positional `String, String, Role, Option<String>`.
pub struct NewSession {
    pub user_id: String,
    pub user_login: String,
    pub role: Role,
    pub avatar_url: Option<String>,
    pub is_broadcaster: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub user_id: String,
    pub user_login: String,
    pub role: Role,
    pub issued_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_role_check: DateTime<Utc>,
    pub csrf_value: [u8; 32],
    /// Twitch helix `profile_image_url` captured at login. Static for the
    /// session lifetime; sidebar reads this without per-request helix calls.
    pub avatar_url: Option<String>,
    /// True when `user_id` matches the configured broadcaster. Captured at
    /// session creation so role badges and broadcaster-only UI bits don't
    /// need to re-read app state.
    pub is_broadcaster: bool,
}

impl Session {
    pub fn is_mod(&self) -> bool {
        // Owner is a strict superset of Mod — anywhere we ask "is this user
        // at least a moderator?" Owner must answer true.
        self.role >= Role::Mod
    }
}

pub struct SessionTable {
    inner: RwLock<HashMap<SessionId, Session>>,
    clock: Arc<dyn Clock>,
    /// Backing file under `$DATA_DIR`. `None` keeps the table purely
    /// in-memory (tests, web-dev bin) so they never touch disk.
    path: Option<PathBuf>,
    /// Serializes [`persist`](Self::persist) calls. `persist` is invoked from
    /// three concurrent contexts (login, logout, the periodic snapshot task)
    /// and `atomic_save_ron_async` always writes the same `.ron.tmp` sibling —
    /// overlapping writes would tear that temp file. Holding this across the
    /// whole snapshot+write makes the last writer win cleanly.
    write_lock: tokio::sync::Mutex<()>,
}

impl SessionTable {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            clock,
            path: None,
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Construct a disk-backed table, loading any previously persisted
    /// sessions so a bot restart (every rolling deploy) does not log everyone
    /// out — the signed sid cookie still matches a live table entry.
    ///
    /// A missing file starts empty; a corrupt/unparseable file is logged and
    /// also starts empty, so a bad file costs at most one forced re-login
    /// instead of wedging startup.
    pub fn load(clock: Arc<dyn Clock>, path: PathBuf) -> Self {
        let mut inner = match std::fs::read_to_string(&path) {
            Ok(data) => match ron::from_str::<HashMap<SessionId, Session>>(&data) {
                Ok(map) => map,
                Err(error) => {
                    tracing::warn!(?error, path = %path.display(), "Failed to parse sessions; starting empty");
                    HashMap::new()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                tracing::warn!(?error, path = %path.display(), "Failed to read sessions; starting empty");
                HashMap::new()
            }
        };
        // Force a fresh helix role re-check on the first request of every
        // restored session. Without this a moderator demoted while the bot was
        // down would keep dashboard access until `mod_check_refresh_secs`
        // elapsed — the pre-persistence restart wiped sessions and re-evaluated
        // roles at the forced re-login, and we must not regress that.
        for s in inner.values_mut() {
            s.last_role_check = DateTime::<Utc>::MIN_UTC;
        }
        Self {
            inner: RwLock::new(inner),
            clock,
            path: Some(path),
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Snapshot the table and atomically write it to disk. No-op when the
    /// table has no backing path.
    ///
    /// The whole snapshot+write runs under `write_lock` so concurrent callers
    /// (login, logout, the periodic task) can't tear the shared `.ron.tmp`.
    /// The snapshot is a quick clone under the `inner` read lock, which is
    /// released before the disk `await`, so only the (already serialized)
    /// writers wait — readers never block on IO.
    pub async fn persist(&self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let _write = self.write_lock.lock().await;
        let snapshot = self.inner.read().unwrap().clone();
        if let Err(error) = atomic_save_ron_async(&snapshot, &path).await {
            tracing::warn!(?error, "Failed to persist sessions");
        }
    }

    /// Returns the new session id together with the freshly-generated csrf
    /// value so the OAuth callback can set both cookies without a second
    /// lookup against the table.
    pub fn insert(&self, new: NewSession) -> Result<(SessionId, [u8; 32])> {
        let mut rng = rand::rng();
        let mut id_bytes = [0u8; 32];
        rng.fill_bytes(&mut id_bytes);
        let mut csrf = [0u8; 32];
        rng.fill_bytes(&mut csrf);
        let id = hex::encode(id_bytes);
        self.insert_at(&id, csrf, new);
        Ok((id, csrf))
    }

    /// Insert a session at a caller-chosen id with a caller-chosen csrf.
    /// Dev-only: the web-dev bin and `/_dev/login` use this to seed a
    /// deterministic session that survives server restarts.
    #[cfg(feature = "dev-login")]
    pub fn insert_with_id(&self, id: &str, csrf: [u8; 32], new: NewSession) {
        self.insert_at(id, csrf, new);
    }

    fn insert_at(&self, id: &str, csrf: [u8; 32], new: NewSession) {
        let now = self.clock.now();
        self.inner.write().unwrap().insert(
            id.to_owned(),
            Session {
                user_id: new.user_id,
                user_login: new.user_login,
                role: new.role,
                issued_at: now,
                last_seen: now,
                last_role_check: now,
                csrf_value: csrf,
                avatar_url: new.avatar_url,
                is_broadcaster: new.is_broadcaster,
            },
        );
    }

    /// Check whether the session with `id` is still live under `ttl` and, if
    /// so, slide its `last_seen` timestamp and return a clone.
    ///
    /// `ttl` is passed by the caller on every call so the value is always the
    /// **current** effective setting — dashboard changes to `session_ttl_secs`
    /// take effect on the next request without a restart.
    pub fn get_and_touch(&self, id: &str, ttl: Duration) -> Option<Session> {
        let now = self.clock.now();
        let ttl = chrono::Duration::from_std(ttl).ok()?;
        let mut g = self.inner.write().unwrap();
        let session = g.get_mut(id)?;
        if now.signed_duration_since(session.last_seen) > ttl {
            g.remove(id);
            return None;
        }
        session.last_seen = now;
        Some(session.clone())
    }

    pub fn drop_session(&self, id: &str) {
        self.inner.write().unwrap().remove(id);
    }

    pub fn record_role_check(&self, id: &str) {
        let now = self.clock.now();
        if let Some(s) = self.inner.write().unwrap().get_mut(id) {
            s.last_role_check = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Manually-advanced clock so TTL behaviour is deterministic.
    struct TestClock {
        now: Mutex<DateTime<Utc>>,
    }

    impl TestClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                now: Mutex::new(DateTime::<Utc>::UNIX_EPOCH),
            })
        }
        fn advance(&self, by: Duration) {
            let mut g = self.now.lock().unwrap();
            *g += chrono::Duration::from_std(by).unwrap();
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.now.lock().unwrap()
        }
    }

    fn sample_session() -> NewSession {
        NewSession {
            user_id: "u1".to_owned(),
            user_login: "login1".to_owned(),
            role: Role::Mod,
            avatar_url: None,
            is_broadcaster: false,
        }
    }

    const TTL: Duration = Duration::from_secs(3600);

    #[test]
    fn get_and_touch_returns_live_session() {
        let clock = TestClock::new();
        let table = SessionTable::new(clock);
        table.insert_at("sid", [0u8; 32], sample_session());

        let got = table.get_and_touch("sid", TTL).expect("session is live");
        assert_eq!(got.user_id, "u1");
        assert_eq!(got.role, Role::Mod);
    }

    #[test]
    fn get_and_touch_unknown_id_is_none() {
        let table = SessionTable::new(TestClock::new());
        assert!(table.get_and_touch("missing", TTL).is_none());
    }

    #[test]
    fn expired_session_is_evicted() {
        let clock = TestClock::new();
        let table = SessionTable::new(clock.clone());
        table.insert_at("sid", [0u8; 32], sample_session());

        clock.advance(TTL + Duration::from_secs(1));
        assert!(
            table.get_and_touch("sid", TTL).is_none(),
            "session past ttl must not validate"
        );
        // Prove the expired lookup *evicted* the entry rather than merely
        // rejecting it: a still-present entry (last_seen at epoch) would
        // validate against a 10-year ttl, but an evicted one stays gone.
        let huge_ttl = Duration::from_secs(10 * 365 * 24 * 3600);
        assert!(
            table.get_and_touch("sid", huge_ttl).is_none(),
            "expired session must be removed from the table, not just rejected"
        );
    }

    #[test]
    fn activity_slides_the_window() {
        let clock = TestClock::new();
        let table = SessionTable::new(clock.clone());
        table.insert_at("sid", [0u8; 32], sample_session());

        // Repeatedly touch just under the ttl; total elapsed exceeds one ttl
        // but the session stays live because last_seen keeps moving forward.
        for _ in 0..5 {
            clock.advance(TTL - Duration::from_secs(1));
            assert!(
                table.get_and_touch("sid", TTL).is_some(),
                "sliding window should keep an active session alive"
            );
        }
    }

    #[test]
    fn drop_session_removes_entry() {
        let table = SessionTable::new(TestClock::new());
        table.insert_at("sid", [0u8; 32], sample_session());
        table.drop_session("sid");
        assert!(table.get_and_touch("sid", TTL).is_none());
    }

    #[test]
    fn record_role_check_stamps_current_time() {
        let clock = TestClock::new();
        let table = SessionTable::new(clock.clone());
        table.insert_at("sid", [0u8; 32], sample_session());

        clock.advance(Duration::from_secs(120));
        table.record_role_check("sid");

        let session = table.get_and_touch("sid", TTL).unwrap();
        assert_eq!(
            session.last_role_check,
            DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(120)
        );
    }

    #[test]
    fn is_mod_tracks_role_tier() {
        let mk = |role| Session {
            user_id: "u".into(),
            user_login: "l".into(),
            role,
            issued_at: DateTime::<Utc>::UNIX_EPOCH,
            last_seen: DateTime::<Utc>::UNIX_EPOCH,
            last_role_check: DateTime::<Utc>::UNIX_EPOCH,
            csrf_value: [0u8; 32],
            avatar_url: None,
            is_broadcaster: false,
        };
        assert!(!mk(Role::Viewer).is_mod());
        assert!(mk(Role::Mod).is_mod());
        assert!(mk(Role::Owner).is_mod(), "owner is a superset of mod");
    }
}
