# Schedules config.toml → settings.ron migration

Status: design
Date: 2026-05-24

## Goal

Complete the migration begun in `2026-05-24-config-to-settings-final-migration-design.md`
by moving `[[schedules]]` out of `config.toml` and into the dashboard-managed
`settings.ron`. After this work, `config.toml` holds only credentials,
identity, and dashboard wiring; every operational knob lives in the dashboard.

The existing `/schedules` stub page is replaced with a CRUD UI. The
`notify-debouncer-mini` file watcher is removed; hot reload comes from
a new settings-change `Notify` signal on `SettingsHandle`, also added
in this PR.

## Storage shape

New module `crates/core/src/settings/schedules.rs`:

```rust
pub struct ScheduleSettings {
    pub name: String,
    pub message: String,
    pub interval: String,                       // "hh:mm"
    pub start_date: Option<String>,             // ISO 8601 (YYYY-MM-DDTHH:MM:SS)
    pub end_date: Option<String>,
    pub active_time_start: Option<String>,      // HH:MM
    pub active_time_end: Option<String>,
    pub enabled: bool,
}
```

Added to `Settings`:

```rust
pub struct Settings {
    pub schema_version: u32,  // still 3 — extends PR 1's v3 layout
    // ... existing fields ...
    pub schedules: Vec<ScheduleSettings>,  // default: empty
}
```

Override is **wholesale-replace** — `Option<Vec<ScheduleSettings>>` in
`SettingsOverrides`. `None` resolves to defaults (empty vec); `Some(v)`
wins. No per-entry override; rows are added/edited/deleted by replacing
the whole vec.

`SettingsSection::Schedules` variant added; consumed by `diff_changes`
and the audit log.

## Migration

New section in `settings/migrate.rs::migrate_legacy_config`:

- Read raw `[[schedules]]` array from `toml::Value`.
- For each entry, build `ScheduleSettings` (1:1 field mapping; default
  `enabled = true` when key absent).
- If any entries present, set `overrides.schedules = Some(Some(vec))`.
- Absent → no-op.

**Sentinel: `$DATA_DIR/.schedules_migrated_v3`** — separate from
`.config_migrated_v3`. PR 1 already shipped, so on existing
deployments `.config_migrated_v3` already exists; sharing the sentinel
would silently skip the schedules migration. A second sentinel
guarantees this step runs exactly once on first PR-2 launch.

`main.rs` boot flow:

1. On startup, if `.schedules_migrated_v3` is absent: read raw
   `toml::Value`, build the schedules patch, call `SettingsStore::apply`
   once, touch the sentinel.
2. Audit entry: `Actor::System`, reason `"schedules v3 migration"`.
3. Idempotent — once the sentinel exists, the legacy `[[schedules]]`
   in `config.toml` are ignored on subsequent boots.

Stale-keys warning: on every startup, if the sentinel exists **and**
`config.toml` still contains `[[schedules]]`, emit one `warn!` listing
the stale schedule names. Never delete the user's file.

The legacy `ScheduleConfig` struct in `config.rs` and the
`schedules: Vec<ScheduleConfig>` field on `Configuration` are removed
in this PR. The migration step accesses the legacy data via the raw
`toml::Value` already returned from `load_configuration`.

## Routes

New `crates/web/src/routes/schedules.rs`. All mod-gated (same auth as
`/settings`). All POSTs require CSRF token.

| Method | Path | Action |
|---|---|---|
| GET  | `/schedules` | render list; `?edit=<name>` switches that row to edit mode |
| POST | `/schedules/add` | append new schedule |
| POST | `/schedules/<name>/edit` | replace existing (supports rename via name field) |
| POST | `/schedules/<name>/delete` | remove by name |

Each handler:

1. Read current `Vec<ScheduleSettings>` from `SettingsHandle`.
2. Mutate (push / replace at index / remove).
3. Build `SettingsOverrides { schedules: Some(Some(new_vec)),
   ..Default::default() }`.
4. Call `SettingsStore::apply(overrides, Actor::User { id },
   "schedule add" | "schedule edit" | "schedule delete")`.
5. On validation error: re-render `/schedules` with errors flash; do
   not persist.
6. On success: 303 redirect to `/schedules`.

The stub-page route at `routes/stubs.rs:43` is removed; the new
`schedules::router()` is mounted in its place.

## UI

Replace stub with `crates/web/templates/schedules/index.html`. Layout:

- Page head: title + sub.
- Validation flash (top), same pattern as `/settings`.
- Add-form (always visible, at top): inline row with all fields + `Add`
  button. POSTs to `/schedules/add`.
- List of existing rows below, one row per entry. Each row two modes:
  - **View** (default): name, message preview, interval, enabled
    badge, active-window summary, `Edit` / `Delete` buttons.
  - **Edit**: inline form with all fields + `Save` / `Cancel`. POSTs
    to `/schedules/<name>/edit`. Cancel is a link back to `/schedules`.
- Delete: form-button with `confirm()` JS guard; works without JS (no
  confirmation).
- Edit toggle: query param `?edit=<name>` switches that row to edit
  mode on the server render. No client-side JS for the toggle.

Each row is keyed by `name` (already unique). Rename is delete-old +
add-new under POST (same `apply` call, single audit entry).

Validation errors are grouped by row index and rendered above the
offending row.

## Validation

Port the `validate_config` schedule loop (currently in `lib.rs`) into
`Settings::validate`. Rules per entry:

- `name` non-empty after trim.
- `name` unique across the vec.
- `message` non-empty.
- `interval` parses via `database::Schedule::parse_interval`, > 0 secs.
- `start_date` / `end_date` parse as ISO 8601 when `Some`.
- `active_time_start` / `active_time_end` parse as `HH:MM` when `Some`.
- Both active-time fields set together (either both `Some` or both
  `None`).
- After parsing, `database::Schedule::validate()` runs (date-range
  sanity, active-window sanity).

Errors are emitted as `ValidationError { field:
"schedules[<index>].<field>", message }`. Empty vec is valid.

## Hot reload — drop file watcher

Drop `notify-debouncer-mini` dependency entirely. Remove
`run_config_watcher_service`, `reload_schedules_from_config`,
`load_schedules_from_config`, `schedule_config_to_schedule`,
`parse_datetime`, `parse_time` (the last two move to a small helper
inside `settings/schedules.rs` since they parse `String` → typed values
for the cache regeneration).

Settings-change notifier: `SettingsHandle` gains an
`Arc<tokio::sync::Notify>` "changed" signal.
`SettingsStore::apply` calls `notify.notify_waiters()` after the
successful swap.

New task `run_schedule_settings_sync` in `handlers/schedules.rs`:

1. Subscribe to the settings-changed `Notify`.
2. On notification: regenerate `Vec<database::Schedule>` from
   `handle.load().schedules` (filter disabled, parse to typed
   `Schedule`, skip-with-error-log any individual row that fails
   `Schedule::validate` — validation should have caught these at
   save time, so this is defence-in-depth, not a normal path).
3. Compare against current cache contents; if different, call
   `cache.update(new_vec)` (bumps version).
4. Loop.

Initial cache population: same regeneration path runs once at startup
before the sync task subscribes.

`run_scheduled_message_handler` is unchanged — keeps polling
`ScheduleCache` every 30s and managing per-schedule tasks. The 30s
floor on schedule applicability stays acceptable for v1; if a user
needs faster propagation later, replace the 30s `interval` with a
`Notify`-driven select.

`get_config_path` is no longer referenced by the schedules handler.

## Audit log + diff

`Settings::diff_changes` gains a schedules branch. Compares old vec
vs new vec by name; emits per-row entries:

- `schedules.<name>: added`
- `schedules.<name>: removed`
- `schedules.<name>: modified (<field-list>)`

Rename is reported as `schedules.<old>: removed` + `schedules.<new>:
added` for v1. A future cleanup can detect renames and emit a single
entry; out of scope here.

A single `apply` call from a route handler produces one audit entry
(`reason: "schedule add" | "edit" | "delete"`), whose diff list
captures the per-row change(s).

## Cleanup

- Remove `notify-debouncer-mini` from `crates/core/Cargo.toml` +
  `Cargo.lock` (single commit, per the auto-memory rule).
- Remove `[[schedules]]` examples from
  `crates/twitch-1337/config.toml.example`.
- Remove `ScheduleConfig` struct from `crates/core/src/config.rs`.
- Remove `schedules: Vec<ScheduleConfig>` field from `Configuration`.
- Remove `parse_datetime` / `parse_time` from
  `handlers/schedules.rs` (the new helpers live in
  `settings/schedules.rs`).
- Update `CLAUDE.md` config section: drop the schedules hot-reload
  note + the `notify-debouncer-mini` reference; point to the
  dashboard.
- Update `CLAUDE.md` data-dir / settings notes to mention
  `.schedules_migrated_v3` sentinel.

## Testing

**Unit (`settings/migrate.rs`):**
- Legacy `[[schedules]]` array → expected `Vec<ScheduleSettings>`
  override.
- Absent → no-op (`SettingsOverrides::default()`).
- Partial entries (some fields omitted) → defaults applied (e.g.
  `enabled = true`).

**Unit (`settings/mod.rs` resolve_tests):**
- `None` override → resolved `schedules` is empty vec.
- `Some(vec![...])` → wins.
- `diff_changes` produces per-row `added` / `removed` /
  `modified(<fields>)`.

**Unit (validate):**
- Duplicate `name` rejected.
- Bad `interval` string rejected.
- Only one of `active_time_start` / `active_time_end` set rejected.
- Bad ISO 8601 date rejected.
- Empty vec valid.

**Integration (`crates/web/tests/schedules_route.rs`):**
- GET `/schedules` empty + populated.
- POST `/schedules/add` → row appears, cache version bumps.
- POST `/schedules/<name>/edit` → row replaced; rename supported.
- POST `/schedules/<name>/edit` with duplicate-name → validation
  error, no persist.
- POST `/schedules/<name>/delete` → row gone.
- `?edit=<name>` renders inline form on the matching row only.
- Non-mod returns 403 on all POSTs.

**Integration (handler sync task):**
- `SettingsStore::apply` with new schedules vec → cache reflects
  change before the next 30s tick (sync task wakes on Notify).

**End-to-end migration:**
- Boot with legacy `config.toml` containing `[[schedules]]`, no
  `.schedules_migrated_v3` sentinel → sentinel created, `settings.ron`
  contains the migrated schedules, idempotent second boot does not
  re-apply.
- Boot with sentinel already present + legacy `[[schedules]]` still
  in `config.toml` → warn logged, no migration.

## Out of scope

- Per-schedule audit-log entry (single `apply` covers a batch via the
  diff list; finer granularity is a follow-up).
- Rename detection in `diff_changes` (treated as remove+add for v1).
- Replacing the 30s cache-poll loop in `run_scheduled_message_handler`
  with a Notify-driven select (current 30s floor on applicability is
  acceptable).
- Modal/JS-heavy editing UX — server-rendered `?edit=<name>` toggle
  is the v1 affordance.
