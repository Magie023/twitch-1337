//! Filesystem layer for v2 memory: read/write/list under per-path mutex,
//! atomic tmp+rename, byte caps, soul seeding, v1 disposal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use eyre::{Result, WrapErr as _, eyre};
use tokio::sync::Mutex;
use tracing::info;

use crate::ai::memory::frontmatter;
use crate::ai::memory::sanitize::normalize_display_name;
use crate::ai::memory::types::{Caps, FileKind, Frontmatter, MemoryFile};
use crate::settings::SettingsHandle;
use crate::util::persist::atomic_write_bytes_async;

/// On-disk modification time, milliseconds since the UNIX epoch. Used as a
/// lost-update token by the dashboard memory editor.
pub type Mtime = u64;

/// Slugs the dashboard reserves for literal `/memory/state/...` routes; the
/// store rejects writes targeting these names so the AI's `write_state` tool
/// gets the same protection the route layer already applies. See spec
/// "Routes" — the literal `/memory/state/new` route lives next to the
/// dynamic `/memory/state/{slug}` capture, so a user-controlled `new` slug
/// would shadow the create form.
pub const RESERVED_STATE_SLUGS: &[&str] = &["new", "delete"];

/// `^[a-zA-Z0-9._-]{1,64}$` and not in `RESERVED_STATE_SLUGS`. Mirrors the
/// route-layer `is_valid_slug` check so the store is authoritative whether
/// the call comes from the web dashboard, the AI tool, or the dreamer
/// ritual. Keeping the rule in the store closes the gap a route-only check
/// would leave open.
pub fn validate_state_slug(slug: &str) -> Result<(), WriteError> {
    if slug.is_empty() || slug.len() > 64 {
        return Err(WriteError::InvalidSlug);
    }
    if RESERVED_STATE_SLUGS.contains(&slug) {
        return Err(WriteError::InvalidSlug);
    }
    if slug.contains("..") {
        return Err(WriteError::InvalidSlug);
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(WriteError::InvalidSlug);
    }
    Ok(())
}

/// Outcome of `MemoryStore::write_with_guard`: either the write went
/// through (caller can read the new mtime back) or the on-disk mtime
/// disagreed with the caller's expected token, in which case the dashboard
/// renders the conflict template with the current body for manual merge.
#[derive(Debug)]
pub enum WriteOutcome {
    Written {
        new_mtime: Mtime,
    },
    Conflict {
        current_body: String,
        current_mtime: Mtime,
    },
}

/// Per-write frontmatter overrides for `write_with_guard`. `Some(non_empty)`
/// replaces the on-disk value; `Some("")` and `None` both preserve the
/// prior value (matches `write`'s existing empty-string filtering, so
/// untouched form inputs round-trip safely through the conflict resubmit
/// path which doesn't carry these fields).
#[derive(Default, Debug, Clone)]
pub struct FrontmatterOverride {
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub created_by: Option<String>,
}

const SOUL_SEED: &str = include_str!("../../../data/prompts/seed_soul.md");

#[derive(Clone)]
pub struct MemoryStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    root: PathBuf,         // $DATA_DIR
    memories_dir: PathBuf, // $DATA_DIR/memories
    settings: SettingsHandle,
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    /// Serialises new state-file creation so the `max_state_files` cap can't
    /// be smuggled past by two concurrent writes to two distinct new slugs.
    /// Held only across the count-check + write path inside `write_state`.
    state_create_lock: Mutex<()>,
}

impl MemoryStore {
    pub async fn open(data_dir: &Path, settings: SettingsHandle) -> Result<Self> {
        let memories_dir = data_dir.join("memories");
        for p in [
            &memories_dir,
            &memories_dir.join("users"),
            &memories_dir.join("state"),
            &memories_dir.join("transcripts"),
        ] {
            tokio::fs::create_dir_all(p)
                .await
                .wrap_err_with(|| format!("create_dir_all {}", p.display()))?;
        }

        // v1 disposal.
        let v1 = data_dir.join("ai_memory.ron");
        if tokio::fs::try_exists(&v1).await.unwrap_or(false) {
            let ts = Utc::now().timestamp();
            let dest = data_dir.join(format!("ai_memory.ron.discarded-{ts}"));
            tokio::fs::rename(&v1, &dest).await.ok();
            info!(target = %dest.display(), "Renamed v1 ai_memory.ron — v2 starts fresh");
        }

        // SOUL.md seed.
        let soul_path = memories_dir.join("SOUL.md");
        if !tokio::fs::try_exists(&soul_path).await.unwrap_or(false) {
            let fm = Frontmatter {
                updated_at: Utc::now(),
                username: None,
                display_name: None,
                created_by: None,
            };
            let raw = frontmatter::emit(&fm, SOUL_SEED);
            atomic_write_bytes_async(raw.as_bytes(), &soul_path)
                .await
                .wrap_err("seed SOUL.md")?;
        }
        // LORE.md seed (empty body).
        let lore_path = memories_dir.join("LORE.md");
        if !tokio::fs::try_exists(&lore_path).await.unwrap_or(false) {
            let fm = Frontmatter {
                updated_at: Utc::now(),
                username: None,
                display_name: None,
                created_by: None,
            };
            atomic_write_bytes_async(frontmatter::emit(&fm, "").as_bytes(), &lore_path)
                .await
                .wrap_err("seed LORE.md")?;
        }
        Ok(Self {
            inner: Arc::new(StoreInner {
                root: data_dir.to_path_buf(),
                memories_dir,
                settings,
                locks: Mutex::new(HashMap::new()),
                state_create_lock: Mutex::new(()),
            }),
        })
    }

    /// Read the current byte caps live from the settings handle. Lock-free
    /// (`ArcSwap::load` is a pointer swap) so safe to call on every operation.
    pub fn caps(&self) -> Caps {
        Caps::from_settings(&self.inner.settings.load())
    }

    pub fn memories_dir(&self) -> &Path {
        &self.inner.memories_dir
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    async fn lock_for(&self, rel: &Path) -> Arc<Mutex<()>> {
        let mut g = self.inner.locks.lock().await;
        g.entry(rel.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Read a single file by `FileKind`. Missing → empty body, default frontmatter.
    pub async fn read_kind(&self, kind: &FileKind) -> Result<MemoryFile> {
        let rel = kind.relative_path();
        let abs = self.inner.memories_dir.join(&rel);
        match tokio::fs::read_to_string(&abs).await {
            Ok(raw) => {
                let (fm, body) =
                    frontmatter::parse(&raw).map_err(|e| eyre!("parse {}: {e}", rel.display()))?;
                Ok(MemoryFile {
                    kind: kind.clone(),
                    frontmatter: fm,
                    body,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MemoryFile {
                kind: kind.clone(),
                frontmatter: Frontmatter {
                    updated_at: Utc::now(),
                    username: None,
                    display_name: None,
                    created_by: None,
                },
                body: String::new(),
            }),
            Err(e) => Err(eyre!(e)),
        }
    }

    /// Write a SOUL/LORE/user file. For user files, `username` (Twitch login)
    /// and `display_name` (cased) are persisted in the frontmatter. Either
    /// `None` arg preserves the existing value on disk so the dreamer or
    /// moderators writing other people's user files don't clobber identity.
    pub async fn write(
        &self,
        kind: &FileKind,
        body: &str,
        username: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<(), WriteError> {
        let limit = self.caps().limit_for(kind);

        let rel = kind.relative_path();
        let abs = self.inner.memories_dir.join(&rel);
        let lock = self.lock_for(&rel).await;
        let _g = lock.lock().await;

        // Build the emitted file inside the lock so the cap covers the *full*
        // on-disk content (frontmatter + body). Doing this before acquiring
        // the lock would let two concurrent writes both pass a body-only cap
        // check and then race the actual write.
        let (username, display_name) = if matches!(kind, FileKind::User { .. }) {
            let username = username.map(str::to_string).filter(|s| !s.is_empty());
            let display_name = display_name
                .map(normalize_display_name)
                .filter(|s| !s.is_empty());
            // For user files, preserve any prior identity fields the caller
            // didn't supply — protects mod/dreamer writes from clobbering
            // display info they don't have on hand. Skip the disk read when
            // the caller already supplied both fields.
            if username.is_some() && display_name.is_some() {
                (username, display_name)
            } else {
                let prior = tokio::fs::read_to_string(&abs)
                    .await
                    .ok()
                    .and_then(|raw| frontmatter::parse(&raw).ok().map(|(fm, _)| fm));
                (
                    username.or_else(|| prior.as_ref().and_then(|fm| fm.username.clone())),
                    display_name.or_else(|| prior.as_ref().and_then(|fm| fm.display_name.clone())),
                )
            }
        } else {
            (None, None)
        };

        let fm = Frontmatter {
            updated_at: Utc::now(),
            username,
            display_name,
            created_by: None,
        };
        let raw = frontmatter::emit(&fm, body);
        if raw.len() > limit {
            return Err(WriteError::Full);
        }

        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| WriteError::Io(eyre!(e)))?;
        }
        atomic_write_bytes_async(raw.as_bytes(), &abs)
            .await
            .map_err(|e| WriteError::Io(eyre!(e)))?;
        Ok(())
    }

    pub async fn write_state(
        &self,
        kind: &FileKind,
        body: &str,
        creator_user_id: Option<&str>,
    ) -> Result<(), WriteError> {
        self.write_state_inner(kind, body, creator_user_id, None)
            .await
    }

    /// `created_by_override`: `Some(non_empty)` replaces the on-disk
    /// `created_by`; `Some("")` and `None` both fall back to preserve-or-init.
    async fn write_state_inner(
        &self,
        kind: &FileKind,
        body: &str,
        creator_user_id: Option<&str>,
        created_by_override: Option<&str>,
    ) -> Result<(), WriteError> {
        let FileKind::State { slug } = kind else {
            return Err(WriteError::Io(eyre!(
                "write_state called on non-state kind"
            )));
        };
        // Validate before touching the filesystem — the AI tool, the
        // dashboard, and the dreamer all funnel through this method, so the
        // slug rule lives here rather than at any one call site.
        validate_state_slug(slug)?;
        let limit = self.caps().state_bytes;

        let rel = kind.relative_path();
        let abs = self.inner.memories_dir.join(&rel);
        let lock = self.lock_for(&rel).await;
        let _g = lock.lock().await;

        let prior_raw = tokio::fs::read_to_string(&abs).await.ok();
        let is_new = prior_raw.is_none();
        let prior_created_by = prior_raw
            .as_deref()
            .and_then(|raw| frontmatter::parse(raw).ok())
            .and_then(|(fm, _)| fm.created_by);
        let created_by = match created_by_override {
            Some(s) if !s.is_empty() => Some(s.to_string()),
            _ => prior_created_by.or_else(|| creator_user_id.map(std::string::ToString::to_string)),
        };

        let fm = Frontmatter {
            updated_at: Utc::now(),
            username: None,
            display_name: None,
            created_by,
        };
        let raw = frontmatter::emit(&fm, body);
        if raw.len() > limit {
            return Err(WriteError::Full);
        }

        // For new files only, serialise the count-check + write through the
        // global state-create lock so two new-slug writes can't both pass.
        let _create_g = if is_new {
            Some(self.inner.state_create_lock.lock().await)
        } else {
            None
        };
        if is_new {
            let count = count_state_files(&self.inner.memories_dir).await?;
            if count >= self.caps().max_state_files {
                return Err(WriteError::StateFull);
            }
        }

        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| WriteError::Io(eyre!(e)))?;
        }
        atomic_write_bytes_async(raw.as_bytes(), &abs)
            .await
            .map_err(|e| WriteError::Io(eyre!(e)))?;
        let _ = slug; // slug already encoded in rel path
        Ok(())
    }

    /// Returns the on-disk modification time of `kind` as milliseconds since
    /// the UNIX epoch. Missing files return `0` so the editor can render a
    /// new-document mtime token without a separate "exists?" probe.
    ///
    /// Used by the dashboard editor (Task 5) to embed an `mtime` token in
    /// the form so a subsequent save can detect lost-update conflicts.
    pub async fn current_mtime(&self, kind: &FileKind) -> Result<Mtime, WriteError> {
        let abs = self.inner.memories_dir.join(kind.relative_path());
        match tokio::fs::metadata(&abs).await {
            Ok(meta) => {
                let modified = meta
                    .modified()
                    .map_err(|e| WriteError::Io(eyre!("modified: {e}")))?;
                let dur = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| WriteError::Io(eyre!("epoch: {e}")))?;
                u64::try_from(dur.as_millis())
                    .map_err(|_| WriteError::Io(eyre!("mtime overflow: u128 -> u64")))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(WriteError::Io(eyre!("metadata: {e}"))),
        }
    }

    /// Lost-update-guarded write used by the dashboard memory editor.
    ///
    /// `expected = Some(mtime)` re-reads the on-disk mtime under the
    /// per-path mutex and aborts with `WriteOutcome::Conflict` (returning
    /// the current body so the conflict template can show both versions
    /// side-by-side) when the disk state doesn't match.
    /// `expected = None` is unconditional — used by the dreamer ritual and
    /// the AI `write_file` tool, which serialise through the same per-path
    /// mutex map and don't carry a token.
    ///
    /// `id` is informational; the path is encoded in `kind`. Kept on the
    /// signature so call sites read fluently.
    ///
    /// TOCTOU note: we drop the outer guard between the mtime check and
    /// the inner `write` / `write_state` (which take their own per-path
    /// lock). In that gap an unconditional write (AI tool or dreamer
    /// ritual) can land between the dashboard user's check and their
    /// write — the dashboard write then silently overwrites that change
    /// without surfacing a conflict. Acceptable for v1 since every write
    /// path serialises on the same per-path mutex map and the gap is
    /// short; the dashboard user's intent is always preserved.
    pub async fn write_with_guard(
        &self,
        kind: FileKind,
        id: &str,
        body: &str,
        expected: Option<Mtime>,
        fm_override: FrontmatterOverride,
    ) -> Result<WriteOutcome, WriteError> {
        let _ = id; // already encoded in `kind`; arg kept for ergonomic call sites

        let rel = kind.relative_path();
        let lock = self.lock_for(&rel).await;
        let g = lock.lock().await;

        if let Some(exp) = expected {
            let current = self.current_mtime(&kind).await?;
            if current != exp {
                let body = self
                    .read_kind(&kind)
                    .await
                    .map(|f| f.body)
                    .unwrap_or_default();
                return Ok(WriteOutcome::Conflict {
                    current_body: body,
                    current_mtime: current,
                });
            }
        }

        drop(g);
        match &kind {
            FileKind::State { slug } => {
                self.write_state_inner(
                    &FileKind::State { slug: slug.clone() },
                    body,
                    None,
                    fm_override.created_by.as_deref(),
                )
                .await?;
            }
            FileKind::User { .. } => {
                self.write(
                    &kind,
                    body,
                    fm_override.username.as_deref(),
                    fm_override.display_name.as_deref(),
                )
                .await?;
            }
            FileKind::Soul | FileKind::Lore => {
                self.write(&kind, body, None, None).await?;
            }
        }
        let new_mtime = self.current_mtime(&kind).await?;
        Ok(WriteOutcome::Written { new_mtime })
    }

    pub async fn delete_state(&self, slug: &str) -> Result<()> {
        // Reject reserved/path-traversal slugs symmetrically with `write_state`.
        validate_state_slug(slug).map_err(|e| eyre!("delete_state: {e}"))?;
        let rel = PathBuf::from(format!("state/{slug}.md"));
        let abs = self.inner.memories_dir.join(&rel);
        let lock = self.lock_for(&rel).await;
        let _g = lock.lock().await;
        match tokio::fs::remove_file(&abs).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(eyre!(e)),
        }
    }

    pub async fn list_users(&self) -> Result<Vec<MemoryFile>> {
        self.list_in_subdir("users", |stem| FileKind::User {
            user_id: stem.into(),
        })
        .await
    }

    pub async fn list_state(&self) -> Result<Vec<MemoryFile>> {
        self.list_in_subdir("state", |stem| FileKind::State { slug: stem.into() })
            .await
    }

    async fn list_in_subdir(
        &self,
        sub: &str,
        kind_for: impl Fn(&str) -> FileKind,
    ) -> Result<Vec<MemoryFile>> {
        let dir = self.inner.memories_dir.join(sub);
        let mut out = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .wrap_err_with(|| format!("read_dir {}", dir.display()))?;
        while let Some(e) = entries.next_entry().await.wrap_err("next_entry")? {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let kind = kind_for(&stem);
            out.push(self.read_kind(&kind).await?);
        }
        out.sort_by_key(|f| std::cmp::Reverse(f.frontmatter.updated_at));
        Ok(out)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("file_full")]
    Full,
    #[error("state_full")]
    StateFull,
    #[error("invalid_slug")]
    InvalidSlug,
    #[error("io: {0}")]
    Io(#[from] eyre::Report),
}

/// Count `*.md` entries in `memories/state/`. Used by `write_state` under
/// the state-create lock to enforce `max_state_files`.
async fn count_state_files(memories_dir: &Path) -> Result<usize, WriteError> {
    let dir = memories_dir.join("state");
    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| WriteError::Io(eyre!("read_dir state/: {e}")))?;
    let mut n = 0usize;
    while let Some(e) = entries
        .next_entry()
        .await
        .map_err(|e| WriteError::Io(eyre!("read_dir next: {e}")))?
    {
        if e.path().extension().and_then(|s| s.to_str()) == Some("md") {
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arc_swap::ArcSwap;

    use super::*;
    use crate::ai::memory::types::Caps;
    use crate::settings::{Settings, SettingsHandle};

    fn test_handle() -> SettingsHandle {
        Arc::new(ArcSwap::from_pointee(Settings::compiled_defaults()))
    }

    fn handle_with_caps(caps: Caps) -> SettingsHandle {
        let mut s = Settings::compiled_defaults();
        s.ai.memory.soul_bytes = caps.soul_bytes;
        s.ai.memory.lore_bytes = caps.lore_bytes;
        s.ai.memory.user_bytes = caps.user_bytes;
        s.ai.memory.state_bytes = caps.state_bytes;
        s.ai.memory.max_state_files = caps.max_state_files;
        Arc::new(ArcSwap::from_pointee(s))
    }

    #[tokio::test]
    async fn open_creates_tree_and_seeds_soul() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        assert!(dir.path().join("memories/SOUL.md").exists());
        assert!(dir.path().join("memories/users").is_dir());
        assert!(dir.path().join("memories/state").is_dir());
        assert!(dir.path().join("memories/transcripts").is_dir());

        let soul = store.read_kind(&FileKind::Soul).await.unwrap();
        assert!(soul.body.contains("Aurora"));
    }

    #[tokio::test]
    async fn open_renames_v1_store_when_present() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("ai_memory.ron"), b"v1 garbage")
            .await
            .unwrap();
        MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        let mut found = false;
        while let Some(e) = entries.next_entry().await.unwrap() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("ai_memory.ron.discarded-") {
                found = true;
            }
        }
        assert!(found, "expected discarded v1 store");
    }

    #[tokio::test]
    async fn open_does_not_seed_prompts_to_disk() {
        // Prompts are code now (#321): baked into the binary via `include_str!`
        // and used directly at runtime. `open` must not create or seed a
        // `$DATA_DIR/prompts/` dir — there is no on-disk override to drift.
        let dir = tempfile::tempdir().unwrap();
        MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        assert!(
            !dir.path().join("prompts").exists(),
            "open must not create a prompts dir"
        );
    }

    #[tokio::test]
    async fn write_user_file_persists_and_caps_apply() {
        let dir = tempfile::tempdir().unwrap();
        // Cap covers the *full* on-disk file (frontmatter + body), so it must
        // be larger than the ~80B frontmatter overhead but small enough to
        // still reject a 4 KiB body.
        let store = MemoryStore::open(
            dir.path(),
            handle_with_caps(Caps {
                user_bytes: 256,
                ..Caps::default()
            }),
        )
        .await
        .unwrap();
        let kind = FileKind::User {
            user_id: "12".into(),
        };

        store
            .write(&kind, "small body", Some("alicepleb"), Some("AlicePleb"))
            .await
            .unwrap();
        let mf = store.read_kind(&kind).await.unwrap();
        assert_eq!(mf.body.trim(), "small body");
        assert_eq!(mf.frontmatter.username.as_deref(), Some("alicepleb"));
        assert_eq!(mf.frontmatter.display_name.as_deref(), Some("AlicePleb"));

        let huge = "x".repeat(4096);
        let err = store
            .write(&kind, &huge, Some("alicepleb"), Some("AlicePleb"))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "file_full");
    }

    #[tokio::test]
    async fn write_state_full_enforced_in_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(
            dir.path(),
            handle_with_caps(Caps {
                max_state_files: 1,
                ..Caps::default()
            }),
        )
        .await
        .unwrap();
        store
            .write_state(
                &FileKind::State {
                    slug: "first".into(),
                },
                "x",
                Some("1"),
            )
            .await
            .unwrap();
        // Overwriting the same slug must still succeed at the cap.
        store
            .write_state(
                &FileKind::State {
                    slug: "first".into(),
                },
                "y",
                Some("1"),
            )
            .await
            .unwrap();
        // A second distinct slug must fail.
        let err = store
            .write_state(
                &FileKind::State {
                    slug: "second".into(),
                },
                "x",
                Some("1"),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, WriteError::StateFull));
    }

    #[tokio::test]
    async fn display_name_is_normalised_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let kind = FileKind::User {
            user_id: "99".into(),
        };
        let dirty = "ali\u{200B}ce\nx";
        store
            .write(&kind, "body", Some("alicex"), Some(dirty))
            .await
            .unwrap();
        let mf = store.read_kind(&kind).await.unwrap();
        assert_eq!(mf.frontmatter.display_name.as_deref(), Some("alicex"));
    }

    #[tokio::test]
    async fn list_users_and_states_orders_by_updated_at_desc() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let a = FileKind::User {
            user_id: "1".into(),
        };
        let b = FileKind::User {
            user_id: "2".into(),
        };
        store
            .write(&a, "first", Some("a"), Some("A"))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        store
            .write(&b, "second", Some("b"), Some("B"))
            .await
            .unwrap();
        let list = store.list_users().await.unwrap();
        assert_eq!(list[0].kind, b);
        assert_eq!(list[1].kind, a);
    }

    #[tokio::test]
    async fn write_state_sets_created_by_on_create_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let kind = FileKind::State {
            slug: "quiz".into(),
        };
        store
            .write_state(&kind, "score: 1", Some("12345"))
            .await
            .unwrap();
        let one = store.read_kind(&kind).await.unwrap();
        assert_eq!(one.frontmatter.created_by.as_deref(), Some("12345"));
        store
            .write_state(&kind, "score: 2", Some("99999"))
            .await
            .unwrap();
        let two = store.read_kind(&kind).await.unwrap();
        assert_eq!(
            two.frontmatter.created_by.as_deref(),
            Some("12345"),
            "created_by stays after overwrite"
        );
    }

    #[tokio::test]
    async fn current_mtime_returns_positive_for_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        // SOUL.md is seeded by `open`, so it must have a non-zero mtime.
        let mt = store.current_mtime(&FileKind::Soul).await.unwrap();
        assert!(mt > 0, "seeded file must have positive mtime; got {mt}");
    }

    #[tokio::test]
    async fn current_mtime_returns_zero_for_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let mt = store
            .current_mtime(&FileKind::User {
                user_id: "404".into(),
            })
            .await
            .unwrap();
        assert_eq!(mt, 0, "missing file must report mtime 0");
        let mt = store
            .current_mtime(&FileKind::State {
                slug: "missing".into(),
            })
            .await
            .unwrap();
        assert_eq!(mt, 0);
    }

    #[tokio::test]
    async fn delete_state_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let kind = FileKind::State {
            slug: "quiz".into(),
        };
        store.write_state(&kind, "x", Some("1")).await.unwrap();
        store.delete_state("quiz").await.unwrap();
        assert!(!dir.path().join("memories/state/quiz.md").exists());
    }

    #[tokio::test]
    async fn write_with_guard_detects_concurrent_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        // Bump SOUL.md so we have a known mtime to test against.
        store
            .write(&FileKind::Soul, "first", None, None)
            .await
            .unwrap();
        let mt1 = store.current_mtime(&FileKind::Soul).await.unwrap();

        // Stale token → Conflict.
        let outcome = store
            .write_with_guard(
                FileKind::Soul,
                "",
                "loser",
                Some(0),
                FrontmatterOverride::default(),
            )
            .await
            .unwrap();
        match outcome {
            WriteOutcome::Conflict {
                current_body,
                current_mtime,
            } => {
                assert!(current_body.contains("first"));
                assert_eq!(current_mtime, mt1);
            }
            WriteOutcome::Written { .. } => panic!("expected Conflict for stale mtime"),
        }
        // The file must NOT have been overwritten.
        let mf = store.read_kind(&FileKind::Soul).await.unwrap();
        assert!(mf.body.contains("first"));

        // Fresh token → Written.
        let outcome = store
            .write_with_guard(
                FileKind::Soul,
                "",
                "winner",
                Some(mt1),
                FrontmatterOverride::default(),
            )
            .await
            .unwrap();
        let new_mt = match outcome {
            WriteOutcome::Written { new_mtime } => new_mtime,
            WriteOutcome::Conflict { .. } => panic!("expected Written for fresh mtime"),
        };
        assert!(new_mt >= mt1);
        let mf = store.read_kind(&FileKind::Soul).await.unwrap();
        assert!(mf.body.contains("winner"));
    }

    #[tokio::test]
    async fn write_with_guard_unconditional_when_expected_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        store
            .write(&FileKind::Lore, "before", None, None)
            .await
            .unwrap();
        let outcome = store
            .write_with_guard(
                FileKind::Lore,
                "",
                "after",
                None,
                FrontmatterOverride::default(),
            )
            .await
            .unwrap();
        assert!(matches!(outcome, WriteOutcome::Written { .. }));
        let mf = store.read_kind(&FileKind::Lore).await.unwrap();
        assert!(mf.body.contains("after"));
    }

    #[tokio::test]
    async fn write_with_guard_override_replaces_user_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let kind = FileKind::User {
            user_id: "42".into(),
        };
        store
            .write(&kind, "body", Some("oldlogin"), Some("OldDisplay"))
            .await
            .unwrap();
        let mt = store.current_mtime(&kind).await.unwrap();
        let fm = FrontmatterOverride {
            username: Some("newlogin".into()),
            display_name: Some("NewDisplay".into()),
            created_by: None,
        };
        let outcome = store
            .write_with_guard(kind.clone(), "42", "body2", Some(mt), fm)
            .await
            .unwrap();
        assert!(matches!(outcome, WriteOutcome::Written { .. }));
        let mf = store.read_kind(&kind).await.unwrap();
        assert_eq!(mf.frontmatter.username.as_deref(), Some("newlogin"));
        assert_eq!(mf.frontmatter.display_name.as_deref(), Some("NewDisplay"));
    }

    #[tokio::test]
    async fn write_with_guard_override_replaces_state_created_by() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let kind = FileKind::State {
            slug: "quiz".into(),
        };
        store.write_state(&kind, "x", Some("12345")).await.unwrap();
        let mt = store.current_mtime(&kind).await.unwrap();
        let fm = FrontmatterOverride {
            created_by: Some("99999".into()),
            ..FrontmatterOverride::default()
        };
        store
            .write_with_guard(kind.clone(), "quiz", "y", Some(mt), fm)
            .await
            .unwrap();
        let mf = store.read_kind(&kind).await.unwrap();
        assert_eq!(mf.frontmatter.created_by.as_deref(), Some("99999"));
    }

    #[tokio::test]
    async fn write_with_guard_empty_override_preserves_state_created_by() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let kind = FileKind::State {
            slug: "notes".into(),
        };
        store.write_state(&kind, "x", Some("12345")).await.unwrap();
        let mt = store.current_mtime(&kind).await.unwrap();
        let fm = FrontmatterOverride {
            created_by: Some(String::new()),
            ..FrontmatterOverride::default()
        };
        store
            .write_with_guard(kind.clone(), "notes", "y", Some(mt), fm)
            .await
            .unwrap();
        let mf = store.read_kind(&kind).await.unwrap();
        assert_eq!(mf.frontmatter.created_by.as_deref(), Some("12345"));
    }

    #[tokio::test]
    async fn state_reserved_slugs_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        for reserved in ["new", "delete"] {
            let err = store
                .write_state(
                    &FileKind::State {
                        slug: reserved.into(),
                    },
                    "x",
                    Some("1"),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(err, WriteError::InvalidSlug),
                "slug `{reserved}` must be rejected, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn state_invalid_slug_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        let too_long = "x".repeat(65);
        let cases = [too_long.as_str(), "", "with/slash", "..", "with..dots"];
        for slug in cases {
            let err = store
                .write_state(&FileKind::State { slug: slug.into() }, "x", Some("1"))
                .await
                .unwrap_err();
            assert!(
                matches!(err, WriteError::InvalidSlug),
                "slug `{slug}` must be rejected, got {err:?}"
            );
        }
    }

    /// Verify that `MemoryStore::caps()` reflects settings changes published
    /// after construction — i.e., the store holds a live handle, not a snapshot.
    #[tokio::test]
    async fn memory_store_caps_rebind_live() {
        let dir = tempfile::tempdir().unwrap();
        let handle = test_handle();
        let store = MemoryStore::open(dir.path(), handle.clone()).await.unwrap();
        // Compiled default is 4096 bytes.
        assert_eq!(store.caps().soul_bytes, 4096);

        let mut new_settings = Settings::compiled_defaults();
        new_settings.ai.memory.soul_bytes = 8192;
        handle.store(Arc::new(new_settings));

        assert_eq!(
            store.caps().soul_bytes,
            8192,
            "store must reflect live settings update"
        );
    }
}
