# Memory edit audit log

Date: 2026-06-19
Branch: `feature/memory-edit-audit-log`
Issue: #161

## Context

Dashboard memory mutations currently leave only a `tracing::info!`/`warn!`
breadcrumb (`target: twitch_1337_web`, `action = memory_write|memory_create|
memory_delete`). Tracing is ephemeral — it rotates out of the journal/podman
buffer — so there is no durable answer to "who changed this state note, and
when", "did the dreamer race a dashboard edit", or "what did the dashboard
change yesterday".

This adds a durable append-only audit line per dashboard mutation, modelled on
the existing settings audit log (`crates/core/src/settings/audit.rs` →
`$DATA_DIR/settings_audit.log`).

Key terrain fact: `MemoryStore::write_with_guard` is called **only** by the web
route. The AI `write_file`/`write_state` tools and the dreamer ritual write
through `store.write()` / `store.write_state()` directly, bypassing the guard.
So auditing the dashboard path captures exactly moderator edits — matching the
"the moderator who saved" framing — and the `write_with_guard` docstring
claiming the dreamer/AI use it is stale. The audit is therefore a **web
concern**, emitted in the route, leaving `MemoryStore` a pure FS layer.

### Decisions (resolved during grilling)

- **Emission site:** in the web route (`crates/web/src/routes/memory.rs`), next
  to the existing tracing logs — *not* inside `write_with_guard`. Diverges from
  the issue's literal "`write_with_guard` emits" wording in favor of keeping the
  store free of actor identity / audit deps.
- **Storage:** single append-only `$DATA_DIR/memory_audit.log`, no rotation, no
  byte cap — a sibling of `settings_audit.log`, reusing `FileAuditLog`. Drops
  the issue's daily-rotation / 1 MiB-cap / faked-clock-rotation acceptance:
  premature for this volume (mods editing by hand; dreamer/AI bypass this path).
- **Scope:** all three dashboard mutations — edit, create, delete.
- **Fields:** cheap fields only (see schema). Drops `bytes_before` and
  `body_sha256_after` (both need an extra disk read, serve none of the three
  scenarios).
- **Timestamp:** `berlin_now(state.clock.now())`, matching `settings_audit.log`
  and the project-wide Europe/Berlin invariant (overrides the issue's "UTC").
- **Failure handling:** best-effort — append error `error!`-and-continue, the
  save still succeeds. Mirrors `SettingsStore::commit`.

## Entry schema

`MemoryAuditEntry`, one JSONL line per mutation outcome:

| field | type | source / notes |
|---|---|---|
| `ts` | `DateTime<chrono_tz::Tz>` | `berlin_now(state.clock.now())`; RFC3339 Berlin offset |
| `actor_id` | `String` | `session.user_id` |
| `actor_login` | `String` | `session.user_login` |
| `op` | `&str` | `write` \| `create` \| `delete` |
| `kind` | `&str` | `soul` \| `lore` \| `user` \| `state`; `create`/`delete` always `state` |
| `id` | `String` | slug or user_id; empty for soul/lore (mirrors existing `target_id`) |
| `result` | `String` | `ok` \| `conflict` \| `error:<WriteError>`; `conflict` only on `op=write` |
| `mtime_before` | `Option<Mtime>` | write only: `form.mtime`; omit otherwise |
| `mtime_after` | `Option<Mtime>` | write: `new_mtime` (Written) / `current_mtime` (Conflict); omit otherwise |
| `body_bytes` | `Option<usize>` | write-ok + create-ok: `form.body.len()` (submitted body length, NOT on-disk file size — file also has frontmatter); omit on conflict/error/delete |

Per-outcome population:

- **edit Written** — `op=write`, `result=ok`, `mtime_before=form.mtime`,
  `mtime_after=new_mtime`, `body_bytes=form.body.len()`.
- **edit Conflict** — `op=write`, `result=conflict`, `mtime_before=form.mtime`,
  `mtime_after=current_mtime`, `body_bytes=None` (nothing persisted).
- **edit Error** — `op=write`, `result=error:<variant>`, `mtime_before=form.mtime`,
  rest `None`.
- **create Ok** — `op=create`, `kind=state`, `result=ok`, `body_bytes=body.len()`.
- **create Error** — `op=create`, `kind=state`, `result=error:<variant>`.
- **delete Ok** — `op=delete`, `kind=state`, `result=ok`.

`result=error:<variant>` uses `WriteError`'s `Display` (`file_full`,
`state_full`, `invalid_slug`, `io: …`), e.g. `error:file_full`.

## Changes

### 1. Generic appender on `FileAuditLog` (`crates/core/src/settings/audit.rs`)

**Problem.** `FileAuditLog::append` and the `AuditLog` trait are typed to the
settings-specific `AuditEntry`, so the writer can't serialize a different entry
type without a trait change that ripples into `Arc<dyn AuditLog>` in the
settings store.

**Fix.** Add an inherent generic method that holds the open/`writeln`/`sync_all`
logic:

```rust
impl FileAuditLog {
    pub fn append_serializable<S: serde::Serialize>(&self, entry: &S)
        -> Result<(), AuditError> { /* current append body, generic over S */ }
}
```

The existing `impl AuditLog for FileAuditLog` delegates: `self.append_serializable(entry)`.
No change to the `AuditLog` trait or the settings store. The web route calls
`append_serializable` directly with a `MemoryAuditEntry`.

**Tests.** Existing `file_log_*` tests still pass (path unchanged via delegation).

### 2. `MemoryAuditEntry` + emit helper (`crates/web/src/routes/memory.rs`)

**Fix.** Define `#[derive(Serialize)] struct MemoryAuditEntry { … }` per the
schema above (web-crate-local — it's a web concern). Add a small
`emit_audit(&WebState, &Session, entry)` helper (or inline construction) that
builds the entry with `berlin_now(state.clock.now())` and calls
`state.memory_audit.append_serializable(&entry)`, logging `error!` on failure
and continuing. A `kind_tag(&FileKind) -> &'static str` match yields the
`soul|lore|user|state` discriminant.

### 3. Wire the audit log into `WebState` + construction

**Fix.**
- `WebState` (`crates/web/src/state.rs`) gains `pub memory_audit: Arc<FileAuditLog>`.
- `crates/twitch-1337/src/main.rs` constructs it alongside the settings audit:
  `Arc::new(FileAuditLog::new(get_data_dir().join("memory_audit.log")))`.
- Web test helpers (`crates/web/tests/helpers/mod.rs`, `bin/web_dev.rs`) point it
  at a tempdir / data dir, matching how `settings_audit.log` is wired in tests.

### 4. Emit at the three mutation sites (`crates/web/src/routes/memory.rs`)

**Fix.** Add an `emit_audit(...)` call beside each existing tracing log:
- `save_kind` — in all three `match outcome` arms (Written / Conflict / Err).
- `create_state` — in both `Ok(())` / `Err` arms of the `write_state` match.
- `delete_state` — after the successful `delete_state` call.

Tracing logs stay as-is (cheap, useful for live debugging); the audit line is
additive.

**Tests.** Web-route integration tests (using the helper's tempdir audit file):
- An edit save (success) appends one line with `op=write`, `result=ok`, the
  acting `actor_id`, and a parseable `ts`.
- A stale-mtime save appends `op=write`, `result=conflict`.
- A state create appends `op=create`, `result=ok`; a delete appends `op=delete`,
  `result=ok`.
- All written lines round-trip through `serde_json::from_str` into a
  `serde_json::Value` and expose the expected fields (mirrors the
  acceptance "lines round-trip through `serde_json`").

## Out of scope

- Diff storage (full body before/after) — issue out-of-scope.
- A web UI for browsing the audit log — separate feature.
- Daily rotation, byte cap, and retention of `memory_audit.log` — dropped as
  premature for this volume. If the file ever grows, revisit alongside
  `settings_audit.log` (same untouched-retention question applies to both).
- `bytes_before` / `body_sha256_after` — dropped; no scenario needs them.
- Auditing AI-tool / dreamer writes — they bypass `write_with_guard`; out of
  scope here.
- `delete_state` error path: it bubbles a `500` via `?` before reaching the
  emit site (as the current tracing log also does), so a failed delete is not
  audited. Left as-is; rare (slug is pre-validated, only IO can fail).

## Defaults (confirmed)

- Audit file: `$DATA_DIR/memory_audit.log`, append-only, no rotation/cap.
- `ts` timezone: Europe/Berlin via `berlin_now(state.clock.now())`.
- Ops audited: `write`, `create`, `delete`.
- Append failure: `error!`-and-continue; the dashboard save still succeeds.
