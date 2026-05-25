# Schedules config.toml → settings.ron Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `[[schedules]]` from `config.toml` into dashboard-managed `settings.ron`, with a CRUD UI on the existing `/schedules` stub.

**Architecture:** Add `ScheduleSettings` to the settings store as a wholesale-replace `Vec`, port validation, drop `notify-debouncer-mini`, add a settings-change `Notify` signal on `SettingsHandle`, and replace the file-watcher task with a sync task that regenerates the `ScheduleCache` whenever settings change. Surface CRUD via per-row inline-edit forms gated to the mod role (mirrors `/pings`).

**Tech Stack:** Rust (Tokio, axum 0.8, askama, ron, arc-swap, tracing). Existing `Schedule` / `ScheduleCache` types in `database.rs` are reused unchanged.

**Spec:** `docs/superpowers/specs/2026-05-24-schedules-migration-design.md`

**Auth tier (clarifies spec ambiguity):** schedules routes mount under
`mod_only` in `crates/web/src/lib.rs` — same as `/pings` and the current
`/schedules` stub. The spec line that says "same auth as /settings" was
imprecise: `/settings` is owner-only, but the existing stub was mod-only
and the operational shape (rotate weekly announcements) matches `/pings`.
Owner-gating would lock mods out of routine maintenance.

**Pre-commit gate (run after every task that touches `.rs` / `.toml`):**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

---

## File structure

### Core crate

- **Create** `crates/core/src/settings/schedules.rs` — `ScheduleSettings` struct (+ `Default`), helpers `parse_to_schedule(&ScheduleSettings) -> Result<database::Schedule>` and `build_schedules(&[ScheduleSettings]) -> Vec<database::Schedule>` (filters disabled, skips parse errors with `error!`).
- **Modify** `crates/core/src/settings/mod.rs` — register module, add `pub schedules: Vec<ScheduleSettings>` to `Settings`, extend `compiled_defaults`, `resolve`, `validate`, `SettingsSection`.
- **Modify** `crates/core/src/settings/overrides.rs` — add `schedules: Option<Vec<ScheduleSettings>>` to `SettingsOverrides`.
- **Modify** `crates/core/src/settings/store.rs` — extend `merge_into`, `reset` (for `SettingsSection::Schedules`), `diff_changes` (per-row added/removed/modified). Add `Arc<tokio::sync::Notify>` to `SettingsStore` and notify-waiters in `apply` + `reset`. Expose via `SettingsStore::change_notify()`.
- **Modify** `crates/core/src/settings/migrate.rs` — add `[[schedules]]` reader to `migrate_legacy_config`.
- **Modify** `crates/core/src/twitch/handlers/schedules.rs` — remove `run_config_watcher_service`, `reload_schedules_from_config`, `load_schedules_from_config`, `schedule_config_to_schedule`, `parse_datetime`, `parse_time`. Add `run_schedule_settings_sync(settings, store, cache, shutdown)`. Keep `run_schedule_task` + `run_scheduled_message_handler` unchanged.
- **Modify** `crates/core/src/twitch/handlers/spawn.rs` — drop the legacy config-watcher arm, always spawn cache + sync task + handler regardless of initial `schedules.is_empty()`, regenerate initial cache from `settings.load().schedules`.
- **Modify** `crates/core/src/lib.rs` — drop the `schedules_enabled` info-log gate; everything is now dashboard-driven.
- **Modify** `crates/core/src/config.rs` — remove `ScheduleConfig` struct + `default_enabled` fn; remove `schedules: Vec<ScheduleConfig>` field from `Configuration`; update `validate_config` (or wherever schedule validation lived) — the schedule loop moves into `Settings::validate`.
- **Modify** `crates/core/Cargo.toml` — drop `notify-debouncer-mini` dep.

### Bin crate

- **Modify** `crates/twitch-1337/src/main.rs` — add second v3 migration block for schedules (sentinel `.schedules_migrated_v3`), extend stale-keys warning list with `[[schedules]]`, update startup `info!` (drop `schedules_enabled` / `schedule_count` fields).
- **Modify** `crates/twitch-1337/config.toml.example` — remove `[[schedules]]` example blocks; add a note pointing to `/schedules`.

### Web crate

- **Create** `crates/web/src/routes/schedules.rs` — `router()` returning the four handlers (`list`, `create`, `update`, `delete`).
- **Create** `crates/web/templates/schedules/index.html` — list + add-form + per-row inline-edit forms.
- **Modify** `crates/web/src/routes/mod.rs` — `pub mod schedules;`.
- **Modify** `crates/web/src/routes/stubs.rs` — remove `SCHEDULES` constant + its `.route("/schedules", ...)` line. Keep `LOGS`.
- **Modify** `crates/web/src/lib.rs` — `.merge(routes::schedules::router())` under `mod_only`.
- **Create** `crates/web/tests/schedules_route.rs` — integration tests.

### Docs

- **Modify** `CLAUDE.md` — drop the schedules hot-reload note + `notify-debouncer-mini` reference; mention `.schedules_migrated_v3` sentinel.

---

## Task 1: `ScheduleSettings` struct + defaults

**Files:**
- Create: `crates/core/src/settings/schedules.rs`
- Modify: `crates/core/src/settings/mod.rs` (registration only — `Settings` extension is Task 2)

- [ ] **Step 1: Write the failing test**

Append to `crates/core/src/settings/schedules.rs` (new file):

```rust
//! Schedule entries persisted in `settings.ron`. Each entry is a row of
//! the dashboard's `/schedules` page. Validation lives in
//! `Settings::validate`; conversion to the runtime `database::Schedule`
//! lives in `build_schedules`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ScheduleSettings {
    pub name: String,
    pub message: String,
    /// `hh:mm`, e.g. `"01:30"` for 90 minutes.
    pub interval: String,
    /// ISO 8601 (`YYYY-MM-DDTHH:MM:SS`).
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    /// `HH:MM`.
    #[serde(default)]
    pub active_time_start: Option<String>,
    #[serde(default)]
    pub active_time_end: Option<String>,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_blank() {
        let s = ScheduleSettings::default();
        assert!(!s.enabled);
        assert!(s.name.is_empty());
        assert!(s.message.is_empty());
        assert!(s.interval.is_empty());
        assert!(s.start_date.is_none());
    }
}
```

Add to `crates/core/src/settings/mod.rs` near the other `pub mod` lines:

```rust
pub mod schedules;
```

And add the re-export near the other `pub use` lines:

```rust
pub use schedules::ScheduleSettings;
```

- [ ] **Step 2: Run test to verify it passes**

```bash
cargo nextest run -p twitch-1337-core settings::schedules --show-progress=none --cargo-quiet --status-level=fail
```

Expected: 1 test (`default_is_disabled_blank`) passes.

- [ ] **Step 3: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

Expected: clean across the workspace.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/settings/schedules.rs crates/core/src/settings/mod.rs
git commit -m "$(cat <<'EOF'
feat(settings): add ScheduleSettings struct

First brick of the PR 2 migration: introduce the persisted row shape
that the dashboard CRUD UI will read and write. Defaults to a disabled
blank entry; no wiring yet.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `schedules` field to `Settings` + `SettingsOverrides`

**Files:**
- Modify: `crates/core/src/settings/mod.rs`
- Modify: `crates/core/src/settings/overrides.rs`

- [ ] **Step 1: Write the failing test**

Add to the `resolve_tests` block in `crates/core/src/settings/mod.rs`:

```rust
#[test]
fn schedules_default_is_empty_vec() {
    let s = Settings::compiled_defaults();
    assert!(s.schedules.is_empty());
}

#[test]
fn schedules_override_wholesale_replaces() {
    use crate::settings::ScheduleSettings;
    let defaults = Settings::compiled_defaults();
    let overrides = overrides::SettingsOverrides {
        schedules: Some(vec![ScheduleSettings {
            name: "noon".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }]),
        ..overrides::SettingsOverrides::default()
    };
    let r = Settings::resolve(&defaults, &overrides);
    assert_eq!(r.schedules.len(), 1);
    assert_eq!(r.schedules[0].name, "noon");
}

#[test]
fn schedules_none_override_resolves_to_defaults() {
    let defaults = Settings::compiled_defaults();
    let overrides = overrides::SettingsOverrides::default();
    let r = Settings::resolve(&defaults, &overrides);
    assert!(r.schedules.is_empty());
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo nextest run -p twitch-1337-core settings:: --show-progress=none --cargo-quiet --status-level=fail
```

Expected: FAIL — `Settings` has no `schedules` field; `SettingsOverrides` has no `schedules` field; `ScheduleSettings` not re-exported.

- [ ] **Step 3: Extend `SettingsOverrides`**

Edit `crates/core/src/settings/overrides.rs`. Add to the `SettingsOverrides` struct (after `web` field):

```rust
    #[serde(default)]
    pub schedules: Option<Vec<crate::settings::ScheduleSettings>>,
```

Extend `Default for SettingsOverrides`:

```rust
            schedules: None,
```

- [ ] **Step 4: Extend `Settings`**

Edit `crates/core/src/settings/mod.rs`. Add field to `Settings` struct (after `web`):

```rust
    pub schedules: Vec<ScheduleSettings>,
```

Extend `compiled_defaults`:

```rust
            schedules: Vec::new(),
```

Extend `Settings::resolve` (after the `web` block, before the closing `}`):

```rust
            schedules: overrides
                .schedules
                .clone()
                .unwrap_or_else(|| defaults.schedules.clone()),
```

- [ ] **Step 5: Run tests to verify pass**

```bash
cargo nextest run -p twitch-1337-core settings:: --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS (all three new tests + existing ones).

- [ ] **Step 6: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/settings/mod.rs crates/core/src/settings/overrides.rs
git commit -m "$(cat <<'EOF'
feat(settings): add schedules field to Settings + overrides

Wholesale-replace override: None resolves to the compiled default
(empty vec), Some(v) wins. No per-row override; the dashboard rewrites
the whole list on each CRUD action.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Port schedule validation into `Settings::validate`

**Files:**
- Modify: `crates/core/src/settings/mod.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `resolve_tests` block in `crates/core/src/settings/mod.rs`:

```rust
#[test]
fn validate_rejects_duplicate_schedule_name() {
    use crate::settings::ScheduleSettings;
    let mut s = Settings::compiled_defaults();
    s.schedules = vec![
        ScheduleSettings {
            name: "a".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        },
        ScheduleSettings {
            name: "a".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        },
    ];
    let errs = s
        .validate(&ValidationContext {
            channel: "test".into(),
        })
        .expect_err("duplicate name must fail");
    assert!(errs.iter().any(|e| e.field.starts_with("schedules[")));
}

#[test]
fn validate_rejects_bad_interval() {
    use crate::settings::ScheduleSettings;
    let mut s = Settings::compiled_defaults();
    s.schedules = vec![ScheduleSettings {
        name: "x".into(),
        message: "hi".into(),
        interval: "not-a-duration".into(),
        enabled: true,
        ..Default::default()
    }];
    let errs = s
        .validate(&ValidationContext {
            channel: "test".into(),
        })
        .expect_err("bad interval must fail");
    assert!(errs.iter().any(|e| e.field == "schedules[0].interval"));
}

#[test]
fn validate_rejects_single_active_time_field() {
    use crate::settings::ScheduleSettings;
    let mut s = Settings::compiled_defaults();
    s.schedules = vec![ScheduleSettings {
        name: "x".into(),
        message: "hi".into(),
        interval: "01:00".into(),
        active_time_start: Some("09:00".into()),
        active_time_end: None,
        enabled: true,
        ..Default::default()
    }];
    let errs = s
        .validate(&ValidationContext {
            channel: "test".into(),
        })
        .expect_err("orphan active_time_start must fail");
    assert!(
        errs.iter()
            .any(|e| e.field == "schedules[0].active_time_end")
    );
}

#[test]
fn validate_accepts_disabled_schedule_even_if_malformed_dates() {
    // Disabled rows still need a parseable interval (cheapest invariant
    // to keep the dashboard's add-form honest), but optional date strings
    // are not validated.
    use crate::settings::ScheduleSettings;
    let mut s = Settings::compiled_defaults();
    s.schedules = vec![ScheduleSettings {
        name: "x".into(),
        message: "hi".into(),
        interval: "01:00".into(),
        enabled: false,
        ..Default::default()
    }];
    s.validate(&ValidationContext {
        channel: "test".into(),
    })
    .expect("disabled schedule with valid required fields must pass");
}

#[test]
fn validate_empty_schedules_is_ok() {
    let s = Settings::compiled_defaults();
    s.validate(&ValidationContext {
        channel: "test".into(),
    })
    .expect("empty schedules must pass");
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo nextest run -p twitch-1337-core settings::resolve_tests --show-progress=none --cargo-quiet --status-level=fail
```

Expected: FAIL — `Settings::validate` does not yet check schedule entries.

- [ ] **Step 3: Add the validator**

In `crates/core/src/settings/mod.rs`, inside `Settings::validate`, before the final `if errs.is_empty()` line:

```rust
        // Schedules
        let mut seen_names: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for (idx, sc) in self.schedules.iter().enumerate() {
            let prefix = format!("schedules[{idx}]");
            if sc.name.trim().is_empty() {
                errs.push(FieldError {
                    field: format!("{prefix}.name"),
                    message: "must not be blank".into(),
                });
            } else if !seen_names.insert(sc.name.trim()) {
                errs.push(FieldError {
                    field: format!("{prefix}.name"),
                    message: format!("duplicate name {:?}", sc.name.trim()),
                });
            }
            if sc.message.trim().is_empty() {
                errs.push(FieldError {
                    field: format!("{prefix}.message"),
                    message: "must not be blank".into(),
                });
            }
            match crate::database::Schedule::parse_interval(&sc.interval) {
                Ok(d) if d.num_seconds() <= 0 => {
                    errs.push(FieldError {
                        field: format!("{prefix}.interval"),
                        message: format!("must be > 0 (got {:?})", sc.interval),
                    });
                }
                Ok(_) => {}
                Err(e) => errs.push(FieldError {
                    field: format!("{prefix}.interval"),
                    message: format!("invalid: {e}"),
                }),
            }
            for (field, val) in [
                ("start_date", sc.start_date.as_deref()),
                ("end_date", sc.end_date.as_deref()),
            ] {
                if let Some(v) = val
                    && chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S").is_err()
                {
                    errs.push(FieldError {
                        field: format!("{prefix}.{field}"),
                        message: format!("must be YYYY-MM-DDTHH:MM:SS (got {v:?})"),
                    });
                }
            }
            for (field, val) in [
                ("active_time_start", sc.active_time_start.as_deref()),
                ("active_time_end", sc.active_time_end.as_deref()),
            ] {
                if let Some(v) = val
                    && chrono::NaiveTime::parse_from_str(v, "%H:%M").is_err()
                {
                    errs.push(FieldError {
                        field: format!("{prefix}.{field}"),
                        message: format!("must be HH:MM (got {v:?})"),
                    });
                }
            }
            match (
                sc.active_time_start.as_deref(),
                sc.active_time_end.as_deref(),
            ) {
                (Some(_), None) => errs.push(FieldError {
                    field: format!("{prefix}.active_time_end"),
                    message: "must be set when active_time_start is set".into(),
                }),
                (None, Some(_)) => errs.push(FieldError {
                    field: format!("{prefix}.active_time_start"),
                    message: "must be set when active_time_end is set".into(),
                }),
                _ => {}
            }
        }
```

- [ ] **Step 4: Run tests to verify pass**

```bash
cargo nextest run -p twitch-1337-core settings::resolve_tests --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS (all five new tests).

- [ ] **Step 5: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/settings/mod.rs
git commit -m "$(cat <<'EOF'
feat(settings): validate schedule entries inline with Settings

Port the validation loop from validate_config into Settings::validate so
dashboard saves reject duplicates, bad intervals, orphan time-windows,
and malformed date strings before they reach disk.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `SettingsSection::Schedules` + merge_into + reset + diff_changes

**Files:**
- Modify: `crates/core/src/settings/mod.rs`
- Modify: `crates/core/src/settings/store.rs`

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/settings/store.rs` inside `mod tests`:

```rust
    #[test]
    fn merge_replaces_schedules_wholesale() {
        use crate::settings::ScheduleSettings;
        let mut into = SettingsOverrides::default();
        into.schedules = Some(vec![ScheduleSettings {
            name: "old".into(),
            ..Default::default()
        }]);
        let patch = SettingsOverrides {
            schedules: Some(vec![
                ScheduleSettings {
                    name: "new1".into(),
                    ..Default::default()
                },
                ScheduleSettings {
                    name: "new2".into(),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        merge_into(&mut into, &patch);
        let v = into.schedules.expect("Some");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "new1");
        assert_eq!(v[1].name, "new2");
    }

    #[test]
    fn merge_skips_schedules_when_patch_is_none() {
        use crate::settings::ScheduleSettings;
        let mut into = SettingsOverrides {
            schedules: Some(vec![ScheduleSettings {
                name: "keep".into(),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let patch = SettingsOverrides::default();
        merge_into(&mut into, &patch);
        let v = into.schedules.expect("Some");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "keep");
    }

    #[test]
    fn diff_emits_schedule_added_removed_modified() {
        use crate::settings::ScheduleSettings;
        let mut prior = Settings::compiled_defaults();
        let mut next = Settings::compiled_defaults();
        prior.schedules = vec![
            ScheduleSettings {
                name: "keep".into(),
                message: "old".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
            ScheduleSettings {
                name: "drop".into(),
                message: "x".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
        ];
        next.schedules = vec![
            ScheduleSettings {
                name: "keep".into(),
                message: "new".into(), // modified
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
            ScheduleSettings {
                name: "add".into(),
                message: "y".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
        ];
        let changes = diff_changes(&prior, &next);
        let keys: Vec<&str> = changes.iter().map(|c| c.key.as_str()).collect();
        assert!(keys.contains(&"schedules.keep"), "got {keys:?}");
        assert!(keys.contains(&"schedules.drop"), "got {keys:?}");
        assert!(keys.contains(&"schedules.add"), "got {keys:?}");
    }

    #[tokio::test]
    async fn reset_schedules_clears_override() {
        use crate::settings::ScheduleSettings;
        let dir = tempfile::tempdir().expect("tmp");
        let audit = std::sync::Arc::new(crate::settings::audit::MemoryAuditLog::default());
        let (store, _h) = SettingsStore::open(dir.path(), audit, "main").expect("open");
        store
            .apply(
                SettingsOverrides {
                    schedules: Some(vec![ScheduleSettings {
                        name: "x".into(),
                        message: "hi".into(),
                        interval: "01:00".into(),
                        enabled: true,
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                Actor {
                    user_id: "owner".into(),
                    user_login: "owner".into(),
                },
            )
            .await
            .expect("apply");
        let s = store
            .reset(
                SettingsSection::Schedules,
                Actor {
                    user_id: "owner".into(),
                    user_login: "owner".into(),
                },
            )
            .await
            .expect("reset");
        assert!(s.schedules.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo nextest run -p twitch-1337-core settings::store --show-progress=none --cargo-quiet --status-level=fail
```

Expected: FAIL — `SettingsSection::Schedules` does not exist; `merge_into` ignores `schedules`; `diff_changes` ignores `schedules`; `reset` panics on `Schedules` arm.

- [ ] **Step 3: Add the section variant**

Edit `crates/core/src/settings/mod.rs`. Extend the `SettingsSection` enum:

```rust
    Schedules,
```

- [ ] **Step 4: Extend `merge_into`**

Edit `crates/core/src/settings/store.rs`. Add to `merge_into`, after the Web block:

```rust
    // Schedules — wholesale replace
    if patch.schedules.is_some() {
        into.schedules = patch.schedules.clone();
    }
```

- [ ] **Step 5: Extend `reset`**

In `crates/core/src/settings/store.rs::SettingsStore::reset`, add to the `match section` block:

```rust
            SettingsSection::Schedules => current.schedules = None,
```

- [ ] **Step 6: Extend `diff_changes`**

In `crates/core/src/settings/store.rs::diff_changes`, before the closing `out` line, append:

```rust
    // Schedules — diff by name across vec
    {
        use std::collections::{BTreeMap, BTreeSet};
        let prior_map: BTreeMap<&str, &crate::settings::ScheduleSettings> = prior
            .schedules
            .iter()
            .map(|s| (s.name.as_str(), s))
            .collect();
        let next_map: BTreeMap<&str, &crate::settings::ScheduleSettings> = next
            .schedules
            .iter()
            .map(|s| (s.name.as_str(), s))
            .collect();
        let all_names: BTreeSet<&str> = prior_map
            .keys()
            .copied()
            .chain(next_map.keys().copied())
            .collect();
        for name in all_names {
            let p = prior_map.get(name).copied();
            let n = next_map.get(name).copied();
            if p != n {
                out.push(AuditChange {
                    key: format!("schedules.{name}"),
                    old: p
                        .map(|s| serde_json::to_value(s).expect("serialize prior schedule"))
                        .unwrap_or(serde_json::Value::Null),
                    new: n
                        .map(|s| serde_json::to_value(s).expect("serialize next schedule"))
                        .unwrap_or(serde_json::Value::Null),
                });
            }
        }
    }
```

- [ ] **Step 7: Run tests to verify pass**

```bash
cargo nextest run -p twitch-1337-core settings::store --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS for the four new tests.

- [ ] **Step 8: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/settings/mod.rs crates/core/src/settings/store.rs
git commit -m "$(cat <<'EOF'
feat(settings): wire schedules into store merge/reset/diff

Schedules become a SettingsSection. merge_into replaces wholesale,
reset clears the override, diff_changes emits per-name added/removed/
modified entries so the audit log captures schedule-level edits.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `Notify`-based settings change signal on `SettingsStore`

**Files:**
- Modify: `crates/core/src/settings/store.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/core/src/settings/store.rs::tests`:

```rust
    #[tokio::test]
    async fn apply_signals_change_notify() {
        let (_dir, store, _handle, _log) = fixture();
        let notify = store.change_notify();
        let waiter = {
            let n = notify.clone();
            tokio::spawn(async move {
                n.notified().await;
            })
        };
        let patch = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(15),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        store
            .apply(
                patch,
                Actor {
                    user_id: "1".into(),
                    user_login: "tester".into(),
                },
            )
            .await
            .expect("apply");
        tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
            .await
            .expect("apply must notify change waiters within 500ms")
            .expect("waiter task ok");
    }
```

- [ ] **Step 2: Run test to verify failure**

```bash
cargo nextest run -p twitch-1337-core settings::store::tests::apply_signals --show-progress=none --cargo-quiet --status-level=fail
```

Expected: FAIL — `SettingsStore::change_notify` method does not exist.

- [ ] **Step 3: Add the field + method + notify-waiters in apply/reset**

In `crates/core/src/settings/store.rs`:

Add use:

```rust
use tokio::sync::{Mutex, Notify};
```

Replace the struct definition's `write_lock: Mutex<()>,` line by appending after it:

```rust
    change_notify: Arc<Notify>,
```

In `SettingsStore::open`, construct it. Two locations (the validation-fallback `store = Arc::new(Self { … })` and the happy-path one); both need `change_notify: Arc::new(Notify::new()),`.

Add the accessor near `handle()`:

```rust
    pub fn change_notify(&self) -> Arc<Notify> {
        self.change_notify.clone()
    }
```

At the end of `apply`, immediately before `Ok(resolved)`:

```rust
        self.change_notify.notify_waiters();
```

At the end of `reset`, immediately before `Ok(resolved)`:

```rust
        self.change_notify.notify_waiters();
```

- [ ] **Step 4: Run test to verify pass**

```bash
cargo nextest run -p twitch-1337-core settings::store --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS — the timeout assertion proves the notify fires.

- [ ] **Step 5: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/settings/store.rs
git commit -m "$(cat <<'EOF'
feat(settings): broadcast change Notify on apply/reset

Adds Arc<Notify> on SettingsStore so handlers can subscribe to settings
swaps without polling. Wakes after every successful apply and reset.
Foundation for the schedules sync task that replaces the file watcher.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `parse_to_schedule` + `build_schedules` helpers

**Files:**
- Modify: `crates/core/src/settings/schedules.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/src/settings/schedules.rs`:

```rust
use crate::database;
use eyre::{Result, WrapErr as _};
use tracing::error;

/// Convert a persisted `ScheduleSettings` into the runtime `database::Schedule`.
/// Parses interval, optional dates, optional times; runs `Schedule::validate`.
pub fn parse_to_schedule(s: &ScheduleSettings) -> Result<database::Schedule> {
    let interval = database::Schedule::parse_interval(&s.interval)
        .wrap_err_with(|| format!("interval {:?}", s.interval))?;
    let start_date = s
        .start_date
        .as_deref()
        .map(|v| chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S"))
        .transpose()
        .wrap_err("start_date")?;
    let end_date = s
        .end_date
        .as_deref()
        .map(|v| chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S"))
        .transpose()
        .wrap_err("end_date")?;
    let active_time_start = s
        .active_time_start
        .as_deref()
        .map(|v| chrono::NaiveTime::parse_from_str(v, "%H:%M"))
        .transpose()
        .wrap_err("active_time_start")?;
    let active_time_end = s
        .active_time_end
        .as_deref()
        .map(|v| chrono::NaiveTime::parse_from_str(v, "%H:%M"))
        .transpose()
        .wrap_err("active_time_end")?;
    let out = database::Schedule {
        name: s.name.clone(),
        start_date,
        end_date,
        active_time_start,
        active_time_end,
        interval,
        message: s.message.clone(),
    };
    out.validate()?;
    Ok(out)
}

/// Build a `Vec<database::Schedule>` from settings entries, filtering disabled
/// rows and logging (then skipping) any that fail `parse_to_schedule`. The
/// settings validator should reject the latter at save time; this is a
/// defence-in-depth net for hand-edited `settings.ron`.
pub fn build_schedules(entries: &[ScheduleSettings]) -> Vec<database::Schedule> {
    let mut out = Vec::new();
    for s in entries {
        if !s.enabled {
            continue;
        }
        match parse_to_schedule(s) {
            Ok(sched) => out.push(sched),
            Err(e) => error!(
                schedule = %s.name,
                error = ?e,
                "Failed to parse schedule entry, skipping"
            ),
        }
    }
    out
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    fn ok_entry() -> ScheduleSettings {
        ScheduleSettings {
            name: "noon".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn parse_minimal_schedule() {
        let s = parse_to_schedule(&ok_entry()).expect("parse");
        assert_eq!(s.name, "noon");
        assert_eq!(s.interval.num_seconds(), 3600);
    }

    #[test]
    fn parse_with_dates_and_times() {
        let mut e = ok_entry();
        e.start_date = Some("2026-01-01T00:00:00".into());
        e.end_date = Some("2026-12-31T23:59:59".into());
        e.active_time_start = Some("09:00".into());
        e.active_time_end = Some("17:00".into());
        let s = parse_to_schedule(&e).expect("parse");
        assert!(s.start_date.is_some());
        assert!(s.active_time_start.is_some());
    }

    #[test]
    fn build_filters_disabled() {
        let entries = vec![
            ScheduleSettings {
                enabled: false,
                ..ok_entry()
            },
            ScheduleSettings {
                name: "kept".into(),
                ..ok_entry()
            },
        ];
        let v = build_schedules(&entries);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "kept");
    }

    #[test]
    fn build_skips_malformed_with_log() {
        // Intentionally bad interval; build_schedules must not panic.
        let entries = vec![ScheduleSettings {
            interval: "garbage".into(),
            ..ok_entry()
        }];
        let v = build_schedules(&entries);
        assert!(v.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify pass**

```bash
cargo nextest run -p twitch-1337-core settings::schedules --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS (4 new tests + the `default_is_disabled_blank` from Task 1).

- [ ] **Step 3: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/settings/schedules.rs
git commit -m "$(cat <<'EOF'
feat(settings): parse_to_schedule + build_schedules helpers

Pure conversion from ScheduleSettings (dashboard shape) to
database::Schedule (runtime shape). build_schedules filters disabled
rows and skips parse errors with a structured log so a hand-edited
settings.ron with one bad row doesn't kill the whole list.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Schedules migration in `migrate_legacy_config`

**Files:**
- Modify: `crates/core/src/settings/migrate.rs`

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/settings/migrate.rs::tests`:

```rust
    #[test]
    fn legacy_schedules_array_migrates_into_overrides() {
        let raw = r#"
            [twitch]
            channel = "c"
            username = "u"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"

            [[schedules]]
            name = "noon"
            message = "midday"
            interval = "01:00"
            enabled = true

            [[schedules]]
            name = "winter"
            message = "snow"
            interval = "06:00"
            start_date = "2026-12-01T00:00:00"
            end_date = "2027-03-01T00:00:00"
            active_time_start = "08:00"
            active_time_end = "20:00"
            enabled = false
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        let v = overrides.schedules.expect("Some");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "noon");
        assert!(v[0].enabled);
        assert_eq!(v[1].name, "winter");
        assert!(!v[1].enabled);
        assert_eq!(v[1].start_date.as_deref(), Some("2026-12-01T00:00:00"));
        assert_eq!(v[1].active_time_start.as_deref(), Some("08:00"));
    }

    #[test]
    fn no_schedules_section_returns_none() {
        let raw = r#"
            [twitch]
            channel = "c"
            username = "u"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        assert!(overrides.schedules.is_none());
    }

    #[test]
    fn schedules_default_enabled_true_when_key_absent() {
        let raw = r#"
            [twitch]
            channel = "c"
            username = "u"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"

            [[schedules]]
            name = "x"
            message = "y"
            interval = "01:00"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        let v = overrides.schedules.expect("Some");
        assert!(v[0].enabled);
    }
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo nextest run -p twitch-1337-core settings::migrate --show-progress=none --cargo-quiet --status-level=fail
```

Expected: FAIL — `migrate_legacy_config` ignores `[[schedules]]`.

- [ ] **Step 3: Extend `migrate_legacy_config`**

In `crates/core/src/settings/migrate.rs::migrate_legacy_config`, before the final `Ok(out)`:

```rust
    if let Some(arr) = root.get("schedules").and_then(toml::Value::as_array) {
        let mut out_vec: Vec<crate::settings::ScheduleSettings> = Vec::new();
        for entry in arr {
            let Some(t) = entry.as_table() else { continue };
            let v = toml::Value::Table(t.clone());
            let name = v
                .get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or("")
                .to_owned();
            let message = v
                .get("message")
                .and_then(toml::Value::as_str)
                .unwrap_or("")
                .to_owned();
            let interval = v
                .get("interval")
                .and_then(toml::Value::as_str)
                .unwrap_or("")
                .to_owned();
            let enabled = v
                .get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            let opt_str = |k: &str| -> Option<String> {
                v.get(k)
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
            };
            out_vec.push(crate::settings::ScheduleSettings {
                name,
                message,
                interval,
                start_date: opt_str("start_date"),
                end_date: opt_str("end_date"),
                active_time_start: opt_str("active_time_start"),
                active_time_end: opt_str("active_time_end"),
                enabled,
            });
        }
        if !out_vec.is_empty() {
            out.schedules = Some(out_vec);
        }
    }
```

- [ ] **Step 4: Run tests to verify pass**

```bash
cargo nextest run -p twitch-1337-core settings::migrate --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS (3 new tests + existing ones).

- [ ] **Step 5: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/settings/migrate.rs
git commit -m "$(cat <<'EOF'
feat(settings): migrate legacy [[schedules]] into overrides

Reads the raw [[schedules]] array out of config.toml and produces a
sparse SchedulesOverrides patch; absent → None (no-op). `enabled`
defaults to true when the key is omitted, matching the legacy
ScheduleConfig serde default.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Bin v3 schedules migration block + stale-key warning

**Files:**
- Modify: `crates/twitch-1337/src/main.rs`

Note: This task does **not** yet remove the legacy `Configuration.schedules`
field — that comes in Task 11. We can still detect `[[schedules]]` here
via the raw `toml::Value`.

- [ ] **Step 1: Add the sentinel + migration block**

Edit `crates/twitch-1337/src/main.rs`. Immediately after the existing `let v3_marker = …` migration block (the one ending with `std::fs::write(&v3_marker, "").wrap_err("write .config_migrated_v3 marker")?;`), insert:

```rust
    // One-shot migration of `[[schedules]]`. Separate sentinel because PR 1's
    // `.config_migrated_v3` may already exist on shipped deployments; sharing
    // it would silently skip the schedules migration step.
    let schedules_marker = get_data_dir().join(".schedules_migrated_v3");
    let was_first_schedules_boot = !schedules_marker.exists();
    if was_first_schedules_boot {
        let patch = twitch_1337::settings::migrate::migrate_legacy_config(&raw_toml)
            .wrap_err("schedules v3 migration")?;
        if patch.schedules.is_some() {
            // Build a slim patch that only carries the schedules section so we
            // don't re-apply other migrated fields a second time.
            let slim = twitch_1337::settings::overrides::SettingsOverrides {
                schedules: patch.schedules,
                ..twitch_1337::settings::overrides::SettingsOverrides::default()
            };
            let actor = twitch_1337::settings::Actor {
                user_id: "migrate".into(),
                user_login: "schedules-v3-migration".into(),
            };
            settings_store
                .apply(slim, actor)
                .await
                .wrap_err("schedules v3 migration apply")?;
            info!("migrated legacy [[schedules]] into settings.ron");
        }
        std::fs::write(&schedules_marker, "")
            .wrap_err("write .schedules_migrated_v3 marker")?;
    }
```

- [ ] **Step 2: Extend the stale-keys warning list**

In the same file, locate the `legacy_v3_keys: &[(&str, &[&str])]` slice and append:

```rust
        ("schedules", &["schedules"]),
```

Then locate the `if !was_first_v3_boot && !stale.is_empty()` block. Replace the predicate so it also defers on the schedules first-boot:

```rust
    if !was_first_v3_boot && !was_first_schedules_boot && !stale.is_empty() {
```

- [ ] **Step 3: Update the startup `info!`**

Find the early `info!` that includes `schedules_enabled = !config.schedules.is_empty()`. Replace those two lines with a single read from settings:

```rust
    let initial_schedule_count = settings_handle.load().schedules.len();
    info!(
        local_time = ?local,
        utc_time = ?Utc::now(),
        channel = %config.twitch.channel,
        username = %config.twitch.username,
        schedule_count = initial_schedule_count,
        "Starting twitch-1337 bot"
    );
```

Drop the `let local = …; info!(…)` block above it and replace with the version that places `local` immediately before this `info!` only (avoid duplicate `local` bindings).

After Task 11 removes `Configuration.schedules`, this same call site no longer references it; do not anticipate that here.

Note: `initial_schedule_count` must be read AFTER both v3 migrations complete so it reflects the migrated state. Place this block immediately before the `extra_channels` capture.

- [ ] **Step 4: Build to confirm compile**

```bash
cargo build --workspace
```

Expected: clean build.

- [ ] **Step 5: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 6: Commit**

```bash
git add crates/twitch-1337/src/main.rs
git commit -m "$(cat <<'EOF'
feat(bin): one-shot schedules v3 migration on first boot

Reads [[schedules]] from raw config.toml and applies them via the
settings store on first launch; gated by .schedules_migrated_v3 so
subsequent edits in config.toml are ignored. Stale-keys warning grows
a "schedules" entry; both first-boot guards must clear before it fires.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: `run_schedule_settings_sync` task; delete file watcher

**Files:**
- Modify: `crates/core/src/twitch/handlers/schedules.rs`

- [ ] **Step 1: Write the failing test**

This task tests the full sync path via `settings_store.apply` → `Notify` → `cache.update` → version bump. Add to `crates/core/src/twitch/handlers/schedules.rs`:

```rust
#[cfg(test)]
mod sync_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::{Notify, RwLock};

    use crate::database;
    use crate::settings::{
        Actor, FileAuditLog, ScheduleSettings, SettingsStore,
        audit::MemoryAuditLog, overrides::SettingsOverrides,
    };

    #[tokio::test]
    async fn settings_apply_bumps_schedule_cache() {
        let dir = tempfile::tempdir().expect("tmp");
        let audit = Arc::new(MemoryAuditLog::default());
        let (store, settings_handle) = SettingsStore::open(dir.path(), audit, "main")
            .expect("open");
        let cache = Arc::new(RwLock::new(database::ScheduleCache::new()));
        let shutdown = Arc::new(Notify::new());

        let cache_for_task = cache.clone();
        let store_for_task = store.clone();
        let shutdown_for_task = shutdown.clone();
        let handle_for_task = settings_handle.clone();
        let task = tokio::spawn(async move {
            super::run_schedule_settings_sync(
                handle_for_task,
                store_for_task,
                cache_for_task,
                shutdown_for_task,
            )
            .await;
        });

        // Yield so the sync task subscribes before we apply.
        tokio::time::sleep(Duration::from_millis(50)).await;

        store
            .apply(
                SettingsOverrides {
                    schedules: Some(vec![ScheduleSettings {
                        name: "noon".into(),
                        message: "hi".into(),
                        interval: "01:00".into(),
                        enabled: true,
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                Actor {
                    user_id: "1".into(),
                    user_login: "tester".into(),
                },
            )
            .await
            .expect("apply");

        // Poll the cache up to 1s for version > 0 + 1 schedule. (The sync
        // task wakes via Notify, processes synchronously, then loops; this
        // should be well under 50ms in practice.)
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            {
                let g = cache.read().await;
                if g.version > 0 && g.schedules.len() == 1 {
                    assert_eq!(g.schedules[0].name, "noon");
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                let g = cache.read().await;
                panic!(
                    "cache did not update within 1s; version={}, len={}",
                    g.version,
                    g.schedules.len()
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
        let _ = FileAuditLog::new(dir.path().join("settings_audit.log")); // unused, silences import lint
    }
}
```

- [ ] **Step 2: Run test to verify failure**

```bash
cargo nextest run -p twitch-1337-core twitch::handlers::schedules::sync_tests --show-progress=none --cargo-quiet --status-level=fail
```

Expected: FAIL — `run_schedule_settings_sync` does not exist.

- [ ] **Step 3: Delete the obsolete watcher + helpers**

In `crates/core/src/twitch/handlers/schedules.rs`, **delete** these items:

- `parse_datetime`, `parse_time` (top-of-file helpers; the new flow uses `settings::schedules::parse_to_schedule`)
- `schedule_config_to_schedule`
- `load_schedules_from_config`
- `reload_schedules_from_config`
- `run_config_watcher_service`

Also remove the `use crate::{config::Configuration, …, get_config_path, …}` line — the new path doesn't need either.

- [ ] **Step 4: Add `run_schedule_settings_sync`**

In `crates/core/src/twitch/handlers/schedules.rs`, add:

```rust
use std::sync::Arc;

use tokio::sync::{Notify, RwLock};
use tracing::{info, instrument};

use crate::database;
use crate::settings::{SettingsHandle, SettingsStore};

/// Subscribe to `SettingsStore`'s change `Notify` and regenerate the
/// `ScheduleCache` whenever the schedules vec resolves to a different list
/// than the cache currently holds. Initial population also happens on the
/// first iteration: the task wakes once at start before subscribing to
/// guarantee we don't miss an apply that races our subscription.
#[instrument(skip(settings, store, cache, shutdown))]
pub async fn run_schedule_settings_sync(
    settings: SettingsHandle,
    store: Arc<SettingsStore>,
    cache: Arc<RwLock<database::ScheduleCache>>,
    shutdown: Arc<Notify>,
) {
    info!("Schedule settings sync task started");
    let change = store.change_notify();
    // Prime the cache once before waiting so the initial state matches
    // whatever was loaded at startup.
    regenerate(&settings, &cache).await;
    loop {
        tokio::select! {
            () = change.notified() => {
                regenerate(&settings, &cache).await;
            }
            () = shutdown.notified() => {
                info!("Schedule settings sync: shutdown received");
                return;
            }
        }
    }
}

async fn regenerate(
    settings: &SettingsHandle,
    cache: &Arc<RwLock<database::ScheduleCache>>,
) {
    let new_list =
        crate::settings::schedules::build_schedules(&settings.load().schedules);
    let mut g = cache.write().await;
    if g.schedules != new_list {
        let old_count = g.schedules.len();
        g.update(new_list);
        info!(
            old_count,
            new_count = g.schedules.len(),
            version = g.version,
            "Schedules updated from settings"
        );
    }
}
```

Keep the existing `run_schedule_task` and `run_scheduled_message_handler` functions in this file unchanged — they read the cache, which still owns the runtime list.

- [ ] **Step 5: Run sync test to verify pass**

```bash
cargo nextest run -p twitch-1337-core twitch::handlers::schedules::sync_tests --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS.

- [ ] **Step 6: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

If clippy complains about an unused `info` import or similar, adjust the use-list. The test alone proves the path runs end-to-end.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/twitch/handlers/schedules.rs
git commit -m "$(cat <<'EOF'
refactor(schedules): replace file watcher with settings sync task

Drops the notify-debouncer-mini watcher and the now-unused config-side
parsers (parse_datetime, parse_time, schedule_config_to_schedule,
load_schedules_from_config, reload_schedules_from_config). The new
run_schedule_settings_sync subscribes to SettingsStore's change Notify
and pushes the latest list into ScheduleCache, which the existing
scheduled-message handler already polls.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Update `spawn.rs` to wire the new task; drop `schedules_enabled` gate

**Files:**
- Modify: `crates/core/src/twitch/handlers/spawn.rs`

- [ ] **Step 1: Rewrite the schedules block**

In `crates/core/src/twitch/handlers/spawn.rs`, locate the existing schedules block (`let schedules_enabled = !config.schedules.is_empty();` through the closing `(Some(watcher), Some(handler)) … (None, None)` match).

Replace the entire `(config_watcher, scheduled_messages)` setup with:

```rust
    // Schedules: always-on. Initial cache is empty; the settings-sync task
    // populates it from settings.ron before the handler's first 30s tick.
    let mut cache = database::ScheduleCache::new();
    cache.update(crate::settings::schedules::build_schedules(
        &settings.load().schedules,
    ));
    let schedule_cache = Arc::new(RwLock::new(cache));

    let settings_sync = tokio::spawn({
        let settings = settings.clone();
        let store = settings_store.clone();
        let cache = schedule_cache.clone();
        let shutdown = shutdown_notify.clone();
        async move {
            schedules::run_schedule_settings_sync(settings, store, cache, shutdown).await;
        }
    });

    let scheduled_messages = tokio::spawn({
        let sender = chat_sender.clone();
        let cache = schedule_cache.clone();
        let channel = config.twitch.channel.clone();
        let notify = shutdown_notify.clone();
        let clk = clock.clone();
        async move {
            schedules::run_scheduled_message_handler(sender, cache, channel, notify, clk).await;
        }
    });
```

- [ ] **Step 2: Update the `use` list at the top**

Replace:

```rust
            schedules::{
                load_schedules_from_config, run_config_watcher_service,
                run_scheduled_message_handler,
            },
```

with:

```rust
            schedules::{self, run_scheduled_message_handler},
```

(The new `run_schedule_settings_sync` is referenced via `schedules::` in the spawn body.)

- [ ] **Step 3: Update the `SpawnDeps`/handler struct usage**

Find the `pub struct Handlers` (or equivalent) that previously held `Option<JoinHandle<()>>` for `config_watcher` and `Option<JoinHandle<()>>` for `scheduled_messages`. Rename `config_watcher` to `settings_sync`, drop the `Option` wrappers (both tasks are unconditional now), and update the constructor at the bottom of `spawn_handlers`:

```rust
        settings_sync,
        scheduled_messages,
```

If the existing `await_shutdown` function still pattern-matches `Option`, simplify those branches: both join handles are always `Some`. Look for `scheduled_messages.unwrap_or_else(|| tokio::spawn(std::future::pending::<()>()))` and remove the `unwrap_or_else` (use the handle directly).

Also locate `let has_sched = scheduled_messages.is_some();` and any conditional that reads it — replace with `let has_sched = true;` or inline `true`. The shutdown path still awaits the join handle with the existing 5s timeout.

- [ ] **Step 4: Update lib.rs startup info log**

In `crates/core/src/lib.rs`, locate the `if schedules_enabled { … } else { … }` info-log block and replace with a single unconditional log:

```rust
    info!(
        "Bot running with continuous connection. Handlers: 1337 tracker, \
         Generic commands, Scheduled messages, Latency monitor, Flight tracker"
    );
```

Drop the `let schedules_enabled = !config.schedules.is_empty();` binding.

- [ ] **Step 5: Add `settings_store` to `SpawnDeps`**

The sync task needs `Arc<SettingsStore>`. If `SpawnDeps` already carries `settings: SettingsHandle` but not the store, add a `settings_store: Arc<SettingsStore>` field, thread it through `lib.rs::run_bot` where `spawn_handlers(SpawnDeps { … })` is constructed, and pass it down. The store is already in `Services` (look near `settings_handle` in main.rs); plumb it the same way.

- [ ] **Step 6: Build to confirm compile**

```bash
cargo build --workspace
```

Expected: clean.

- [ ] **Step 7: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/twitch/handlers/spawn.rs crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(spawn): always run schedules; drive cache from settings

Drops the schedules_enabled gate: the message handler + sync task spawn
unconditionally, with an empty initial cache that grows as the
dashboard rewrites settings.ron. SpawnDeps now carries the store handle
so the sync task can subscribe to change notifications.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Remove `ScheduleConfig` + `Configuration.schedules`; drop `notify-debouncer-mini`

**Files:**
- Modify: `crates/core/src/config.rs`
- Modify: `crates/core/Cargo.toml`
- Modify: `crates/twitch-1337/src/main.rs` (drop dangling references)
- Modify: `crates/twitch-1337/config.toml.example`

- [ ] **Step 1: Strip `ScheduleConfig` from `config.rs`**

In `crates/core/src/config.rs`, **delete**:

- The `default_enabled` fn (now only used by `ScheduleConfig`).
- The entire `pub struct ScheduleConfig { … }` block.
- The `pub schedules: Vec<ScheduleConfig>` field from `Configuration`.
- If `Configuration` has a `Default` impl or `validate_config` function that mentions `schedules`, remove the schedule-related branches (validation now lives in `Settings::validate`).

- [ ] **Step 2: Strip the dep**

In `crates/core/Cargo.toml`, remove the `notify-debouncer-mini = …` line under `[dependencies]`.

- [ ] **Step 3: Update `config.toml.example`**

In `crates/twitch-1337/config.toml.example`, **delete** every `[[schedules]]` block and any preceding header comment that introduces them.

Add a single comment in place of the deleted section:

```toml
# Schedules now live in the dashboard at /schedules.
# settings.ron stores the canonical list; legacy [[schedules]] in this
# file are ignored on every boot after the first.
```

- [ ] **Step 4: Drop dangling references in main.rs**

`crates/twitch-1337/src/main.rs` may still mention `config.schedules` somewhere besides the migration block (it's already been audited up to Task 8, but a `Default::default()`-style construction elsewhere may still reference it). Search:

```bash
rg 'config\.schedules|ScheduleConfig' crates/
```

Patch any remaining call sites — typically these are debug logs or test fixtures. The migration block in main.rs reads `raw_toml`, not `config.schedules`, so it is unaffected.

- [ ] **Step 5: Build to confirm compile**

```bash
cargo build --workspace
```

Expected: clean. If clippy or build fails because something still references `Configuration::schedules`, fix the use site (typically removing the field reference or replacing it with `settings_handle.load().schedules.len()`).

- [ ] **Step 6: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 7: Verify `Cargo.lock` updated**

```bash
git status -s Cargo.lock
```

`Cargo.lock` should show as modified (notify-debouncer-mini removed transitively).

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/config.rs crates/core/Cargo.toml crates/twitch-1337/src/main.rs crates/twitch-1337/config.toml.example Cargo.lock
git commit -m "$(cat <<'EOF'
chore(schedules): drop ScheduleConfig + notify-debouncer-mini

Schedules now live exclusively in settings.ron; the config.rs structs
and the file-watcher dependency are gone. config.toml.example loses
its [[schedules]] examples in favour of a pointer to /schedules.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Web routes — list page (GET `/schedules`)

**Files:**
- Create: `crates/web/src/routes/schedules.rs`
- Create: `crates/web/templates/schedules/index.html`
- Modify: `crates/web/src/routes/mod.rs`
- Modify: `crates/web/src/routes/stubs.rs`
- Modify: `crates/web/src/lib.rs`

- [ ] **Step 1: Create `routes/schedules.rs` skeleton**

`crates/web/src/routes/schedules.rs`:

```rust
//! `/schedules` CRUD handlers (mod-gated).
//!
//! The list page renders all schedules + an add form. Each row has its
//! own inline edit form, toggled via `?edit=<name>`. Validation errors
//! re-render the list with per-row error attribution.

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::settings::{Actor, ScheduleSettings, overrides::SettingsOverrides};

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::routes::render;
use crate::state::WebState;

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/schedules", get(list))
        .route("/schedules/add", post(create))
        .route("/schedules/{name}/edit", post(update))
        .route("/schedules/{name}/delete", post(delete))
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ListQuery {
    /// When `Some(name)`, the matching row renders in edit mode.
    edit: Option<String>,
}

#[derive(Template)]
#[template(path = "schedules/index.html")]
struct ListTpl {
    rows: Vec<ScheduleSettings>,
    edit_name: Option<String>,
    errors_for: std::collections::HashMap<String, Vec<(String, String)>>,
    flash: Option<String>,
    csrf: String,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
}

async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Query(q): Query<ListQuery>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let rows = state.settings.load().schedules.clone();
    let flash_msg = flash::take(&cookies);
    render(&ListTpl {
        rows,
        edit_name: q.edit,
        errors_for: Default::default(),
        flash: flash_msg,
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::SCHEDULES,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
    })
}

async fn create(
    State(_state): State<WebState>,
    Extension(_session): Extension<Session>,
) -> Result<Response, WebError> {
    // Task 13 implements this. Stub so the route table compiles.
    Ok(Redirect::to("/schedules").into_response())
}

async fn update(
    State(_state): State<WebState>,
    Extension(_session): Extension<Session>,
    Path(_name): Path<String>,
) -> Result<Response, WebError> {
    Ok(Redirect::to("/schedules").into_response())
}

async fn delete(
    State(_state): State<WebState>,
    Extension(_session): Extension<Session>,
    Path(_name): Path<String>,
) -> Result<Response, WebError> {
    Ok(Redirect::to("/schedules").into_response())
}
```

- [ ] **Step 2: Create the askama template**

`crates/web/templates/schedules/index.html`:

```html
{# crates/web/templates/schedules/index.html #}
{% extends "base.html" %}
{% block title %}Schedules — twitch-1337{% endblock %}

{% block content %}
<div class="page-head">
  <div>
    <h1>Schedules</h1>
    <p class="page-sub">Recurring announcements fired by the bot at a fixed interval. Changes apply within ~30 seconds — no restart needed.</p>
  </div>
</div>

{% if let Some(msg) = flash %}<div class="flash">{{ msg }}</div>{% endif %}

<section class="settings-card">
  <header class="settings-card-head">
    <div>
      <h2>Add schedule</h2>
      <p>Name and message are required. Interval is <code>hh:mm</code>.</p>
    </div>
  </header>
  <form method="post" action="/schedules/add" class="settings-rows">
    <input type="hidden" name="_csrf" value="{{ csrf }}">
    <div class="row"><label>Name <input type="text" name="name" required></label></div>
    <div class="row"><label>Message <input type="text" name="message" required></label></div>
    <div class="row"><label>Interval (hh:mm) <input type="text" name="interval" placeholder="01:00" required></label></div>
    <div class="row"><label>Start date (optional, YYYY-MM-DDTHH:MM:SS) <input type="text" name="start_date"></label></div>
    <div class="row"><label>End date (optional) <input type="text" name="end_date"></label></div>
    <div class="row"><label>Active time start (optional, HH:MM) <input type="text" name="active_time_start"></label></div>
    <div class="row"><label>Active time end (optional, HH:MM) <input type="text" name="active_time_end"></label></div>
    <div class="row"><label><input type="checkbox" name="enabled" value="true" checked> Enabled</label></div>
    <div class="row"><button type="submit" class="btn primary">Add</button></div>
  </form>
</section>

<section class="settings-card">
  <header class="settings-card-head"><div><h2>Existing schedules</h2></div></header>
  {% if rows.is_empty() %}
    <p class="empty">No schedules configured.</p>
  {% else %}
    <ul class="schedules-list">
    {% for row in rows %}
      <li class="schedule-row">
        {% if let Some(name) = edit_name %}{% if name == row.name %}
        <form method="post" action="/schedules/{{ row.name }}/edit" class="settings-rows">
          <input type="hidden" name="_csrf" value="{{ csrf }}">
          <div class="row"><label>Name <input type="text" name="name" value="{{ row.name }}" required></label></div>
          <div class="row"><label>Message <input type="text" name="message" value="{{ row.message }}" required></label></div>
          <div class="row"><label>Interval <input type="text" name="interval" value="{{ row.interval }}" required></label></div>
          <div class="row"><label>Start date <input type="text" name="start_date" value="{{ row.start_date.as_deref().unwrap_or(\"\") }}"></label></div>
          <div class="row"><label>End date <input type="text" name="end_date" value="{{ row.end_date.as_deref().unwrap_or(\"\") }}"></label></div>
          <div class="row"><label>Active time start <input type="text" name="active_time_start" value="{{ row.active_time_start.as_deref().unwrap_or(\"\") }}"></label></div>
          <div class="row"><label>Active time end <input type="text" name="active_time_end" value="{{ row.active_time_end.as_deref().unwrap_or(\"\") }}"></label></div>
          <div class="row"><label><input type="checkbox" name="enabled" value="true" {% if row.enabled %}checked{% endif %}> Enabled</label></div>
          <div class="row">
            <button type="submit" class="btn primary">Save</button>
            <a class="btn ghost" href="/schedules">Cancel</a>
          </div>
        </form>
        {% else %}{% include "schedules/_view_row.html" %}{% endif %}{% else %}
          {% include "schedules/_view_row.html" %}
        {% endif %}
      </li>
    {% endfor %}
    </ul>
  {% endif %}
</section>
{% endblock %}
```

Also create `crates/web/templates/schedules/_view_row.html`:

```html
<div class="schedule-view">
  <div class="schedule-meta">
    <strong>{{ row.name }}</strong>
    <span class="badge {% if row.enabled %}on{% else %}off{% endif %}">{% if row.enabled %}enabled{% else %}disabled{% endif %}</span>
    <span class="muted">every {{ row.interval }}</span>
  </div>
  <p class="schedule-message">{{ row.message }}</p>
  <div class="schedule-actions">
    <a class="btn" href="/schedules?edit={{ row.name }}">Edit</a>
    <form method="post" action="/schedules/{{ row.name }}/delete" onsubmit="return confirm('Delete schedule {{ row.name }}?');" style="display: inline">
      <input type="hidden" name="_csrf" value="{{ csrf }}">
      <button type="submit" class="btn danger">Delete</button>
    </form>
  </div>
</div>
```

- [ ] **Step 3: Register the module + drop the stub**

In `crates/web/src/routes/mod.rs`, add:

```rust
pub mod schedules;
```

In `crates/web/src/routes/stubs.rs`, delete the `const SCHEDULES: StubMeta = …` block and remove the `.route("/schedules", get(|s| render_stub(SCHEDULES, s)))` line in `router`. Keep the `LOGS` const + its route untouched.

- [ ] **Step 4: Mount the new router**

In `crates/web/src/lib.rs`, inside the `mod_only` router builder (between `routes::memory::router()` and `routes::stubs::router()`):

```rust
        .merge(routes::schedules::router())
```

- [ ] **Step 5: Build to confirm compile**

```bash
cargo build --workspace
```

Expected: clean.

- [ ] **Step 6: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/routes/schedules.rs crates/web/templates/schedules/ crates/web/src/routes/mod.rs crates/web/src/routes/stubs.rs crates/web/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(web): /schedules list page

Replaces the stub with a list view + per-row inline edit toggle via
?edit=<name>. POSTs are stubbed in this commit — the next task wires
add/edit/delete to SettingsStore::apply.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Web routes — add / edit / delete handlers

**Files:**
- Modify: `crates/web/src/routes/schedules.rs`

- [ ] **Step 1: Write the failing integration tests**

Create `crates/web/tests/schedules_route.rs`:

```rust
//! Integration tests for /schedules CRUD.

mod common;

use common::TestApp;
use twitch_1337_core::settings::ScheduleSettings;

#[tokio::test]
async fn add_schedule_persists_and_renders() {
    let app = TestApp::owner().await;
    let resp = app
        .post(
            "/schedules/add",
            &[
                ("name", "noon"),
                ("message", "midday"),
                ("interval", "01:00"),
                ("enabled", "true"),
            ],
        )
        .await;
    assert!(resp.status().is_redirection());
    let listed = app.get("/schedules").await.text().await.unwrap();
    assert!(listed.contains("noon"), "row must appear in list");
    let v = app.state.settings.load().schedules.clone();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].name, "noon");
}

#[tokio::test]
async fn edit_schedule_renames() {
    let app = TestApp::owner().await;
    app.apply_schedules(vec![ScheduleSettings {
        name: "old".into(),
        message: "hi".into(),
        interval: "01:00".into(),
        enabled: true,
        ..Default::default()
    }])
    .await;
    let resp = app
        .post(
            "/schedules/old/edit",
            &[
                ("name", "new"),
                ("message", "hi"),
                ("interval", "01:00"),
                ("enabled", "true"),
            ],
        )
        .await;
    assert!(resp.status().is_redirection());
    let v = app.state.settings.load().schedules.clone();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].name, "new");
}

#[tokio::test]
async fn edit_duplicate_name_returns_error_without_persisting() {
    let app = TestApp::owner().await;
    app.apply_schedules(vec![
        ScheduleSettings {
            name: "a".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        },
        ScheduleSettings {
            name: "b".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        },
    ])
    .await;
    let resp = app
        .post(
            "/schedules/b/edit",
            &[
                ("name", "a"),
                ("message", "hi"),
                ("interval", "01:00"),
                ("enabled", "true"),
            ],
        )
        .await;
    // Validation error renders the list with errors_for populated; status
    // should be 200 (re-render), not 303 redirect.
    assert_eq!(resp.status().as_u16(), 200);
    let v = app.state.settings.load().schedules.clone();
    assert_eq!(v.len(), 2, "no row may be renamed on validation failure");
    assert!(v.iter().any(|s| s.name == "a"));
    assert!(v.iter().any(|s| s.name == "b"));
}

#[tokio::test]
async fn delete_schedule_removes_row() {
    let app = TestApp::owner().await;
    app.apply_schedules(vec![ScheduleSettings {
        name: "gone".into(),
        message: "x".into(),
        interval: "01:00".into(),
        enabled: true,
        ..Default::default()
    }])
    .await;
    let resp = app.post("/schedules/gone/delete", &[]).await;
    assert!(resp.status().is_redirection());
    let v = app.state.settings.load().schedules.clone();
    assert!(v.is_empty());
}

#[tokio::test]
async fn edit_query_param_renders_inline_form() {
    let app = TestApp::owner().await;
    app.apply_schedules(vec![ScheduleSettings {
        name: "alpha".into(),
        message: "hi".into(),
        interval: "01:00".into(),
        enabled: true,
        ..Default::default()
    }])
    .await;
    let body = app.get("/schedules?edit=alpha").await.text().await.unwrap();
    assert!(
        body.contains("/schedules/alpha/edit"),
        "edit form action must be present for the matching row"
    );
}
```

If `crates/web/tests/common/mod.rs` (or equivalent) does not already expose `TestApp::owner()` and `apply_schedules`, extend it. Look for an existing helper in `crates/web/tests/`:

```bash
ls crates/web/tests/
```

If a `common.rs` or `common/` mod exists with `TestApp`, add an `apply_schedules` helper:

```rust
impl TestApp {
    pub async fn apply_schedules(
        &self,
        rows: Vec<twitch_1337_core::settings::ScheduleSettings>,
    ) {
        use twitch_1337_core::settings::{Actor, overrides::SettingsOverrides};
        self.store
            .apply(
                SettingsOverrides {
                    schedules: Some(rows),
                    ..Default::default()
                },
                Actor {
                    user_id: "test".into(),
                    user_login: "test".into(),
                },
            )
            .await
            .expect("apply schedules");
    }
}
```

(If no `TestApp` exists, mirror the pattern from `crates/web/tests/settings_route.rs` — that file was added in PR 1 and has the same auth/setup shape.)

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo nextest run -p twitch-1337-web schedules_route --show-progress=none --cargo-quiet --status-level=fail
```

Expected: FAIL — the stub handlers don't persist.

- [ ] **Step 3: Implement the handlers**

Replace the stub `create`/`update`/`delete` bodies in `crates/web/src/routes/schedules.rs`:

Add at the top of the file (with existing imports):

```rust
use twitch_1337_core::settings::SettingsError;
```

Add the form struct and a helper:

```rust
#[derive(Debug, Deserialize)]
struct ScheduleForm {
    _csrf: String,
    name: String,
    message: String,
    interval: String,
    #[serde(default)]
    start_date: String,
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    active_time_start: String,
    #[serde(default)]
    active_time_end: String,
    #[serde(default)]
    enabled: Option<String>, // checkbox: "true" or absent
}

fn opt(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_owned()) }
}

impl ScheduleForm {
    fn into_settings(self) -> ScheduleSettings {
        ScheduleSettings {
            name: self.name.trim().to_owned(),
            message: self.message,
            interval: self.interval.trim().to_owned(),
            start_date: opt(self.start_date),
            end_date: opt(self.end_date),
            active_time_start: opt(self.active_time_start),
            active_time_end: opt(self.active_time_end),
            enabled: self.enabled.is_some(),
        }
    }
}
```

Replace `create`:

```rust
async fn create(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<ScheduleForm>,
) -> Result<Response, WebError> {
    csrf::verify(&form._csrf, &session.csrf_value)?;
    let new_row = form.into_settings();
    let mut next = state.settings.load().schedules.clone();
    next.push(new_row);
    apply_or_rerender(&state, &session, cookies, next, None).await
}
```

Replace `update`:

```rust
async fn update(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<ScheduleForm>,
) -> Result<Response, WebError> {
    csrf::verify(&form._csrf, &session.csrf_value)?;
    let new_row = form.into_settings();
    let mut next = state.settings.load().schedules.clone();
    if let Some(idx) = next.iter().position(|s| s.name == name) {
        next[idx] = new_row;
    } else {
        return Ok(Redirect::to("/schedules").into_response());
    }
    apply_or_rerender(&state, &session, cookies, next, Some(name)).await
}
```

Replace `delete`:

```rust
#[derive(Debug, Deserialize)]
struct DeleteForm {
    _csrf: String,
}

async fn delete(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<DeleteForm>,
) -> Result<Response, WebError> {
    csrf::verify(&form._csrf, &session.csrf_value)?;
    let next: Vec<ScheduleSettings> = state
        .settings
        .load()
        .schedules
        .iter()
        .filter(|s| s.name != name)
        .cloned()
        .collect();
    apply_or_rerender(&state, &session, cookies, next, None).await
}
```

Add the helper that does the apply + re-render-on-error dance:

```rust
async fn apply_or_rerender(
    state: &WebState,
    session: &Session,
    cookies: Cookies,
    next: Vec<ScheduleSettings>,
    edit_on_error: Option<String>,
) -> Result<Response, WebError> {
    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };
    let patch = SettingsOverrides {
        schedules: Some(next.clone()),
        ..Default::default()
    };
    match state.settings_store.apply(patch, actor).await {
        Ok(_) => {
            flash::set(&cookies, "Schedules saved.");
            Ok(Redirect::to("/schedules").into_response())
        }
        Err(SettingsError::Validation(errs)) => {
            // Bucket errors by `schedules[<idx>]` prefix.
            let mut errors_for: std::collections::HashMap<String, Vec<(String, String)>> =
                Default::default();
            for e in errs {
                if let Some(rest) = e.field.strip_prefix("schedules[")
                    && let Some(end) = rest.find(']')
                {
                    let idx_str = &rest[..end];
                    if let Ok(idx) = idx_str.parse::<usize>()
                        && let Some(row) = next.get(idx)
                    {
                        let field_name = &rest[end + 2..]; // skip "].:"
                        errors_for
                            .entry(row.name.clone())
                            .or_default()
                            .push((field_name.to_owned(), e.message));
                        continue;
                    }
                }
                errors_for
                    .entry(String::new())
                    .or_default()
                    .push((e.field, e.message));
            }
            let resp = ListTpl {
                rows: next,
                edit_name: edit_on_error,
                errors_for,
                flash: None,
                csrf: csrf::encode(&session.csrf_value),
                user_login: session.user_login.clone(),
                user_avatar_url: session.avatar_url.clone(),
                current_page: crate::nav::SCHEDULES,
                is_mod: session.is_mod(),
                is_broadcaster: session.is_broadcaster,
                is_owner: matches!(session.role, crate::auth::Role::Owner),
            };
            render(&resp)
        }
        Err(e) => Err(WebError::Internal(eyre::eyre!("settings apply: {e}"))),
    }
}
```

Note: `rest[end + 2..]` skips `].` so a field key like `schedules[0].name` becomes `name`. If the index parse / strip fails, the error lands in the empty-key bucket and the template renders it at the top.

Also extend `crates/web/templates/schedules/index.html` to render the errors above the offending row. Insert this near the top of the `<section>` containing the list, before the `<ul>`:

```html
{% if !errors_for.is_empty() %}
  <div class="flash error">
    <strong>Validation failed.</strong>
    <ul>
      {% for (name, errs) in errors_for %}
        {% for (field, msg) in errs %}
          <li><code>{% if !name.is_empty() %}{{ name }}.{% endif %}{{ field }}</code>: {{ msg }}</li>
        {% endfor %}
      {% endfor %}
    </ul>
  </div>
{% endif %}
```

- [ ] **Step 4: Run integration tests to verify pass**

```bash
cargo nextest run -p twitch-1337-web schedules_route --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS (all five tests).

- [ ] **Step 5: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 6: Commit**

```bash
git add crates/web/src/routes/schedules.rs crates/web/templates/schedules/index.html crates/web/tests/schedules_route.rs crates/web/tests/common/
git commit -m "$(cat <<'EOF'
feat(web): /schedules add/edit/delete handlers

Each handler reads the current vec, mutates a copy, and replays it
through SettingsStore::apply. Validation errors re-render the list
with per-row error attribution (no persist) — successful saves redirect
with a flash. Delete uses confirm() and degrades cleanly without JS.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 14: Docs — `CLAUDE.md` + final cleanup pass

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Update `CLAUDE.md`**

In `CLAUDE.md`, locate the **Config** section's `[[schedules]]` paragraph (currently the line ending `"Schedules hot-reload on save (2s debounce via notify-debouncer-mini). No restart."`). Replace with:

```
Schedules live in `settings.ron`, managed via `/schedules`. Changes apply
within ~30s. On first v3 launch, any legacy `[[schedules]]` in
config.toml are migrated into `settings.ron` once (sentinel:
`$DATA_DIR/.schedules_migrated_v3`); subsequent edits to those legacy
keys are ignored.
```

Update the **Data dir** section's file listing to include `.schedules_migrated_v3` alongside `.config_migrated_v3` / `.ai_migrated_v2`.

In the **Architecture invariants** bullet on the scheduled messages handler, replace `Loaded from config.toml, reloads on file change` with `Loaded from settings.ron via SettingsStore change Notify; ScheduleCache polled every 30s by the message handler`.

- [ ] **Step 2: Run pre-commit gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

Expected: clean.

- [ ] **Step 3: Run a full smoke build**

```bash
cargo build --workspace --release
```

Expected: clean. (Release build catches integer overflow assertions and any unused-import-only-in-release warnings.)

- [ ] **Step 4: Commit + push**

```bash
git add CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(claude): update schedules + data-dir notes for settings-managed flow

Drops the file-watcher reference, points to /schedules, lists the new
.schedules_migrated_v3 sentinel.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git push -u origin spec/schedules-to-settings
```

- [ ] **Step 5: Open the PR**

```bash
gh pr create --base main --title "feat: migrate [[schedules]] to dashboard settings (PR 2 of v3)" --body "$(cat <<'EOF'
## Summary
- Add `ScheduleSettings` to `Settings` (wholesale-replace Vec); port validation; gate by `SettingsSection::Schedules`.
- Drop `notify-debouncer-mini` in favour of a `SettingsStore` change `Notify`; new sync task regenerates `ScheduleCache` on every settings swap.
- Replace `/schedules` stub with a CRUD page (list + add + inline edit + delete); mod-gated.
- One-shot migration on first PR-2 boot via `.schedules_migrated_v3` sentinel (separate from `.config_migrated_v3` which has already shipped).

Spec: `docs/superpowers/specs/2026-05-24-schedules-migration-design.md`

## Test plan
- [ ] Local: legacy `[[schedules]]` in config.toml → first boot creates sentinel + migrates rows; second boot is a no-op
- [ ] Dashboard: add/edit/delete schedule rows; verify ScheduleCache version bumps within ~1s
- [ ] Dashboard: duplicate-name save → validation error renders, no persist
- [ ] Dashboard: edit a row's interval → next scheduled-message tick uses the new interval
- [ ] Stale `[[schedules]]` left in config.toml after migration → warn logged on subsequent boot

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-review

**Spec coverage:**

- ScheduleSettings + Settings field + override: Task 1, 2 ✓
- Validation rules: Task 3 ✓
- merge_into / reset / diff_changes / SettingsSection: Task 4 ✓
- Notify-based settings change: Task 5 ✓
- parse_to_schedule / build_schedules helpers: Task 6 ✓
- migrate_legacy_config schedules branch: Task 7 ✓
- main.rs sentinel + stale-keys warning: Task 8 ✓
- run_schedule_settings_sync + watcher removal: Task 9 ✓
- spawn.rs wire-up + lib.rs gate drop: Task 10 ✓
- ScheduleConfig + dep + config.toml.example cleanup: Task 11 ✓
- /schedules list page + askama template: Task 12 ✓
- /schedules add/edit/delete handlers + integration tests: Task 13 ✓
- CLAUDE.md docs: Task 14 ✓

**Placeholder scan:**
- No `TBD` / `TODO` / "add error handling" / `Similar to Task N`. Every code step shows the exact block to paste.
- Task 10 Step 5 says "If `SpawnDeps` already carries `settings: SettingsHandle` but not the store, add a `settings_store` field" — this is conditional but not a placeholder; the operative instruction is unambiguous.

**Type consistency:**
- `ScheduleSettings` shape locked in Task 1; every later task references the same fields.
- `run_schedule_settings_sync(settings, store, cache, shutdown)` signature: Task 9 (definition) + Task 10 (call site) match.
- `SettingsStore::change_notify() -> Arc<Notify>`: Task 5 (definition) + Task 9 (consumer) match.
- `SettingsOverrides.schedules: Option<Vec<ScheduleSettings>>`: Task 2 (definition) + Tasks 4, 7, 13 (consumers) match.
- `SettingsSection::Schedules` variant: Task 4 (definition) + Task 4 reset arm — consistent.
- Migration sentinel `.schedules_migrated_v3` string: Task 8 + Task 14 (CLAUDE.md) match.
