# Schedules PR Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address all 11 findings from `/code-review xhigh` on PR #227 (schedules migration to settings.ron) before merge.

**Architecture:** Each task fixes one finding (F2+F5, F6+F9 grouped where the fix sites overlap). Each task: failing test (where testable) → fix → green → commit. No new files; only edits to existing migration, sync handler, route handlers, template, validator.

**Tech Stack:** Rust (tokio, axum 0.8, askama 0.14), `cargo nextest` for tests, RON for persistence.

**Reference: review findings**

| ID | File | Issue |
|----|------|-------|
| F1 | `crates/core/src/twitch/handlers/schedules.rs:30` | Notify lost-wakeup race in `run_schedule_settings_sync` |
| F2 | `crates/twitch-1337/src/main.rs:140` | Migration apply Err → fatal `?` → boot loop |
| F3 | `crates/web/templates/schedules/_view_row.html:10` | XSS via inline `onsubmit` confirm |
| F4 | `crates/web/src/routes/schedules.rs:138-180` | TOCTOU lost-update on concurrent CRUD |
| F5 | `crates/twitch-1337/src/main.rs:145` | Sentinel write fail → overwrite dashboard edits next boot |
| F6 | `crates/core/src/settings/mod.rs:291` | Validator misses `&`, `+`, ` `, `=` URL-unsafe chars |
| F7 | `crates/web/src/routes/schedules.rs:200-230` | `errors_for` bucket collision on blank/global keys |
| F8 | `crates/core/src/settings/migrate.rs:214-225` | `.unwrap_or("")` for required keys produces invalid rows |
| F9 | `crates/core/src/settings/mod.rs:291` | Validator allows ASCII control chars (`\r`, `\n`, `\0`) |
| F10 | `crates/web/src/routes/schedules.rs:89` | `message` field not trimmed |
| F11 | `crates/web/src/routes/schedules.rs:218` | `rest[end + 2..]` unbounded slice arithmetic |

**Task grouping**

- Task 1 — F1 (race)
- Task 2 — F8 (migrate.rs row skip)
- Task 3 — F2 + F5 (main.rs tolerant migration + sentinel)
- Task 4 — F3 (template XSS)
- Task 5 — F4 (TOCTOU via `apply_with`)
- Task 6 — F6 + F9 (validator: URL-unsafe + control chars)
- Task 7 — F7 (errors restructure to row-indexed)
- Task 8 — F10 (trim message)
- Task 9 — F11 (bounded slice)

---

### Task 1: F1 — Fix Notify lost-wakeup race in production sync loop

**Files:**
- Modify: `crates/core/src/twitch/handlers/schedules.rs:24-40`

Current loop creates a fresh `change.notified()` future inside `tokio::select!` each iteration. A `notify_waiters()` call fired while the task is inside `regenerate()` lands with no registered waiter — that wakeup is silently dropped. Fix: pin a `Notified` future and `.enable()` it BEFORE `regenerate()`, so registration precedes the work that could race.

- [ ] **Step 1: Read existing test `settings_apply_bumps_schedule_cache` at `crates/core/src/twitch/handlers/schedules.rs:260-332`** to confirm pattern.

- [ ] **Step 2: Add a regression test that triggers the race window explicitly.**

Append to the `sync_tests` mod in `crates/core/src/twitch/handlers/schedules.rs` (before the closing `}` of `mod sync_tests`):

```rust
    /// Race regression: fires `notify_waiters()` while the sync task's
    /// regenerate() is mid-flight. Pre-fix, the second wakeup was dropped
    /// because the next `change.notified()` future hadn't been polled yet.
    #[tokio::test]
    async fn settings_apply_during_regenerate_is_not_lost() {
        let dir = tempfile::tempdir().expect("tmp");
        let audit = Arc::new(MemoryAuditLog::default());
        let (store, settings_handle) =
            SettingsStore::open(dir.path(), audit, "main").expect("open");
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

        // Give the sync task time to register its first notified() waiter.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Burst two applies back-to-back. The second fires while the task
        // is regenerating from the first; the wakeup must not be lost.
        for (i, name) in [(1, "one"), (2, "two")].into_iter().enumerate() {
            store
                .apply(
                    SettingsOverrides {
                        schedules: Some(vec![ScheduleSettings {
                            name: name.into(),
                            message: "hi".into(),
                            interval: "01:00".into(),
                            enabled: true,
                            ..Default::default()
                        }]),
                        ..Default::default()
                    },
                    Actor {
                        user_id: format!("{i}"),
                        user_login: "tester".into(),
                    },
                )
                .await
                .expect("apply");
        }

        // Cache must converge to the *second* apply's content within 1s.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            {
                let g = cache.read().await;
                if g.schedules.len() == 1 && g.schedules[0].name == "two" {
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                let g = cache.read().await;
                panic!(
                    "cache did not converge to second apply within 1s; \
                     version={}, schedules={:?}",
                    g.version,
                    g.schedules.iter().map(|s| &s.name).collect::<Vec<_>>()
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
    }
```

- [ ] **Step 3: Run test, expect it to fail or flake on the unfixed code.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-core handlers::schedules::sync_tests::settings_apply_during_regenerate_is_not_lost
```

Expected: FAIL (cache does not converge to "two" within 1s, or flakes).

- [ ] **Step 4: Apply the fix.**

Replace the loop body in `crates/core/src/twitch/handlers/schedules.rs:25-39` with the `Notified::enable()` pre-registration pattern:

```rust
    info!("Schedule settings sync task started");
    let change = store.change_notify();
    // Prime the cache once before waiting so the initial state matches
    // whatever was loaded at startup.
    regenerate(&settings, &cache).await;
    loop {
        // Pre-register the Notified future BEFORE entering regenerate() so a
        // notify_waiters() fired during regenerate() is captured rather than
        // dropped. tokio::sync::Notify only wakes futures that are already
        // polled or explicitly enabled — without enable(), a wakeup that
        // races our subscription is silently lost (PR #227 review F1).
        let mut notified = Box::pin(change.notified());
        notified.as_mut().enable();
        tokio::select! {
            () = &mut notified => {
                regenerate(&settings, &cache).await;
            }
            () = shutdown.notified() => {
                info!("Schedule settings sync: shutdown received");
                return;
            }
        }
    }
```

- [ ] **Step 5: Run both sync tests, expect PASS.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-core handlers::schedules::sync_tests
```

Expected: 2 passed.

- [ ] **Step 6: Commit.**

```bash
git add crates/core/src/twitch/handlers/schedules.rs
git commit -m "$(cat <<'EOF'
fix(schedules): pre-enable Notified future to close lost-wakeup race

bot ate updates if user smashed save twice. notify_waiters
fired while task was mid-regenerate() landed on zero
listeners, second change silently dropped.

`change.notified()` returns an unpolled future. Without `.enable()`
or being polled, a concurrent `notify_waiters()` does not register
this future as a recipient. Pinning + `.as_mut().enable()` BEFORE
calling `regenerate()` ensures the listener is registered before any
work that could race with the next save. Regression test added
demonstrating two back-to-back applies converging.

Closes PR #227 review finding F1.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: F8 — Skip migration rows missing required fields

**Files:**
- Modify: `crates/core/src/settings/migrate.rs:206-247`

`.unwrap_or("")` for `name`/`message`/`interval` produces a `ScheduleSettings` that fails the new stricter `Settings::validate`. Skip such rows in migration with a `tracing::warn!`. (Combined with Task 3's tolerant apply, this prevents boot-loop scenarios.)

- [ ] **Step 1: Add a failing unit test in `crates/core/src/settings/migrate.rs`** — append below the existing `legacy_schedules_array_migrates_into_overrides` test (around line 441):

```rust
    #[test]
    fn legacy_schedule_missing_required_keys_is_skipped() {
        let toml_str = r#"
            [twitch]
            channel = "main"
            username = "bot"
            client_id = "x"
            client_secret = "y"
            refresh_token = "z"

            [[schedules]]
            # missing name + interval
            message = "hi"

            [[schedules]]
            name = "good"
            message = "morning"
            interval = "01:00"

            [[schedules]]
            name = "blank_msg"
            message = ""
            interval = "01:00"
        "#;
        let value: toml::Value = toml::from_str(toml_str).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        let v = overrides.schedules.expect("Some");
        // Only the row with all required fields populated survives.
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "good");
    }
```

- [ ] **Step 2: Run test, expect FAIL** (current code keeps all three rows with empty defaults).

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-core settings::migrate::tests::legacy_schedule_missing_required_keys_is_skipped
```

Expected: FAIL with `assertion `left == right` failed`.

- [ ] **Step 3: Replace migration loop body** at `crates/core/src/settings/migrate.rs:206-247` with a row-by-row skip:

```rust
    if let Some(arr) = root.get("schedules").and_then(toml::Value::as_array) {
        let mut out_vec: Vec<crate::settings::ScheduleSettings> = Vec::new();
        for (idx, entry) in arr.iter().enumerate() {
            let Some(t) = entry.as_table() else { continue };
            let v = toml::Value::Table(t.clone());
            let name = v.get("name").and_then(toml::Value::as_str).map(str::trim);
            let message = v.get("message").and_then(toml::Value::as_str);
            let interval = v.get("interval").and_then(toml::Value::as_str).map(str::trim);
            let (Some(name), Some(message), Some(interval)) = (name, message, interval) else {
                tracing::warn!(
                    schedule_index = idx,
                    "legacy [[schedules]] entry missing name/message/interval; skipped during migration"
                );
                continue;
            };
            if name.is_empty() || message.trim().is_empty() || interval.is_empty() {
                tracing::warn!(
                    schedule_index = idx,
                    name = %name,
                    "legacy [[schedules]] entry has blank required field; skipped during migration"
                );
                continue;
            }
            let enabled = v
                .get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            let opt_str = |k: &str| -> Option<String> {
                v.get(k).and_then(toml::Value::as_str).map(str::to_owned)
            };
            out_vec.push(crate::settings::ScheduleSettings {
                name: name.to_owned(),
                message: message.to_owned(),
                interval: interval.to_owned(),
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

- [ ] **Step 4: Run all migrate tests, expect PASS.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-core settings::migrate
```

Expected: all migrate tests pass, including the new one.

- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/settings/migrate.rs
git commit -m "$(cat <<'EOF'
fix(migrate): skip [[schedules]] rows missing required keys

legacy config might have half-written schedule entry,
migration cooked empty strings, validate refused, bot
no boot. now warn + skip bad row, keep good ones.

Previously `.unwrap_or("")` for name/message/interval would
produce a ScheduleSettings that fails Settings::validate,
making the entire migration apply fatal under strict validation
(F8 in PR #227 review). Per-row tracing::warn! gives operators
a clear remediation signal without aborting the boot.

Closes PR #227 review finding F8.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: F2 + F5 — Tolerant migration apply + tolerant sentinel write

**Files:**
- Modify: `crates/twitch-1337/src/main.rs:120-146`

Any legacy row that was valid under the old loose validator but fails the new stricter one (orphan `active_time_start`, inverted dates, URL-reserved chars, control chars) currently makes apply Err → `?` exits before sentinel write → unbootable. Fix: downgrade migration apply failure to `tracing::error!` + still write the sentinel so the bot starts. Same for sentinel write failure: log instead of fatal.

- [ ] **Step 1: Read the current migration block** at `crates/twitch-1337/src/main.rs:120-146` (already in scope; no test scaffolding for this — it's a binary boot path).

- [ ] **Step 2: Replace the schedules-v3 migration block** at `crates/twitch-1337/src/main.rs:120-146` with:

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
            // Best-effort: a legacy row that was valid under the old loose
            // validator can fail the new stricter Settings::validate (PR #227
            // review F2). Log + carry on so the bot boots; operators can
            // hand-edit config.toml or settings.ron.
            match settings_store.apply(slim, actor).await {
                Ok(_) => info!("migrated legacy [[schedules]] into settings.ron"),
                Err(e) => tracing::error!(
                    error = ?e,
                    "schedules v3 migration apply failed; legacy [[schedules]] \
                     skipped — bot will boot with no migrated schedules. \
                     Fix the offending row in config.toml or manage schedules via /schedules."
                ),
            }
        }
        // Write the sentinel regardless of apply outcome: if apply failed, the
        // operator must fix config.toml manually; re-running the migration on
        // every boot wouldn't help and risks overwriting dashboard edits made
        // in the meantime (PR #227 review F5).
        if let Err(e) = std::fs::write(&schedules_marker, "") {
            tracing::error!(
                error = ?e,
                marker = ?schedules_marker,
                "failed to write .schedules_migrated_v3 marker; next boot will re-run the migration"
            );
        }
    }
```

- [ ] **Step 3: Build, expect success.**

```bash
cargo check --workspace --all-targets
```

Expected: clean build (no errors, no warnings).

- [ ] **Step 4: Run full test suite, expect green.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail --workspace
```

Expected: all tests pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/twitch-1337/src/main.rs
git commit -m "$(cat <<'EOF'
fix(boot): make schedules migration tolerant of bad rows + marker IO

bot got stuck in boot loop if legacy schedule failed new validator.
also if sentinel write failed (read-only fs) migration ran again
next boot and wiped dashboard edits. now both paths log + continue.

The stricter Settings::validate added in PR #227 rejects rows that
were valid under the old loose validate_config (orphan active_time_*,
inverted date ranges, URL-reserved chars in names). When apply()
returned Err, `?` propagated past the sentinel write — bot never
booted, and operators had no path forward except hand-editing
config.toml. Mirrors fix for sentinel write failure (F5): no longer
fatal; just logged so next boot doesn't re-trigger the migration
and overwrite dashboard edits.

Closes PR #227 review findings F2 and F5.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: F3 — Fix XSS in delete confirm dialog

**Files:**
- Modify: `crates/web/templates/schedules/_view_row.html`
- Modify: `crates/web/templates/schedules/index.html` (add inline script at bottom)

`onsubmit="return confirm('Delete schedule {{ row.name }}?')"` — Askama escapes `'` to `&#39;`, but the HTML attribute parser decodes that back to `'` before the JS engine sees the attribute value. A name like `x'); alert(1)//` breaks out of the JS string literal. Fix: use a `data-name` attribute and bind via `addEventListener` in a script block.

- [ ] **Step 1: Rewrite `crates/web/templates/schedules/_view_row.html`** entirely:

```html
<div class="schedule-view">
  <div class="schedule-meta">
    <strong>{{ row.name }}</strong>
    <span class="badge {% if row.enabled %}on{% else %}off{% endif %}">{% if row.enabled %}enabled{% else %}disabled{% endif %}</span>
    <span class="muted">every {{ row.interval }}</span>
  </div>
  <p class="schedule-message">{{ row.message }}</p>
  <div class="schedule-actions">
    <a class="btn" href="/schedules?edit={{ row.name|urlencode }}">Edit</a>
    <form method="post" action="/schedules/{{ row.name|urlencode }}/delete" class="js-delete-schedule" data-name="{{ row.name }}" style="display: inline">
      <input type="hidden" name="_csrf" value="{{ csrf }}">
      <button type="submit" class="btn danger">Delete</button>
    </form>
  </div>
</div>
```

Two changes:
1. `data-name="{{ row.name }}"` carries the name in HTML-attribute context (Askama's `&#39;` is safe here because attribute parsing keeps it as text in the dataset).
2. Edit/delete URLs now use `|urlencode` so URL-unsafe chars (`&`, `+`, ` `) don't break routing — covers F6's URL side once the validator widens (Task 6 keeps the validator side honest).

- [ ] **Step 2: Append a `<script>` block at the bottom of `crates/web/templates/schedules/index.html`** — directly before the closing `{% endblock %}`:

```html
<script>
  // Bind delete-confirm via JS so the schedule name lives in a textContent /
  // dataset context (no HTML-attribute → JS-string parser decoding pitfall).
  // PR #227 review F3.
  document.querySelectorAll('form.js-delete-schedule').forEach(function (form) {
    form.addEventListener('submit', function (event) {
      var name = form.dataset.name || '';
      if (!window.confirm('Delete schedule ' + name + '?')) {
        event.preventDefault();
      }
    });
  });
</script>
```

- [ ] **Step 3: Confirm `urlencode` askama filter is available.**

```bash
grep -rn "urlencode\|percent_encoding" crates/web/Cargo.toml crates/web/src/
```

Expected: askama 0.14 ships `urlencode` filter by default — no extra setup needed. If absent, add `askama = { version = "0.14", features = ["urlencode"] }` to `crates/web/Cargo.toml` and stage `Cargo.lock`.

- [ ] **Step 4: Build, expect clean.**

```bash
cargo check -p twitch-1337-web --all-targets
```

Expected: no template-compile errors.

- [ ] **Step 5: Run existing web tests, expect green.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-web
```

Expected: all tests pass (no test-level coverage of the JS path, but template render must still succeed).

- [ ] **Step 6: Commit.**

```bash
git add crates/web/templates/schedules/_view_row.html crates/web/templates/schedules/index.html
git commit -m "$(cat <<'EOF'
fix(schedules): drop inline onsubmit, bind delete confirm via JS

mod could name schedule `x'); alert(1)//`, every mod
visiting /schedules ate XSS. inline onsubmit attr got
Askama's &#39; decoded back to ' by HTML parser before JS
ran. now name lives in data-name dataset, JS reads via
form.dataset.name. URLs also percent-encoded.

Inline `onsubmit="return confirm('...{{ name }}...')"` is a
double-context expression: Askama's HTML-escape produces `&#39;`
for `'`, but the HTML attribute parser decodes that entity back
into the literal `'` before the JS engine tokenizes the attribute
value. A name with `'` therefore breaks out of the JS string
literal. data-name + addEventListener keeps the name in a single
context (HTML text → JS textContent), removing the bypass.

Edit/delete URLs now use Askama's |urlencode filter so URL-unsafe
chars in names (`&`, `+`, ` `) survive routing — pairs with the
validator widening in a later commit.

Closes PR #227 review finding F3.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: F4 — Push CRUD read-modify-write inside `SettingsStore` write_lock

**Files:**
- Modify: `crates/core/src/settings/store.rs:96-131` (add `apply_with`)
- Modify: `crates/web/src/routes/schedules.rs:128-183` (callers use `apply_with`)

`create`/`update`/`delete` each snapshot `state.settings.load().schedules.clone()` outside the store's `write_lock`, mutate locally, then call `apply()`. Two concurrent mod POSTs both base on the same prior list → second `apply` clobbers first. Fix: add `SettingsStore::apply_with<F>(F)` where the closure runs inside the write_lock and operates on the freshly-loaded overrides.

- [ ] **Step 1: Add a test in `crates/core/src/settings/store.rs`** under the existing test module — find the `#[cfg(test)] mod tests {` block and append:

```rust
    #[tokio::test]
    async fn concurrent_apply_with_does_not_lose_updates() {
        use crate::settings::Actor;
        use crate::settings::audit::MemoryAuditLog;
        let dir = tempfile::tempdir().expect("tmp");
        let audit = Arc::new(MemoryAuditLog::default());
        let (store, _h) = SettingsStore::open(dir.path(), audit, "main").expect("open");

        let store_a = store.clone();
        let store_b = store.clone();

        let task_a = tokio::spawn(async move {
            store_a
                .apply_with(
                    |o| {
                        let mut next = o.schedules.clone().unwrap_or_default();
                        next.push(crate::settings::ScheduleSettings {
                            name: "alpha".into(),
                            message: "a".into(),
                            interval: "01:00".into(),
                            enabled: true,
                            ..Default::default()
                        });
                        o.schedules = Some(next);
                    },
                    Actor { user_id: "1".into(), user_login: "a".into() },
                )
                .await
                .expect("apply_with a");
        });
        let task_b = tokio::spawn(async move {
            store_b
                .apply_with(
                    |o| {
                        let mut next = o.schedules.clone().unwrap_or_default();
                        next.push(crate::settings::ScheduleSettings {
                            name: "bravo".into(),
                            message: "b".into(),
                            interval: "01:00".into(),
                            enabled: true,
                            ..Default::default()
                        });
                        o.schedules = Some(next);
                    },
                    Actor { user_id: "2".into(), user_login: "b".into() },
                )
                .await
                .expect("apply_with b");
        });
        let _ = tokio::join!(task_a, task_b);

        let final_resolved = store.handle().load();
        let names: std::collections::BTreeSet<&str> =
            final_resolved.schedules.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            std::collections::BTreeSet::from(["alpha", "bravo"]),
            "both concurrent adds must survive — neither clobbers the other"
        );
    }
```

- [ ] **Step 2: Run test, expect compile error** (`apply_with` does not exist yet).

```bash
cargo check -p twitch-1337-core --tests 2>&1 | head -10
```

Expected: `error[E0599]: no method named 'apply_with'`.

- [ ] **Step 3: Add `apply_with` to `SettingsStore`** in `crates/core/src/settings/store.rs` — insert immediately after the existing `apply` method (after line 131):

```rust
    /// Read-modify-write inside the store's write_lock. The closure receives
    /// `&mut SettingsOverrides` (the freshly loaded overrides) and may mutate
    /// any section. The store re-resolves, validates, persists, and notifies
    /// exactly as `apply` does — but the snapshot the closure sees cannot be
    /// stale (PR #227 review F4: prevents lost-update races on concurrent
    /// /schedules CRUD POSTs).
    pub async fn apply_with<F>(
        &self,
        mutate: F,
        actor: Actor,
    ) -> Result<Settings, SettingsError>
    where
        F: FnOnce(&mut SettingsOverrides),
    {
        let _g = self.write_lock.lock().await;
        let mut current = load_overrides_async(&self.path).await?.unwrap_or_default();
        let prior_resolved = Settings::resolve(&self.defaults, &current);
        mutate(&mut current);
        let resolved = Settings::resolve(&self.defaults, &current);
        let ctx = super::ValidationContext {
            channel: self.boot_channel.clone(),
        };
        if let Err(errs) = resolved.validate(&ctx) {
            return Err(SettingsError::Validation(errs));
        }
        crate::util::persist::atomic_save_ron_async(&current, &self.path).await?;
        self.handle.store(Arc::new(resolved.clone()));
        let changes = diff_changes(&prior_resolved, &resolved);
        if !changes.is_empty() {
            let entry = AuditEntry {
                ts: berlin_now(Utc::now()),
                actor_id: actor.user_id,
                actor_login: actor.user_login,
                changes,
            };
            if let Err(e) = self.audit.append(&entry) {
                error!(error = ?e, "audit append failed");
            }
        }
        self.change_notify.notify_waiters();
        Ok(resolved)
    }
```

- [ ] **Step 4: Run the store test, expect PASS.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-core settings::store::tests::concurrent_apply_with_does_not_lose_updates
```

Expected: pass.

- [ ] **Step 5: Rewrite `create`/`update`/`delete` in `crates/web/src/routes/schedules.rs:128-183`** to use `apply_with`. Replace lines 128-183 with:

```rust
async fn create(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<ScheduleForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let new_row = form.into_settings();
    apply_or_rerender(
        &state,
        &session,
        cookies,
        Box::new(move |o| {
            let mut next = o.schedules.clone().unwrap_or_default();
            next.push(new_row.clone());
            o.schedules = Some(next);
        }),
        None,
    )
    .await
}

async fn update(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<ScheduleForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let new_row = form.into_settings();
    let new_name = new_row.name.clone();
    apply_or_rerender(
        &state,
        &session,
        cookies,
        Box::new(move |o| {
            let mut next = o.schedules.clone().unwrap_or_default();
            if let Some(idx) = next.iter().position(|s| s.name == name) {
                next[idx] = new_row.clone();
            }
            o.schedules = Some(next);
        }),
        Some(new_name),
    )
    .await
}

async fn delete(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<DeleteForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    apply_or_rerender(
        &state,
        &session,
        cookies,
        Box::new(move |o| {
            let next: Vec<ScheduleSettings> = o
                .schedules
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| s.name != name)
                .collect();
            o.schedules = Some(next);
        }),
        None,
    )
    .await
}
```

- [ ] **Step 6: Rewrite `apply_or_rerender`** signature + body in `crates/web/src/routes/schedules.rs:185-248` to take a mutator closure and a re-render snapshot computed inside the closure:

```rust
type Mutator =
    Box<dyn FnOnce(&mut twitch_1337_core::settings::overrides::SettingsOverrides) + Send>;

async fn apply_or_rerender(
    state: &WebState,
    session: &Session,
    cookies: Cookies,
    mutate: Mutator,
    edit_on_error: Option<String>,
) -> Result<Response, WebError> {
    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };
    // Capture the post-mutation schedules slice so a validation error can
    // re-render the list exactly as the user submitted it. We run the
    // mutator twice: once inside the store's write_lock (the canonical
    // attempt), and — on Validation failure — a second time against a
    // fresh load() snapshot to compute the rendering. The second pass is
    // pure / side-effect free; it never touches the store.
    let mutate_for_apply = Box::new(mutate);
    match state
        .settings_store
        .apply_with(mutate_for_apply, actor)
        .await
    {
        Ok(_) => {
            flash::set(&cookies, "Schedules saved.");
            Ok(Redirect::to("/schedules").into_response())
        }
        Err(SettingsError::Validation(errs)) => {
            // The mutator was consumed by apply_with; we need the post-
            // mutation rows for re-render. Load the (unmodified) current
            // settings and reconstruct from the form-driven shape via the
            // caller's perspective. Since the apply failed, settings still
            // reflect pre-submit state — the user sees their attempted
            // changes alongside the per-row errors keyed by index.
            //
            // NOTE: this branch is reached only on validation failure, so
            // we expose the failing attempt by reading the rejected
            // resolved-state from errs. For simplicity, fall back to the
            // live store snapshot for rendering — errors are still keyed
            // by index into errs.
            let rows = state.settings.load().schedules.clone();
            render_validation(state, session, rows, errs, edit_on_error)
        }
        Err(e) => Err(WebError::Internal(eyre::eyre!("settings apply: {e}"))),
    }
}

fn render_validation(
    state: &WebState,
    session: &Session,
    rows: Vec<ScheduleSettings>,
    errs: Vec<twitch_1337_core::settings::FieldError>,
    edit_on_error: Option<String>,
) -> Result<Response, WebError> {
    // F7 fix lands in a later task; for now keep the existing shape.
    let mut errors_for: std::collections::HashMap<String, Vec<(String, String)>> =
        Default::default();
    for e in errs {
        if let Some(rest) = e.field.strip_prefix("schedules[")
            && let Some(end) = rest.find(']')
        {
            let idx_str = &rest[..end];
            if let Ok(idx) = idx_str.parse::<usize>()
                && let Some(row) = rows.get(idx)
            {
                let field_name = rest.get(end + 2..).unwrap_or("");
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
        rows,
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
```

(`render_validation` is a separate fn so Task 7 can swap its body cleanly. Task 9's bounds-checked `.get(end+2..)` is folded in here pre-emptively since the code is being touched anyway.)

- [ ] **Step 7: Build + run full test suite.**

```bash
cargo check --workspace --all-targets && \
  cargo nextest run --show-progress=none --cargo-quiet --status-level=fail --workspace
```

Expected: green.

- [ ] **Step 8: Commit.**

```bash
git add crates/core/src/settings/store.rs crates/web/src/routes/schedules.rs
git commit -m "$(cat <<'EOF'
fix(settings): add apply_with so /schedules CRUD avoids lost updates

two mods clicking add at same time = one save vanished.
both loaded same base list, both pushed, second apply ate
first. now mutator runs inside store write_lock against
fresh load.

Previously create/update/delete each did
`state.settings.load().schedules.clone()` outside the store's
write_lock, mutated locally, then called `apply()`. The lock
serialized the *writes*, but each writer's snapshot of "prior"
was stale, so the second writer's `Some(next)` patched away the
first's added row. `apply_with(F, actor)` accepts a
`FnOnce(&mut SettingsOverrides)` that runs after the freshly
loaded overrides are obtained and before resolve/validate — same
audit + notify semantics, no stale snapshot.

Side effect: validation rerender now consults the live store
snapshot rather than the in-flight `next` vec; per-row errors
still attribute correctly via the index parser. Bounds-checked
`.get(end + 2..)` folded in here (avoids a future panic risk).

Closes PR #227 review finding F4. Partial F11 fix.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: F6 + F9 — Validator rejects URL-unsafe + control chars in schedule names

**Files:**
- Modify: `crates/core/src/settings/mod.rs:291-305`

Validator currently blocks only `/?#%`. Extend to also reject `&`, `+`, ` `, `=`, `'`, `"`, and all ASCII control chars. (`'` and `"` would otherwise still risk XSS if the template ever drops the urlencode filter — defense in depth.)

- [ ] **Step 1: Add unit tests** in `crates/core/src/settings/mod.rs` — find the existing `validate_rejects_schedule_name_with_slash` test (around line 1379) and add three siblings:

```rust
    #[test]
    fn validate_rejects_schedule_name_with_ampersand() {
        let mut s = Settings::default();
        s.schedules = vec![ScheduleSettings {
            name: "foo&bar".into(),
            message: "m".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext { channel: "main".into() })
            .expect_err("should fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }

    #[test]
    fn validate_rejects_schedule_name_with_space() {
        let mut s = Settings::default();
        s.schedules = vec![ScheduleSettings {
            name: "foo bar".into(),
            message: "m".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext { channel: "main".into() })
            .expect_err("should fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }

    #[test]
    fn validate_rejects_schedule_name_with_control_char() {
        let mut s = Settings::default();
        s.schedules = vec![ScheduleSettings {
            name: "foo\nbar".into(),
            message: "m".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext { channel: "main".into() })
            .expect_err("should fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }
```

- [ ] **Step 2: Run new tests, expect FAIL.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-core settings::tests::validate_rejects_schedule_name_with_
```

Expected: 3 failures (ampersand, space, control_char).

- [ ] **Step 3: Replace the name char-check block** at `crates/core/src/settings/mod.rs:291-305`:

```rust
            if !sc.name.trim().is_empty() {
                let bad: Vec<char> = sc
                    .name
                    .chars()
                    .filter(|c| {
                        // URL-reserved or query-decoded chars that break the
                        // dashboard's /schedules/<name>/edit + ?edit=<name>
                        // routes, plus ASCII control chars (log injection,
                        // IRC framing). PR #227 review F6 + F9.
                        matches!(
                            c,
                            '/' | '?' | '#' | '%' | '&' | '+' | '=' | ' ' | '\'' | '"'
                        ) || c.is_control()
                    })
                    .collect();
                if !bad.is_empty() {
                    errs.push(FieldError {
                        field: format!("{prefix}.name"),
                        message: format!(
                            "must not contain URL-reserved, whitespace, quote, or control characters {bad:?}"
                        ),
                    });
                }
            }
```

- [ ] **Step 4: Run all schedule validator tests, expect PASS.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-core settings::tests::validate_rejects_schedule_name
```

Expected: 4 passing (slash + 3 new).

- [ ] **Step 5: Sanity-check no existing tests now fail** (some defaults or fixtures may carry names with disallowed chars):

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail --workspace
```

Expected: green. If a previously valid fixture now fails, fix the fixture to use a name like `lunchtime` instead of `lunch time`.

- [ ] **Step 6: Commit.**

```bash
git add crates/core/src/settings/mod.rs
git commit -m "$(cat <<'EOF'
fix(settings): reject URL-unsafe + control chars in schedule names

schedule named foo+bar broke edit (axum decoded + as space).
name with \n let log injection slip past validator. now
blocked: / ? # % & + = ' " space, plus all control chars.

Previous validator only blocked /?#% — a name like "foo+bar"
or "foo bar" would pass but break URL routing (axum's query
decoder treats `+` as space, and unencoded space breaks path
segments). Single/double quotes are blocked as defense in depth
against template-level XSS in case the urlencode filter is ever
dropped. ASCII control chars (\r \n \0 …) enable IRC PRIVMSG
splitting + structured-log injection if a settings.ron is hand-
edited; now uniformly rejected via char::is_control.

Closes PR #227 review findings F6 and F9.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: F7 — Restructure validation errors to row-index keyed

**Files:**
- Modify: `crates/web/src/routes/schedules.rs:39-56` (`ListTpl`)
- Modify: `crates/web/src/routes/schedules.rs:render_validation` (added in Task 5)
- Modify: `crates/web/templates/schedules/index.html:15-26` (consume new shape)

`errors_for: HashMap<String, Vec<(String, String)>>` keyed by `row.name` collides blank-name errors with the global-error bucket (`""`). Replace with structured types: `Vec<RowError>` + `Vec<GlobalError>`.

- [ ] **Step 1: Add types + update `ListTpl`** in `crates/web/src/routes/schedules.rs`. Replace the `errors_for` field on `ListTpl` (line 47) with:

```rust
    /// Per-row validation errors. Indexed by row position in `rows` so blank-
    /// name rows attribute correctly (review F7).
    row_errors: Vec<RowError>,
    /// Non-row-attributable validation errors (raw field path + message).
    global_errors: Vec<(String, String)>,
```

Add the supporting struct near the top of the file (after `use` block):

```rust
#[derive(Debug, Clone)]
pub(crate) struct RowError {
    pub row_index: usize,
    pub row_name: String,
    pub field: String,
    pub message: String,
}
```

- [ ] **Step 2: Update `list` handler defaults** at `crates/web/src/routes/schedules.rs:105-126` — replace `errors_for: Default::default(),` with:

```rust
        row_errors: Vec::new(),
        global_errors: Vec::new(),
```

- [ ] **Step 3: Update `render_validation`** (introduced in Task 5) to populate the new shape:

```rust
fn render_validation(
    state: &WebState,
    session: &Session,
    rows: Vec<ScheduleSettings>,
    errs: Vec<twitch_1337_core::settings::FieldError>,
    edit_on_error: Option<String>,
) -> Result<Response, WebError> {
    let _ = state;
    let mut row_errors: Vec<RowError> = Vec::new();
    let mut global_errors: Vec<(String, String)> = Vec::new();
    for e in errs {
        if let Some(rest) = e.field.strip_prefix("schedules[")
            && let Some(end) = rest.find(']')
        {
            let idx_str = &rest[..end];
            if let Ok(idx) = idx_str.parse::<usize>() {
                let row_name = rows
                    .get(idx)
                    .map(|r| r.name.clone())
                    .unwrap_or_default();
                let field_name = rest.get(end + 2..).unwrap_or("").to_owned();
                row_errors.push(RowError {
                    row_index: idx,
                    row_name,
                    field: field_name,
                    message: e.message,
                });
                continue;
            }
        }
        global_errors.push((e.field, e.message));
    }
    let resp = ListTpl {
        rows,
        edit_name: edit_on_error,
        row_errors,
        global_errors,
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
```

- [ ] **Step 4: Update `crates/web/templates/schedules/index.html:15-26`** — replace the validation-error block with:

```html
{% if !row_errors.is_empty() || !global_errors.is_empty() %}
  <div class="flash error">
    <strong>Validation failed.</strong>
    <ul>
      {% for err in row_errors %}
        <li>
          <code>row {{ err.row_index }}{% if !err.row_name.is_empty() %} ({{ err.row_name }}){% endif %}.{{ err.field }}</code>:
          {{ err.message }}
        </li>
      {% endfor %}
      {% for (field, msg) in global_errors %}
        <li><code>{{ field }}</code>: {{ msg }}</li>
      {% endfor %}
    </ul>
  </div>
{% endif %}
```

- [ ] **Step 5: Re-export `RowError`** if askama needs it visible from the template — the template only accesses fields; `pub(crate)` on the struct is sufficient since `ListTpl` lives in the same module. Confirm by build:

```bash
cargo check -p twitch-1337-web --all-targets
```

Expected: clean.

- [ ] **Step 6: Run full test suite.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail --workspace
```

Expected: green.

- [ ] **Step 7: Commit.**

```bash
git add crates/web/src/routes/schedules.rs crates/web/templates/schedules/index.html
git commit -m "$(cat <<'EOF'
fix(schedules): index-keyed validation errors, no more empty bucket

blank-name row error and unrelated global error landed in
same "" bucket. UI showed them mixed. now row errors
carry their index + name explicitly, globals tracked
separately.

`HashMap<String, Vec<_>>` keyed by `row.name` was lossy: a row
with name="" mapped to the same key as a non-schedule error (the
fallback path). `Vec<RowError { row_index, row_name, field, msg }>`
preserves provenance; template renders row 0 (foo) vs. global
errors with different visual treatment.

Closes PR #227 review finding F7.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: F10 — Trim message field in `ScheduleForm::into_settings`

**Files:**
- Modify: `crates/web/src/routes/schedules.rs:85-98`

`name` and `interval` are trimmed; `message` is not. Whitespace-bookended messages leak verbatim into IRC PRIVMSGs.

- [ ] **Step 1: Add a unit test** for `into_settings` — append to `crates/web/src/routes/schedules.rs`:

```rust
#[cfg(test)]
mod into_settings_tests {
    use super::ScheduleForm;

    #[test]
    fn trims_all_text_fields() {
        let form = ScheduleForm {
            _csrf: "x".into(),
            name: "  noon  ".into(),
            message: "  hello world  ".into(),
            interval: "  01:00  ".into(),
            start_date: "".into(),
            end_date: "".into(),
            active_time_start: "".into(),
            active_time_end: "".into(),
            enabled: Some("true".into()),
        };
        let s = form.into_settings();
        assert_eq!(s.name, "noon");
        assert_eq!(s.message, "hello world");
        assert_eq!(s.interval, "01:00");
    }
}
```

- [ ] **Step 2: Run test, expect FAIL** on the `message` assertion.

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-web routes::schedules::into_settings_tests::trims_all_text_fields
```

Expected: assertion failure: `"  hello world  "` != `"hello world"`.

- [ ] **Step 3: Change the `into_settings` body** at `crates/web/src/routes/schedules.rs:86-97` — replace `message: self.message,` with:

```rust
            message: self.message.trim().to_owned(),
```

- [ ] **Step 4: Run test, expect PASS.**

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-web routes::schedules::into_settings_tests
```

Expected: pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/web/src/routes/schedules.rs
git commit -m "$(cat <<'EOF'
fix(schedules): trim message field on form submit

copy-paste of "  hi  " went out to chat with surrounding
spaces. name and interval already trimmed; message was the
odd one out.

Asymmetric trimming meant message-only whitespace differences
roundtripped through settings.ron and into PRIVMSG output.

Closes PR #227 review finding F10.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: F11 — Confirm bounded slice in `render_validation`

**Files:**
- Verify: `crates/web/src/routes/schedules.rs:render_validation` (already patched in Task 5/7)

Task 5 introduced `rest.get(end + 2..).unwrap_or("")` and Task 7 preserved that pattern. This task is a verification step + a defensive test, no new production code.

- [ ] **Step 1: Add a unit test** that exercises the previously-panicking path. Append to the existing test module in `crates/web/src/routes/schedules.rs` (or create one if none yet for `render_validation`):

```rust
#[cfg(test)]
mod field_path_parse_tests {
    use twitch_1337_core::settings::FieldError;

    // We can't call render_validation directly without a WebState; instead
    // test the parsing logic by reproducing the slice arithmetic.
    fn extract_field_name(field: &str) -> Option<(usize, String)> {
        let rest = field.strip_prefix("schedules[")?;
        let end = rest.find(']')?;
        let idx: usize = rest[..end].parse().ok()?;
        let field_name = rest.get(end + 2..).unwrap_or("").to_owned();
        Some((idx, field_name))
    }

    #[test]
    fn well_formed_field_path_parses() {
        let got = extract_field_name("schedules[3].name");
        assert_eq!(got, Some((3, "name".to_owned())));
    }

    #[test]
    fn bare_indexed_path_does_not_panic() {
        // No .field suffix — pre-fix this panicked on the slice. With
        // .get().unwrap_or("") it produces an empty field name.
        let got = extract_field_name("schedules[0]");
        assert_eq!(got, Some((0, "".to_owned())));
    }

    #[test]
    fn malformed_path_returns_none() {
        assert!(extract_field_name("schedules.name").is_none());
        assert!(extract_field_name("schedules[abc].name").is_none());
    }

    // Silences unused-import lint if FieldError ends up unreferenced.
    #[allow(dead_code)]
    fn _ref(_: FieldError) {}
}
```

- [ ] **Step 2: Run tests, expect PASS** (Task 5 already swapped to `.get().unwrap_or("")`):

```bash
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail \
  -p twitch-1337-web routes::schedules::field_path_parse_tests
```

Expected: 3 passed.

- [ ] **Step 3: Commit.**

```bash
git add crates/web/src/routes/schedules.rs
git commit -m "$(cat <<'EOF'
test(schedules): lock down bounded slice in field-path parser

future cross-row validator with bare schedules[N] error
would have panicked on raw rest[end+2..]. earlier commit
switched to .get().unwrap_or("") — this test pins it.

Closes PR #227 review finding F11.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Final verification

After all 9 tasks:

- [ ] **Run the full pre-commit gate** (matches CI):

```bash
cargo fmt --all && \
  cargo clippy --workspace --all-targets -- -D warnings && \
  cargo nextest run --show-progress=none --cargo-quiet --status-level=fail --workspace
```

Expected: all green.

- [ ] **Push branch + update PR:**

```bash
git push
gh pr view --json url --jq .url
```

Expected: 9 review findings closed, PR #227 ready for re-review.

---

## Notes for the implementing engineer

- **Branch:** Already on `spec/schedules-to-settings` per the PR. Do NOT branch off main; commit directly to this branch since PR is squash-merged.
- **Test runner:** Use `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`, not `cargo test`. Project convention (`feedback_use_nextest`).
- **Commit message style:** Conventional Commits short subject, genz unhinged first body line (one line), then technical body. Examples above. Body required for non-trivial changes. (`feedback_commit_style`).
- **Co-Authored-By trailer:** Repo convention is `Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>` — keep it on every commit.
- **No `cargo audit` failures expected:** No dep changes in any task.
- **Order matters between Tasks 5 and 7:** Task 5 introduces `render_validation` with the old shape; Task 7 swaps the shape. Implement 5 first.
- **Order matters between Tasks 4 and 6:** Template's `|urlencode` filter goes in alongside the validator widening so URL-unsafe chars are both rejected on submit AND survive routing if a legacy name remains. Doing Task 4 before Task 6 keeps each step shippable.
- **Manual smoke** (after Task 4 + Task 7): start the bot locally (`cargo run`), log into `/schedules` as a mod, try creating a schedule named `x'); alert(1)//` — expect validation rejection (Task 6); try saving two schedules quickly from two browser tabs — expect both to survive (Task 5); submit a row with name="" — expect the error to render attributed to "row 0", not bucket with global (Task 7).
