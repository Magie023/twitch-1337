# Schedules redesign

Status: design
Date: 2026-05-28

## Goal

Rebuild the schedules feature on a typed trigger model that supports
weekday-anchored firing (e.g. "every Thursday at 20:00") alongside the
existing interval trigger, with wall-clock alignment, drift-free
execution, immediate reaction to settings saves, and a card-grid
dashboard with per-schedule runtime telemetry.

In one pass this also collapses the duplicate validation layer, deletes
the `ScheduleCache` indirection, fixes the 30s orchestrator poll gap,
and removes the implicit-Berlin string-typed date fields.

Out of scope (deferred): bulk import/export of schedules.

## Pain points addressed

1. **No way to schedule "every Thursday at 20:00"** — interval-only
   model. Also no way to restrict Interval triggers to specific
   weekdays (e.g. "every 30 min, Tue/Thu nights").
2. **Two validation layers** (`Settings::validate` + `database::Schedule::validate`) with partial overlap.
3. **30s orchestrator poll** — saves take up to 30s to take effect even though `SettingsStore::change_notify` already fires immediately.
4. **No wall-clock alignment** — `Interval { every: 30m }` fires 30m after task spawn, not at `:00`/`:30`.
5. **Drift** — `sleep(interval)` doesn't subtract execution time.
6. **Missed-fire semantics undefined on restart** — bot startup is "fresh" but the existing model can't express "fire at 09:00 daily, skip if I was offline".
7. **String-typed dates and times** in storage + form, with implicit-Berlin tz and DST footguns.
8. **`ScheduleCache` is a third source of truth** alongside `SettingsHandle` and `database::Schedule`.

## Data model

Replace `ScheduleSettings`, `database::Schedule`, and `ScheduleCache`
with a single `Schedule` struct living in `crates/core/src/settings/schedules.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub name: String,
    pub message: String,
    pub trigger: Trigger,
    pub start_date: Option<NaiveDate>,   // Berlin-local, inclusive
    pub end_date:   Option<NaiveDate>,   // Berlin-local, inclusive
    pub enabled: bool,
}

pub type WeekdaySet = BTreeSet<Weekday>;   // canonical order, dedup-free

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    Interval {
        #[serde(with = "humantime_serde")]
        every: Duration,
        #[serde(default)]
        days: WeekdaySet,            // empty = every day
        active_from: Option<NaiveTime>,
        active_to:   Option<NaiveTime>,
    },
    Calendar {
        #[serde(default)]
        days: WeekdaySet,            // empty = every day
        at:   NaiveTime,
    },
}
```

Notes:
- Two variants instead of three. `Calendar` subsumes both "Daily" (empty
  `days`) and "Weekly" (specific `days`). The form still labels these
  conceptually as Daily/Weekly based on the `days` value.
- `Interval` gains the same `days` filter so schedules can be restricted
  to specific weekdays (e.g. "every 30 min, Tue/Thu, 18:00–23:00").
  Empty set means every day.
- The within-day suppression window stays *inside* `Trigger::Interval`. It
  is meaningless for `Calendar`, where `at` already fixes the moment.
- `start_date`/`end_date` become `NaiveDate` (full-day inclusive). This
  removes the DST footguns of `NaiveDateTime` and matches user intent
  (date ranges, not exact instants).
- The runtime `database::Schedule` struct, `ScheduleCache`,
  `parse_to_schedule`, `parse_interval`, and `build_schedules` are
  deleted. `SettingsHandle::load().schedules` is the only source of
  truth.

RON example:

```ron
schedules: Some([
    Schedule(
        name: "hourly_reminder",
        message: "Sub goal: 67/100",
        trigger: Interval(every: "1h", days: [], active_from: Some("08:00"), active_to: Some("23:00")),
        start_date: None, end_date: None, enabled: true,
    ),
    Schedule(
        name: "thursday_8",
        message: "stream soon",
        trigger: Calendar(days: ["Thu"], at: "20:00"),
        start_date: None, end_date: None, enabled: true,
    ),
    Schedule(
        name: "stream_pings",
        message: "Stream in 30 min PogChamp",
        trigger: Interval(every: "30m", days: ["Tue", "Thu"], active_from: Some("18:00"), active_to: Some("23:00")),
        start_date: None, end_date: None, enabled: true,
    ),
]),
```

## Validation

Single `Schedule::validate(&self) -> Result<(), Vec<FieldError>>`,
called from `Settings::validate`. No second validation layer.

Rules:
- `name`: non-blank, unique, charset (existing rules; no `/?#%&+= \"\'` or control chars).
- `message`: non-blank, no control chars.
- `Trigger::Interval.every`: ≥ 1 minute.
- `Trigger::Interval.active_from` / `active_to`: both set or neither.
- `Trigger::*.days`: `BTreeSet` ensures dedup. Empty set is legal (= every day). No other server-side constraint.
- `start_date ≤ end_date` if both set.

## Scheduler engine

Each `Trigger` knows its next fire time given "now":

```rust
impl Trigger {
    /// Next instant strictly > `now` when this trigger should fire.
    /// Berlin-local semantics.
    fn next_fire_after(&self, now: DateTime<Tz>) -> DateTime<Tz>;
}
```

Per-variant logic, all in `Europe/Berlin`:

- **Interval { every, days, active_from, active_to }** — compute next
  wall-clock multiple of `every` strictly > `now`, anchored to Berlin
  midnight (so `every: 30m` fires at `:00` and `:30`). If the candidate
  falls outside the active window or its weekday isn't in `days` (when
  non-empty), advance to the next candidate that satisfies both
  filters. Active windows that cross midnight wrap.
- **Calendar { days, at }** — scan up to 7 days starting at `now`'s
  date. If `days` is empty, every day is eligible; otherwise only the
  listed weekdays. Pick the earliest eligible day whose `at` is >
  `now`.

DST handling: `chrono_tz::Berlin::from_local_datetime`. Spring-forward
gap → pick post-gap instant. Fall-back fold → pick first occurrence.
Documented on `Trigger::next_fire_after`.

`Schedule::next_active_fire(now)` wraps `Trigger::next_fire_after` and
enforces `start_date`/`end_date`:

- If `now` is before `start_date` (Berlin), bump `now` to
  `start_date` 00:00 Berlin and compute from there.
- If the computed next fire is after `end_date` 23:59:59 Berlin (or
  `now` already is), return `None` so the calling task can exit.

Per-schedule task:

```rust
async fn run_schedule_task(
    name: String,
    schedule: Schedule,
    clock: Arc<dyn Clock>,
    sender: Arc<dyn ChatSender>,
    channel: String,
    settings: SettingsHandle,
    cancel: CancellationToken,
) {
    loop {
        let now = clock.now_berlin();
        let Some(next) = schedule.next_active_fire(now) else { return; };
        let sleep_dur = (next - now).to_std().unwrap_or_default();
        tokio::select! {
            _ = sleep(sleep_dur)    => {}
            _ = cancel.cancelled()  => return,
        }
        if !still_present_and_enabled(&settings, &name, &schedule) { return; }
        sender.say(&channel, &schedule.message).await;
        record_fire(&name, clock.now_utc()).await;   // telemetry
    }
}
```

Missed-fire skip is automatic: `next_active_fire` always returns an
instant strictly after `now`, so missed fires during downtime are
silently dropped. Drift is zero because each iteration recomputes from
the absolute clock.

Cancellation uses `CancellationToken` rather than `JoinHandle::abort()`
so an in-flight `say()` is never aborted mid-call.

Orchestrator (`run_scheduled_message_handler`) — rewritten:

```rust
loop {
    tokio::select! {
        _ = change_notify.notified() => reconcile(&settings, &mut running).await,
        _ = cancel.cancelled()       => { abort_all(&mut running); return; }
    }
}
```

`reconcile`:

1. Snapshot `settings.load().schedules`, filter to `enabled == true`.
2. Diff against `running` keyed by `name`.
3. Spawn new, cancel removed (includes anything just disabled),
   cancel-and-respawn on content change.

A disabled schedule has no running task. Toggling `enabled` is a
content change like any other; reconcile handles it the same way.

The separate `run_schedule_settings_sync` task is folded into the
orchestrator and deleted. No 30s poll. Saves take effect within
milliseconds.

## Runtime telemetry

Card chips ("12 today", "12m ago", "NEXT 05:10") require state that the
current code does not keep. Settings stay config-only; telemetry lives
in a sibling file:

```rust
// $DATA_DIR/schedule_runtime.ron
pub struct ScheduleRuntime {
    pub last_fired_at: Option<DateTime<Utc>>,
    pub fires_today:   u32,
    pub day_anchor:    NaiveDate,    // Berlin; resets fires_today on roll-over
}
// keyed by schedule name
```

- Persistence: atomic tmp+rename (existing pattern), debounced flush at
  most every 5s.
- `NEXT` countdown is **not** stored — computed live via
  `Trigger::next_fire_after`.
- Day rollover: recomputed lazily on read; `fires_today` resets to 0
  when Berlin date crosses `day_anchor`.
- Missing file → empty map, created lazily on first fire.

## Dashboard UI

The schedules page becomes a card grid that matches the agreed visual
style:

- **Header** — title with count, subtitle "Changes apply within
  seconds — no restart needed.", `+ New schedule` button on the right.
- **Stats strip** — four tiles: `TOTAL`, `ACTIVE n/total`, `FIRED
  TODAY`, `NEXT FIRE hh:mm · <name>`. Derived live from settings +
  telemetry.
- **Filter chips** — `All`, `Active`, `Paused`, `Windowed` (Interval
  with an active window set). Server-side via `?tab=`.
- **Sort dropdown** — default `next fire ↑`.
- **Cards** (3 cols desktop / 2 tablet / 1 mobile):
  - Header: name + toggle switch (POST `/schedules/{name}/toggle`).
  - Body: message preview, truncated, monospace.
  - Trigger badges: `⏱ every 30 min`, `📅 Thu 20:00`, `🌙 any time`
    or `🌅 18:00–23:00`.
  - For Interval with an active window: thin 24h timeline bar with the
    window highlighted.
  - Footer chips: `● NEXT 05:10` (countdown) · `↳ 12m ago` (last
    fired) · `12 today`. Paused cards swap `NEXT` for a `paused` chip.
- **Edit** — click card opens inline form with the variant picker.
- **New schedule** — modal/sheet with the same variant picker form.

Form shape (mod-gated as today):

```
Name       [____________]
Message    [____________]
Trigger
  ( ) Interval   every  [__:__]
                 active [__:__]–[__:__]   (optional)
                 days   [☐Mon ☐Tue ☐Wed ☐Thu ☐Fri ☐Sat ☐Sun]   (empty = every day)
  ( ) Calendar   days   [☐Mon ☐Tue ☐Wed ☑Thu ☐Fri ☐Sat ☐Sun]   (empty = every day)
                 at     [20:00]
Active range  from [YYYY-MM-DD] to [YYYY-MM-DD]   (optional, Berlin)
Enabled       [☑]
Next fires:   Thu 2026-05-28 20:00 Berlin
[Save]
```

- `<input type="time">` for HH:MM.
- `<input type="date">` for `start_date`/`end_date`.
- `<input type="text" pattern="^\d{1,3}:[0-5]\d$">` for Interval `every`.
- Weekday checkbox row is a shared partial — same template fragment
  used by both variants.
- Server-rendered "Next fires:" preview using
  `Trigger::next_fire_after(clock.now_berlin())`.
- Card-view trigger badge: Calendar with empty `days` renders as
  "📅 daily 20:00"; with specific days renders as "📅 Thu 20:00" (or
  "📅 Tue·Thu 20:00" for multi-day).

Route shape (`crates/web/src/routes/schedules.rs`):

- `ScheduleForm` gains `kind: String` and two parallel weekday vectors
  (`interval_days: Vec<String>`, `calendar_days: Vec<String>`) so each
  variant's checkbox state survives a switch without bleeding into the
  other. Variant fields all `Option<String>`.
- `parse_form -> Result<Schedule, FieldErrors>` dispatches on `kind`,
  validates, returns either a typed `Schedule` or per-field errors.
- Error attribution: pass the user's submitted form back to the
  template on failure, indexing errors by submitted-row index (fixes
  the divergence bug where edit-mode error rendering could attribute
  errors to the wrong row index after a concurrent write).
- New endpoint `POST /schedules/{name}/toggle` — flips `enabled` on a
  single row via `apply_with`.

## Migration

Existing deployed instances are already on the v3 schedules layout
(`ScheduleSettings` in `settings.ron`). The legacy `[[schedules]]`
config.toml path is no longer relevant and is removed entirely
alongside the `.schedules_migrated_v3` sentinel check.

In-settings migration from old `ScheduleSettings` to new `Schedule`:

- `Settings::schema_version` bumps from 3 to 4.
- New migration step in `crates/core/src/settings/migrate.rs` runs on
  load: for each entry in raw RON `schedules`, if it has no `trigger`
  field, synthesize `trigger: Trigger::Interval { every, days: [],
  active_from, active_to }` from the legacy `interval` /
  `active_time_start` / `active_time_end` fields. Drop the legacy
  fields.
- Convert `start_date`/`end_date` from `Option<String>`
  (`YYYY-MM-DDTHH:MM:SS`) to `Option<NaiveDate>` by parsing and
  truncating the time component. The time was never UI-exposed
  meaningfully.
- Run inside settings load, before `SettingsOverrides::deserialize`,
  using a `ron::Value` transform.
- After successful migration, the next `SettingsStore::apply_with`
  writes back with `schema_version: 4`. Atomic tmp+rename keeps it
  safe.

Telemetry file: new file, no migration needed.

## Tests

New / changed coverage:

- `Trigger::next_fire_after` per variant:
  - Interval wall-clock alignment (`every 30m` at 09:17 → 09:30).
  - Interval with active window: now inside, now outside,
    midnight-spanning window.
  - Interval with `days` filter: candidate falls on a non-eligible
    weekday → advances to next eligible day's first valid slot.
  - Interval with `days` AND active window combined.
  - Calendar with empty `days`: today-at vs tomorrow-at edge (daily).
  - Calendar with specific `days`: today-not-in-days,
    today-in-days-before-`at`, today-in-days-after-`at`, multi-day
    sets.
- DST behavior:
  - Spring-forward gap: `Calendar { days: [], at: 02:30 }` on
    transition day picks post-gap moment.
  - Fall-back fold: picks first occurrence.
- `Schedule::next_active_fire` respects `start_date` (skip before),
  `end_date` (returns `None` after).
- Missed-fire skip: bot starts at 09:05 with `Calendar { days: [], at:
  09:00 }` → fires next day, not immediately.
- Orchestrator reconcile:
  - Save → notify → respawn within tight bound (`tokio::time::pause`).
  - Add / remove / edit / toggle paths reconcile correctly.
  - Cancellation mid-sleep doesn't drop a `say()` in progress.
- Settings migration v3→v4: golden RON fixture, deserialize via
  migrator, assert shape.
- Validation: single `Schedule::validate` path; remove
  `database::Schedule::validate` and its tests.
- Dashboard:
  - New schedule of each variant kind round-trips.
  - Toggle endpoint flips `enabled` without entering edit mode.
  - "Next fires" preview rendered correctly per variant.
- Telemetry:
  - Day rollover resets `fires_today`.
  - Debounced flush writes at most once per 5s under burst.
  - Atomic tmp+rename pattern.

## Deletions

- `database::Schedule`, `database::ScheduleCache`.
- `parse_interval`, `parse_to_schedule`, `build_schedules`.
- `run_schedule_settings_sync` (folded into orchestrator).
- Legacy `1h30m` interval format parsing.
- Legacy `[[schedules]]` config.toml migration path and the
  `.schedules_migrated_v3` sentinel handling in `main.rs`.
- All tests covering the above.
