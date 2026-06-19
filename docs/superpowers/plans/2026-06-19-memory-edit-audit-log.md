# Memory Edit Audit Log Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Append a durable JSONL audit line per dashboard memory mutation (edit/create/delete) to `$DATA_DIR/memory_audit.log`.

**Architecture:** Emit in the web route (`crates/web/src/routes/memory.rs`), beside the existing `tracing` logs — `MemoryStore` stays a pure FS layer. Reuse the settings `FileAuditLog` writer via a new generic `append_serializable` method. A new `memory_audit: Arc<FileAuditLog>` on `WebState` is constructed at each `WebState` build site and points at a single append-only file (no rotation). Timestamps use the existing web stub clock through `berlin_now`.

**Tech Stack:** Rust, axum, `serde`/`serde_json`, `chrono` + `chrono-tz`, `tempfile` (tests), `cargo nextest`.

Full design: `docs/superpowers/specs/2026-06-19-memory-edit-audit-log-design.md`.

## Global Constraints

- Never commit to `main`. Work is on branch `feature/memory-edit-audit-log` (already created).
- Pre-commit gate, in order: `cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → tests. Clippy is `-D warnings`; do not `#[allow]` without a one-line reason comment.
- Run tests with: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`.
- Commit messages: Conventional Commits, unhinged genz subject line, sober body only if large. End commit body with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- No new dependencies (all of `serde`, `serde_json`, `chrono`, `chrono-tz` already present in both crates). If any `Cargo.toml` changes, stage `Cargo.lock` in the same commit.
- All time ops use `Europe/Berlin`: `ts = berlin_now(state.clock.now())`.
- Audit append is best-effort: on `Err`, `tracing::error!` and continue — never fail the dashboard save.
- Log errors with `?error` (backtrace), not `%error`.

## File Structure

- `crates/core/src/settings/audit.rs` — **modify.** Add inherent `FileAuditLog::append_serializable<S: Serialize>`; the existing `AuditLog::append` delegates to it. (Task 1)
- `crates/web/src/state.rs` — **modify.** Add `pub memory_audit: Arc<FileAuditLog>` field. (Task 2)
- `crates/twitch-1337/src/main.rs` — **modify.** Construct `memory_audit` and set it in the `WebState { … }` literal. (Task 2)
- `crates/web/src/bin/web_dev.rs` — **modify.** Same construction. (Task 2)
- `crates/web/tests/helpers/mod.rs` — **modify.** Construct `memory_audit` from the memory tempdir in `build_state_inner_keep_settings`. (Task 2)
- `crates/web/src/routes/memory.rs` — **modify.** Add `MemoryAuditEntry`, `kind_tag`, `audit_memory`; emit at the three mutation sites. (Task 3)
- `crates/web/tests/memory_write.rs` — **modify.** Integration tests asserting audit lines. (Task 3)

---

### Task 1: Generic `append_serializable` on `FileAuditLog`

Make the existing append-only JSONL writer reusable for a non-settings entry type without touching the `AuditLog` trait or the `Arc<dyn AuditLog>` consumers in the settings store.

**Files:**
- Modify: `crates/core/src/settings/audit.rs:50-62` (the `impl AuditLog for FileAuditLog`)
- Test: `crates/core/src/settings/audit.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `FileAuditLog::append_serializable<S: serde::Serialize>(&self, entry: &S) -> Result<(), AuditError>` — appends one JSON line + `\n`, `create(true).append(true)`, `sync_all()`. Used by Task 3.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/core/src/settings/audit.rs`:

```rust
#[test]
fn append_serializable_writes_one_json_line_per_call() {
    #[derive(serde::Serialize)]
    struct Tiny {
        a: u32,
        b: &'static str,
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("memory_audit.log");
    let log = FileAuditLog::new(&path);
    log.append_serializable(&Tiny { a: 1, b: "hi" })
        .expect("first");
    log.append_serializable(&Tiny { a: 2, b: "yo" })
        .expect("second");
    let body = std::fs::read_to_string(&path).expect("read");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2);
    let v: serde_json::Value = serde_json::from_str(lines[0]).expect("valid json");
    assert_eq!(v["a"], 1);
    assert_eq!(v["b"], "hi");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p twitch-1337-core --show-progress=none --cargo-quiet append_serializable_writes_one_json_line_per_call`
Expected: compile error — `no method named append_serializable found for struct FileAuditLog`.

- [ ] **Step 3: Add the generic method and delegate the trait impl**

Replace the existing `impl AuditLog for FileAuditLog { … }` block (currently `audit.rs:50-62`) with:

```rust
impl FileAuditLog {
    /// Append any serializable value as one JSON line. Shared by the settings
    /// `AuditEntry` trait impl below and the web memory-audit entry, so both
    /// audit logs use the identical create+append+sync writer.
    pub fn append_serializable<S: serde::Serialize>(&self, entry: &S) -> Result<(), AuditError> {
        use std::io::Write as _;
        let line = serde_json::to_string(entry)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        f.sync_all()?;
        Ok(())
    }
}

impl AuditLog for FileAuditLog {
    fn append(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        self.append_serializable(entry)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p twitch-1337-core --show-progress=none --cargo-quiet settings::audit`
Expected: PASS — the new test plus the existing `file_log_appends_one_json_line_per_call`, `file_log_survives_truncation_between_writes`, `memory_log_records_entries`.

- [ ] **Step 5: Gate + commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
git add crates/core/src/settings/audit.rs
git commit -m "refactor(settings): FileAuditLog learns to write any serde shape, not just its own diary 📓"
```

---

### Task 2: `memory_audit` field on `WebState` + construction wiring

Thread an `Arc<FileAuditLog>` for memory writes through every `WebState` build site. Pure plumbing — the field is unused until Task 3, so the deliverable's gate is "workspace builds and all existing tests still pass."

**Files:**
- Modify: `crates/web/src/state.rs:17-30` (imports) and `:55` (after `memory_store` field)
- Modify: `crates/twitch-1337/src/main.rs:419` (`WebState { … }` literal) + construction above it
- Modify: `crates/web/src/bin/web_dev.rs:104` (`WebState { … }` literal) + construction above it
- Modify: `crates/web/tests/helpers/mod.rs:223-257` (construction + literal)

**Interfaces:**
- Consumes: `FileAuditLog::new` (existing), `MemoryStore` data dir.
- Produces: `WebState.memory_audit: Arc<FileAuditLog>` — read by `audit_memory` in Task 3. Audit file path convention: `<memory store data dir>/memory_audit.log` (sibling of `memories/`), matching `settings_audit.log`.

- [ ] **Step 1: Add the field + import to `WebState`**

In `crates/web/src/state.rs`, add to the imports near line 17:

```rust
use twitch_1337_core::settings::FileAuditLog;
```

Add the field to `pub struct WebState` immediately after the `pub memory_store: MemoryStore,` field (line 55):

```rust
    /// Append-only audit log for dashboard memory mutations
    /// (`$DATA_DIR/memory_audit.log`). Sibling of the settings audit log;
    /// written best-effort by the `/memory/*` write/create/delete routes.
    pub memory_audit: Arc<FileAuditLog>,
```

- [ ] **Step 2: Construct it in `main.rs`**

In `crates/twitch-1337/src/main.rs`, just above the `let state = twitch_1337_web::WebState {` literal (line 419), add:

```rust
    let memory_audit = Arc::new(twitch_1337_web::settings::FileAuditLog::new(
        get_data_dir().join("memory_audit.log"),
    ));
```

Add `memory_audit,` to the `WebState { … }` literal (next to `memory_store,`).

> Note: `main.rs` already references `twitch_1337::settings::FileAuditLog` for the settings audit; `twitch_1337_web::settings` re-exports the same core type. If the existing `settings_audit.log` line uses `twitch_1337::settings::FileAuditLog`, use that exact path here too for consistency — both resolve to `twitch_1337_core::settings::FileAuditLog`.

- [ ] **Step 3: Construct it in `web_dev.rs`**

In `crates/web/src/bin/web_dev.rs`, just above the `let state = WebState {` literal (line 104), add:

```rust
    let memory_audit = Arc::new(twitch_1337_core::settings::FileAuditLog::new(
        data_dir.join("memory_audit.log"),
    ));
```

Add `memory_audit,` to the `WebState { … }` literal (next to `memory_store,`).

- [ ] **Step 4: Construct it in the test helper**

In `crates/web/tests/helpers/mod.rs`, inside `build_state_inner_keep_settings`, immediately after the `let memory_store = MemoryStore::open(memory_dir.path(), …)` block (line ~223–225), add:

```rust
    let memory_audit = Arc::new(twitch_1337_core::settings::FileAuditLog::new(
        memory_dir.path().join("memory_audit.log"),
    ));
```

Add `memory_audit,` to the `WebState { … }` literal (next to `memory_store,`, line ~240).

- [ ] **Step 5: Build + run the full suite to verify no regression**

Run: `cargo build --workspace`
Expected: builds clean (all four `WebState` literals now set the field).

Run: `cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail`
Expected: PASS — no behavior changed; this confirms the new field didn't break any construction site.

- [ ] **Step 6: Gate + commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
git add crates/web/src/state.rs crates/twitch-1337/src/main.rs crates/web/src/bin/web_dev.rs crates/web/tests/helpers/mod.rs
git commit -m "feat(web): WebState now carries a memory_audit log, idle until the routes start gossiping 🗃️"
```

---

### Task 3: Emit audit lines at the three mutation sites + integration tests

Define the entry type and emit helper, then call it from `save_kind` (all three outcomes), `create_state` (both arms), and `delete_state` (success). Define-and-use in one diff so there's no dead-code lint.

**Files:**
- Modify: `crates/web/src/routes/memory.rs` — imports (`:20-23`), new items (near the other `struct`/`fn` helpers), three call-site edits in `save_kind` (`:691-741`), `create_state` (`:916-937`), `delete_state` (`:1011-1025`)
- Test: `crates/web/tests/memory_write.rs`

**Interfaces:**
- Consumes: `WebState.memory_audit` (Task 2), `FileAuditLog::append_serializable` (Task 1), `berlin_now` from `twitch_1337_core::settings::audit`, `Mtime` from `twitch_1337_core::ai::memory::store`.
- Produces: JSONL lines in `$DATA_DIR/memory_audit.log` with fields `ts, actor_id, actor_login, op, kind, id, result` plus optional `mtime_before, mtime_after, bytes_after`.

- [ ] **Step 1: Write the first failing integration test**

In `crates/web/tests/memory_write.rs`, add a reader helper and the first test. Bind `td_memory` (existing tests discard it as `_tdm`):

```rust
fn read_audit_lines(td_memory: &TempDir) -> Vec<serde_json::Value> {
    let path = td_memory.path().join("memory_audit.log");
    let body = std::fs::read_to_string(&path).expect("memory_audit.log exists");
    body.lines()
        .map(|l| serde_json::from_str(l).expect("audit line is valid json"))
        .collect()
}

#[tokio::test]
async fn save_writes_audit_line() {
    let (state, sid, csrf, bare_csrf, _tdp, td_memory) = authed_setup().await;
    let mtime = state
        .memory_store
        .current_mtime(&FileKind::Soul)
        .await
        .unwrap();
    let body = format!(
        "_csrf={csrf}&mtime={mtime}&body=audited-soul",
        csrf = urlencoding::encode(&bare_csrf),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/soul", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let lines = read_audit_lines(&td_memory);
    assert_eq!(lines.len(), 1);
    let e = &lines[0];
    assert_eq!(e["op"], "write");
    assert_eq!(e["kind"], "soul");
    assert_eq!(e["result"], "ok");
    assert_eq!(e["actor_id"], "9001");
    assert_eq!(e["actor_login"], "admin");
    assert!(e["ts"].as_str().is_some(), "ts must serialize as a string");
    assert_eq!(e["bytes_after"], "audited-soul".len());
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p twitch-1337-web --show-progress=none --cargo-quiet save_writes_audit_line`
Expected: FAIL — `memory_audit.log exists` panics (no emit yet, file never created).

- [ ] **Step 3: Add imports, the entry type, and the helpers**

In `crates/web/src/routes/memory.rs`, update the two affected `use` lines:

```rust
use serde::{Deserialize, Serialize};
use twitch_1337_core::ai::memory::store::{
    FrontmatterOverride, Mtime, WriteOutcome, validate_state_slug,
};
```

Add `use twitch_1337_core::settings::audit::berlin_now;` to the project-imports block.

Add the entry type and helpers near the other module-private helpers (e.g. just below the `SaveForm`/`CreateStateForm` structs):

```rust
/// One JSONL line per dashboard memory mutation, appended to
/// `$DATA_DIR/memory_audit.log`. Cheap fields only — see
/// docs/superpowers/specs/2026-06-19-memory-edit-audit-log-design.md.
#[derive(Serialize)]
struct MemoryAuditEntry {
    ts: chrono::DateTime<chrono_tz::Tz>,
    actor_id: String,
    actor_login: String,
    op: &'static str,   // "write" | "create" | "delete"
    kind: &'static str, // "soul" | "lore" | "user" | "state"
    id: String,         // slug or user_id; empty for soul/lore
    result: String,     // "ok" | "conflict" | "error:<variant>"
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime_before: Option<Mtime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime_after: Option<Mtime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_after: Option<usize>,
}

fn kind_tag(kind: &FileKind) -> &'static str {
    match kind {
        FileKind::Soul => "soul",
        FileKind::Lore => "lore",
        FileKind::User { .. } => "user",
        FileKind::State { .. } => "state",
    }
}

/// Best-effort append of one memory-audit line. A write failure is logged and
/// swallowed — the dashboard save already succeeded and must not be undone by
/// an audit hiccup (mirrors `SettingsStore::commit`).
#[allow(clippy::too_many_arguments)]
// 9 args is past clippy's 7; this is a single flat record builder, threading a
// struct here would just move the field list one call deeper.
fn audit_memory(
    state: &WebState,
    session: &Session,
    op: &'static str,
    kind: &'static str,
    id: &str,
    result: String,
    mtime_before: Option<Mtime>,
    mtime_after: Option<Mtime>,
    bytes_after: Option<usize>,
) {
    let entry = MemoryAuditEntry {
        ts: berlin_now(state.clock.now()),
        actor_id: session.user_id.clone(),
        actor_login: session.user_login.clone(),
        op,
        kind,
        id: id.to_owned(),
        result,
        mtime_before,
        mtime_after,
        bytes_after,
    };
    if let Err(error) = state.memory_audit.append_serializable(&entry) {
        tracing::error!(
            target: "twitch_1337_web",
            ?error,
            "memory audit append failed"
        );
    }
}
```

- [ ] **Step 4: Emit in `save_kind`'s three outcome arms**

In `save_kind`, change the `Written` arm to bind `new_mtime` and emit; add emits to the `Conflict` and `Err` arms. The arms become:

```rust
        Ok(WriteOutcome::Written { new_mtime }) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_write",
                target_label = %label,
                target_id = %id,
                result = "ok",
            );
            audit_memory(
                state,
                session,
                "write",
                kind_tag(&kind),
                &id,
                "ok".to_owned(),
                Some(form.mtime),
                Some(new_mtime),
                Some(form.body.len()),
            );
            flash::set(cookies, &format!("{label} saved"));
            Ok(Redirect::to(&redirect_to).into_response())
        }
        Ok(WriteOutcome::Conflict {
            current_body,
            current_mtime,
        }) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_write",
                target_label = %label,
                target_id = %id,
                result = "conflict",
            );
            audit_memory(
                state,
                session,
                "write",
                kind_tag(&kind),
                &id,
                "conflict".to_owned(),
                Some(form.mtime),
                Some(current_mtime),
                None,
            );
            Err(WebError::Conflict(Box::new(ConflictPayload {
                kind: label,
                id,
                current_body,
                current_mtime_display: fmt_mtime_ms(current_mtime),
                current_mtime,
                draft: form.body,
                csrf: csrf_hex,
                user_login: session.user_login.clone(),
                user_avatar_url: session.avatar_url.clone(),
                is_mod: session.is_mod(),
                is_broadcaster: session.is_broadcaster,
                is_owner: matches!(session.role, crate::auth::Role::Owner),
                current_page,
                cancel_url,
            })))
        }
        Err(err) => {
            tracing::warn!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_write",
                target_label = %label,
                target_id = %id,
                result = "error",
                error = ?err,
            );
            audit_memory(
                state,
                session,
                "write",
                kind_tag(&kind),
                &id,
                format!("error:{err}"),
                Some(form.mtime),
                None,
                None,
            );
            // Render the editor with the user's draft preserved. `Io` is the
            // only variant that lacks a meaningful form context — bubble it.
            let msg = write_error_for_form(err)?;
```

(Leave the rest of the `Err` arm body unchanged.)

- [ ] **Step 5: Emit in `create_state`'s two arms**

In `create_state`, add an emit to each arm of the `write_state` match (`state`/`session` are owned here, so pass by ref):

```rust
        Ok(()) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_create",
                target_label = "state",
                target_id = %slug,
                result = "ok",
            );
            audit_memory(
                &state,
                &session,
                "create",
                "state",
                &slug,
                "ok".to_owned(),
                None,
                None,
                Some(form.body.len()),
            );
            flash::set(&cookies, &format!("state `{slug}` created"));
            Ok(Redirect::to(&format!("/memory/state/{slug}")).into_response())
        }
        Err(err) => {
            tracing::warn!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "memory_create",
                target_label = "state",
                target_id = %slug,
                result = "error",
                error = ?err,
            );
            audit_memory(
                &state,
                &session,
                "create",
                "state",
                &slug,
                format!("error:{err}"),
                None,
                None,
                None,
            );
            let msg = write_error_for_form(err)?;
```

(Leave the rest of the `Err` arm body unchanged.)

- [ ] **Step 6: Emit in `delete_state` after success**

In `delete_state`, add the emit between the `tracing::info!` and `flash::set`:

```rust
    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "memory_delete",
        target_label = "state",
        target_id = %slug,
        result = "ok",
    );
    audit_memory(
        &state,
        &session,
        "delete",
        "state",
        &slug,
        "ok".to_owned(),
        None,
        None,
        None,
    );
    flash::set(&cookies, &format!("state `{slug}` deleted"));
```

- [ ] **Step 7: Run the first test to verify it passes**

Run: `cargo nextest run -p twitch-1337-web --show-progress=none --cargo-quiet save_writes_audit_line`
Expected: PASS.

- [ ] **Step 8: Add the conflict / create / delete tests**

Append to `crates/web/tests/memory_write.rs`:

```rust
#[tokio::test]
async fn conflict_writes_audit_line() {
    let (state, sid, csrf, bare_csrf, _tdp, td_memory) = authed_setup().await;
    state
        .memory_store
        .write(&FileKind::Soul, "current-on-disk", None, None)
        .await
        .unwrap();
    let body = format!(
        "_csrf={csrf}&mtime=0&body=loser-draft",
        csrf = urlencoding::encode(&bare_csrf),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/soul", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    let lines = read_audit_lines(&td_memory);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["op"], "write");
    assert_eq!(lines[0]["result"], "conflict");
    assert!(
        lines[0].get("bytes_after").is_none(),
        "conflict persisted nothing, so bytes_after must be omitted"
    );
}

#[tokio::test]
async fn create_writes_audit_line() {
    let (state, sid, csrf, bare_csrf, _tdp, td_memory) = authed_setup().await;
    let body = format!(
        "_csrf={csrf}&slug=quiz&body=score: 1",
        csrf = urlencoding::encode(&bare_csrf),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/state", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let lines = read_audit_lines(&td_memory);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["op"], "create");
    assert_eq!(lines[0]["kind"], "state");
    assert_eq!(lines[0]["id"], "quiz");
    assert_eq!(lines[0]["result"], "ok");
}

#[tokio::test]
async fn delete_writes_audit_line() {
    let (state, sid, csrf, bare_csrf, _tdp, td_memory) = authed_setup().await;
    // Create then delete; the audit log should hold one create + one delete.
    state
        .memory_store
        .write_state(&FileKind::State { slug: "doomed".into() }, "x", Some("9001"))
        .await
        .unwrap();
    let body = format!(
        "_csrf={csrf}",
        csrf = urlencoding::encode(&bare_csrf),
    );
    let app = build_router(state.clone());
    let res = app
        .oneshot(post_form("/memory/state/doomed/delete", &sid, &csrf, body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let lines = read_audit_lines(&td_memory);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["op"], "delete");
    assert_eq!(lines[0]["kind"], "state");
    assert_eq!(lines[0]["id"], "doomed");
    assert_eq!(lines[0]["result"], "ok");
}
```

> Note: `delete_writes_audit_line` seeds the file directly via `write_state` (not the create route) so only the delete produces an audit line — keeping the assertion at exactly one line.

- [ ] **Step 9: Run the memory_write suite to verify all pass**

Run: `cargo nextest run -p twitch-1337-web --show-progress=none --cargo-quiet memory_write`
Expected: PASS — the four new audit tests plus all pre-existing `memory_write` tests.

- [ ] **Step 10: Full gate + commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
git add crates/web/src/routes/memory.rs crates/web/tests/memory_write.rs
git commit -m "feat(web): dashboard memory edits now leave a paper trail instead of vanishing into the log void 🕵️📝

Append one JSONL line per memory write/create/delete to
\$DATA_DIR/memory_audit.log so mods can answer who-changed-what-when
without scraping ephemeral tracing. Closes #161.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage:**
- Emission in web route → Task 3 (all three sites). ✓
- Single append-only `$DATA_DIR/memory_audit.log`, reuse `FileAuditLog` → Task 1 (`append_serializable`) + Task 2 (construction). ✓
- Scope edit + create + delete → Task 3 (save_kind, create_state, delete_state). ✓
- Field set (cheap only, `bytes_before`/`sha256` dropped) → `MemoryAuditEntry` in Task 3 Step 3. ✓
- `ts = berlin_now(state.clock.now())` → Task 3 `audit_memory`. ✓
- Best-effort failure handling → `audit_memory` logs `?error` and continues. ✓
- `op` field; `conflict` only on write; create/delete always `kind=state` → enforced by literal call args. ✓
- Tests: serde round-trip + one line per outcome incl. conflict → Task 1 unit test + Task 3 integration tests. ✓
- Out of scope (rotation, byte cap, UTC, sha256, AI/dreamer writes, delete-error auditing) → not implemented, matching spec. ✓

**2. Placeholder scan:** No TBD/TODO/"handle edge cases"/"similar to" — every code step carries full code. ✓

**3. Type consistency:** `append_serializable<S: Serialize>` (Task 1) called on `Arc<FileAuditLog>` (Task 2 field) by `audit_memory` (Task 3). `Mtime` = `u64` from `store`. `kind_tag(&FileKind) -> &'static str` matches `MemoryAuditEntry.kind: &'static str`. `audit_memory` signature matches all six call sites (`state`/`session` passed by ref; owned at create/delete via `&state`/`&session`, already-ref at `save_kind`). ✓
