use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use twitch_1337_web::auth::Role;
use twitch_1337_web::auth::session::SessionTable;
use twitch_1337_web::clock::Clock;

struct StubClock(std::sync::Mutex<DateTime<Utc>>);

impl Clock for StubClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

impl StubClock {
    fn new(t: DateTime<Utc>) -> Self {
        Self(std::sync::Mutex::new(t))
    }
    fn advance(&self, secs: i64) {
        let mut g = self.0.lock().unwrap();
        *g += chrono::Duration::seconds(secs);
    }
}

#[test]
fn session_round_trips() {
    let clock = Arc::new(StubClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    let table = SessionTable::new(clock.clone());
    let (id, _csrf) = table
        .insert(twitch_1337_web::auth::session::NewSession {
            user_id: "12345".into(),
            user_login: "alice".into(),
            role: Role::Mod,
            avatar_url: None,
            is_broadcaster: false,
        })
        .expect("insert");
    let got = table
        .get_and_touch(&id, Duration::from_secs(7 * 24 * 3600))
        .expect("present");
    assert_eq!(got.user_login, "alice");
    assert_eq!(got.user_id, "12345");
}

#[test]
fn session_expires_after_ttl() {
    let clock = Arc::new(StubClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    let table = SessionTable::new(clock.clone());
    let (id, _csrf) = table
        .insert(twitch_1337_web::auth::session::NewSession {
            user_id: "12345".into(),
            user_login: "alice".into(),
            role: Role::Mod,
            avatar_url: None,
            is_broadcaster: false,
        })
        .unwrap();
    clock.advance(61);
    assert!(
        table.get_and_touch(&id, Duration::from_secs(60)).is_none(),
        "expected expiry past TTL"
    );
}

#[test]
fn session_sliding_refresh_keeps_alive() {
    let clock = Arc::new(StubClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    let table = SessionTable::new(clock.clone());
    let (id, _csrf) = table
        .insert(twitch_1337_web::auth::session::NewSession {
            user_id: "12345".into(),
            user_login: "alice".into(),
            role: Role::Mod,
            avatar_url: None,
            is_broadcaster: false,
        })
        .unwrap();
    let ttl = Duration::from_secs(120);
    clock.advance(60);
    assert!(table.get_and_touch(&id, ttl).is_some()); // bumps last_seen
    clock.advance(90);
    assert!(
        table.get_and_touch(&id, ttl).is_some(),
        "sliding refresh should keep alive"
    );
    clock.advance(150);
    assert!(table.get_and_touch(&id, ttl).is_none());
}

#[tokio::test]
async fn sessions_survive_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sessions.ron");
    let clock = Arc::new(StubClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));

    let table = SessionTable::load(clock.clone(), path.clone());
    let (id, _csrf) = table
        .insert(twitch_1337_web::auth::session::NewSession {
            user_id: "12345".into(),
            user_login: "alice".into(),
            role: Role::Mod,
            avatar_url: Some("https://example.invalid/a.png".into()),
            is_broadcaster: false,
        })
        .expect("insert");
    table.persist().await;
    drop(table);

    // Simulate a bot restart: a fresh table reading the same file must still
    // recognise the sid the user's signed cookie carries.
    let reloaded = SessionTable::load(clock.clone(), path);
    let got = reloaded
        .get_and_touch(&id, Duration::from_secs(7200))
        .expect("session survives reload");
    assert_eq!(got.user_login, "alice");
    assert_eq!(got.role, Role::Mod);
    // last_role_check is reset on load so the gate re-verifies the helix mod
    // list on the first post-restart request (a mod demoted while the bot was
    // down must not keep access on stale state).
    assert_eq!(
        got.last_role_check,
        DateTime::<Utc>::MIN_UTC,
        "restored session must force a fresh role check"
    );
}

#[test]
fn corrupt_session_file_starts_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sessions.ron");
    std::fs::write(&path, "this is not valid ron {{{").expect("write garbage");
    let clock = Arc::new(StubClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    // Must not panic — a corrupt file costs one re-login, never a crash.
    let table = SessionTable::load(clock, path);
    assert!(
        table
            .get_and_touch("deadbeef", Duration::from_secs(7200))
            .is_none()
    );
}
