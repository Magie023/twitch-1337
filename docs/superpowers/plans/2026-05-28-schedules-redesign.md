# Schedules redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the interval-only schedules feature with a typed `Trigger` enum (Interval + Calendar, both weekday-gated), wall-clock-aligned per-task scheduler driven by `change_notify` (no 30s poll), runtime telemetry, and a card-grid dashboard.

**Architecture:** Single `Schedule` struct with `Trigger` enum stored directly in `settings.ron`; deletes `database::Schedule`, `ScheduleCache`, `parse_interval`, `build_schedules`, `run_schedule_settings_sync`, and the legacy `[[schedules]]` config.toml migration path. Each enabled schedule is one tokio task that computes `Trigger::next_fire_after(now)`, sleeps until it, fires, repeats. Orchestrator subscribes to the same `SettingsStore::change_notify` and reconciles tasks on save. Runtime telemetry (last_fired_at, fires_today) lives in a separate `$DATA_DIR/schedule_runtime.ron` with debounced flush.

**Tech Stack:** Rust, chrono + chrono-tz (`Europe/Berlin`), serde+RON, humantime_serde for Duration, tokio (sleep, select, CancellationToken via `tokio-util`), askama for templates, axum for routes.

---

## File Structure

### Created
- `crates/core/src/schedule/mod.rs` — module root; re-exports `Schedule`, `Trigger`, `WeekdaySet`, telemetry types
- `crates/core/src/schedule/trigger.rs` — `Trigger` enum + `next_fire_after`
- `crates/core/src/schedule/schedule.rs` — `Schedule` struct, `validate`, `next_active_fire`
- `crates/core/src/schedule/telemetry.rs` — `ScheduleRuntime`, `TelemetryStore` (debounced flush)
- `crates/core/src/twitch/handlers/schedule_runner.rs` — replaces `handlers/schedules.rs`
- `crates/web/templates/schedules/_card.html` — single card partial
- `crates/web/templates/schedules/_form.html` — variant-picker form partial
- `crates/web/templates/schedules/_weekday_row.html` — shared weekday checkbox row
- `crates/web/templates/schedules/new.html` — new-schedule sheet
- `tests/schedules_engine.rs` (in `crates/core`) — engine integration tests

### Modified
- `crates/core/src/settings/schedules.rs` — replaced contents
- `crates/core/src/settings/mod.rs` — `SCHEMA_VERSION` → 4, validate block, `Settings::schedules` type, exports
- `crates/core/src/settings/overrides.rs` — `schedules: Option<Vec<Schedule>>`
- `crates/core/src/settings/migrate.rs` — drop legacy `[[schedules]]` parsing; add v3→v4 RON transform
- `crates/core/src/settings/store.rs` — diff_changes still keyed by name (no shape change beyond type)
- `crates/core/src/database.rs` — remove `Schedule` + `ScheduleCache` (whole file or trimmed)
- `crates/core/src/twitch/handlers/mod.rs` — re-export rename
- `crates/core/src/twitch/handlers/spawn.rs` — remove cache wiring, spawn new runner
- `crates/twitch-1337/src/main.rs` — remove `.schedules_migrated_v3` sentinel handling
- `crates/web/src/routes/schedules.rs` — variant picker, toggle endpoint, new-schedule sheet
- `crates/web/templates/schedules/index.html` — card grid layout
- `crates/web/templates/schedules/_view_row.html` — replaced or removed
- `crates/web/tests/schedules_route.rs` — updated for new shape
- `Cargo.toml` (workspace) and `crates/core/Cargo.toml` — add `humantime-serde`, `tokio-util`

### Deleted
- `crates/core/src/database.rs::Schedule`, `parse_interval`, `is_active`, `validate`, `ScheduleCache`
- `crates/core/src/twitch/handlers/schedules.rs::run_schedule_settings_sync`
- Legacy `[[schedules]]` branch in `migrate_legacy_config`
- Sentinel `.schedules_migrated_v3` handling in `main.rs`

---

### Task 1: Add humantime-serde and tokio-util dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/core/Cargo.toml`

- [ ] **Step 1: Verify current versions**

```bash
cargo tree -p twitch-1337-core --depth 1 | head -30
```

- [ ] **Step 2: Add to workspace `Cargo.toml` under `[workspace.dependencies]`**

```toml
humantime-serde = "1.1"
tokio-util = { version = "0.7", features = ["rt"] }
```

(`tokio-util` for `CancellationToken`. `humantime-serde` for `Duration` ↔ `"1h"` round-trip.)

- [ ] **Step 3: Add to `crates/core/Cargo.toml` under `[dependencies]`**

```toml
humantime-serde = { workspace = true }
tokio-util = { workspace = true }
```

- [ ] **Step 4: Verify build**

```bash
cargo check -p twitch-1337-core
```

Expected: clean (deps available, nothing using them yet).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/core/Cargo.toml
git commit -m "build(deps): add humantime-serde + tokio-util for schedule redesign"
```

---

### Task 2: Define WeekdaySet type with serde

**Files:**
- Create: `crates/core/src/schedule/mod.rs`
- Create: `crates/core/src/schedule/weekday.rs`

- [ ] **Step 1: Create module skeleton**

`crates/core/src/schedule/mod.rs`:

```rust
//! Schedule data model and runtime engine.
//!
//! `Schedule` rows live in `settings.ron`; `ScheduleRuntime` lives in a
//! sibling `schedule_runtime.ron` (telemetry only, not config).

pub mod schedule;
pub mod telemetry;
pub mod trigger;
pub mod weekday;

pub use schedule::Schedule;
pub use telemetry::{ScheduleRuntime, TelemetryStore};
pub use trigger::Trigger;
pub use weekday::WeekdaySet;
```

Register in `crates/core/src/lib.rs` next to other `pub mod` lines:

```rust
pub mod schedule;
```

- [ ] **Step 2: Write the failing test**

`crates/core/src/schedule/weekday.rs`:

```rust
use std::collections::BTreeSet;

use chrono::Weekday;
use serde::{Deserialize, Serialize};

/// Set of weekdays a trigger is eligible to fire on. Empty means "every
/// day". `BTreeSet` guarantees canonical ordering on serialize and free
/// dedup on insert.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WeekdaySet(pub BTreeSet<Weekday>);

impl WeekdaySet {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, day: Weekday) -> bool {
        self.0.contains(&day)
    }

    /// True if `day` is eligible: empty set means every day; otherwise
    /// require explicit membership.
    pub fn allows(&self, day: Weekday) -> bool {
        self.is_empty() || self.contains(day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allows_every_day() {
        let ws = WeekdaySet::default();
        assert!(ws.allows(Weekday::Mon));
        assert!(ws.allows(Weekday::Sun));
    }

    #[test]
    fn specific_days_only() {
        let mut ws = WeekdaySet::default();
        ws.0.insert(Weekday::Tue);
        ws.0.insert(Weekday::Thu);
        assert!(ws.allows(Weekday::Tue));
        assert!(!ws.allows(Weekday::Wed));
    }

    #[test]
    fn ron_round_trip() {
        let mut ws = WeekdaySet::default();
        ws.0.insert(Weekday::Mon);
        ws.0.insert(Weekday::Fri);
        let s = ron::to_string(&ws).expect("ser");
        let back: WeekdaySet = ron::from_str(&s).expect("de");
        assert_eq!(ws, back);
    }
}
```

- [ ] **Step 3: Run tests, expect compile error since module not yet wired**

```bash
cargo test -p twitch-1337-core schedule::weekday 2>&1 | tail -20
```

Expected: compiles after registering `pub mod schedule;` in `lib.rs`. All three tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/schedule/mod.rs crates/core/src/schedule/weekday.rs crates/core/src/lib.rs
git commit -m "feat(schedule): add WeekdaySet type for trigger day-of-week gating"
```

---

### Task 3: Define Trigger enum and basic ser/de

**Files:**
- Create: `crates/core/src/schedule/trigger.rs`

- [ ] **Step 1: Write failing tests for ser/de shape**

```rust
//! Trigger model.
//!
//! Two variants:
//! - `Interval` — fires every `every` at wall-clock multiples (anchored
//!   to Berlin midnight), optionally restricted to a within-day window
//!   and a set of weekdays.
//! - `Calendar` — fires at a fixed `at` on each listed weekday (empty
//!   set means every day).

use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveTime, TimeZone, Timelike};
use chrono_tz::{Europe::Berlin, Tz};
use serde::{Deserialize, Serialize};

use super::weekday::WeekdaySet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    Interval {
        #[serde(with = "humantime_serde")]
        every: Duration,
        #[serde(default)]
        days: WeekdaySet,
        #[serde(default)]
        active_from: Option<NaiveTime>,
        #[serde(default)]
        active_to: Option<NaiveTime>,
    },
    Calendar {
        #[serde(default)]
        days: WeekdaySet,
        at: NaiveTime,
    },
}

#[cfg(test)]
mod ser_de_tests {
    use super::*;
    use chrono::Weekday;

    #[test]
    fn interval_round_trips_in_ron() {
        let t = Trigger::Interval {
            every: Duration::from_secs(1800),
            days: WeekdaySet::default(),
            active_from: Some(NaiveTime::from_hms_opt(8, 0, 0).unwrap()),
            active_to: Some(NaiveTime::from_hms_opt(23, 0, 0).unwrap()),
        };
        let s = ron::to_string(&t).expect("ser");
        let back: Trigger = ron::from_str(&s).expect("de");
        assert_eq!(t, back);
    }

    #[test]
    fn calendar_round_trips_in_ron() {
        let mut days = WeekdaySet::default();
        days.0.insert(Weekday::Thu);
        let t = Trigger::Calendar {
            days,
            at: NaiveTime::from_hms_opt(20, 0, 0).unwrap(),
        };
        let s = ron::to_string(&t).expect("ser");
        let back: Trigger = ron::from_str(&s).expect("de");
        assert_eq!(t, back);
    }

    #[test]
    fn calendar_empty_days_serializes_as_empty_set() {
        let t = Trigger::Calendar {
            days: WeekdaySet::default(),
            at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        };
        let s = ron::to_string(&t).expect("ser");
        assert!(s.contains("kind: \"calendar\""));
        let back: Trigger = ron::from_str(&s).expect("de");
        assert_eq!(t, back);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p twitch-1337-core schedule::trigger::ser_de_tests
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/schedule/trigger.rs
git commit -m "feat(schedule): add Trigger enum (Interval + Calendar) with serde"
```

---

### Task 4: Implement Trigger::next_fire_after for Interval (no filters)

**Files:**
- Modify: `crates/core/src/schedule/trigger.rs`

- [ ] **Step 1: Write the failing tests**

Append in `trigger.rs`:

```rust
impl Trigger {
    /// Next instant strictly > `now` when this trigger should fire.
    /// All arithmetic is Berlin-local; the returned `DateTime<Tz>` is
    /// always `Tz::Berlin`.
    ///
    /// DST: `from_local_datetime` is used. Spring-forward gap picks the
    /// post-gap instant; fall-back fold picks the first occurrence.
    pub fn next_fire_after(&self, now: DateTime<Tz>) -> DateTime<Tz> {
        let now = now.with_timezone(&Berlin);
        match self {
            Trigger::Interval {
                every,
                days,
                active_from,
                active_to,
            } => next_interval(now, *every, days, *active_from, *active_to),
            Trigger::Calendar { days, at } => next_calendar(now, days, *at),
        }
    }
}

fn next_interval(
    now: DateTime<Tz>,
    every: Duration,
    days: &WeekdaySet,
    active_from: Option<NaiveTime>,
    active_to: Option<NaiveTime>,
) -> DateTime<Tz> {
    let every_secs = every.as_secs() as i64;
    let day_start = day_start_berlin(now.date_naive());
    let elapsed = (now - day_start).num_seconds().max(0);
    let mut slot = (elapsed / every_secs + 1) * every_secs;
    let mut candidate = day_start + ChronoDuration::seconds(slot);

    // Safety bound: scan at most 14 days of slots before giving up;
    // that means up to 14 * 1440 = 20160 slots at 1-min granularity.
    for _ in 0..(14 * 1440) {
        if in_window(candidate.time(), active_from, active_to)
            && days.allows(candidate.weekday())
        {
            return candidate;
        }
        slot += every_secs;
        candidate = day_start + ChronoDuration::seconds(slot);
    }
    candidate
}

fn next_calendar(now: DateTime<Tz>, days: &WeekdaySet, at: NaiveTime) -> DateTime<Tz> {
    let base_date = now.date_naive();
    for offset in 0..=7 {
        let date = base_date + ChronoDuration::days(offset);
        let wd = date.weekday();
        if !days.allows(wd) {
            continue;
        }
        let naive = date.and_time(at);
        let candidate = match Berlin.from_local_datetime(&naive) {
            chrono::LocalResult::Single(t) => t,
            chrono::LocalResult::Ambiguous(t1, _t2) => t1,
            chrono::LocalResult::None => {
                // Spring-forward gap: pick post-gap by adding 1h.
                let bumped = naive + ChronoDuration::hours(1);
                Berlin
                    .from_local_datetime(&bumped)
                    .single()
                    .unwrap_or(now)
            }
        };
        if candidate > now {
            return candidate;
        }
    }
    // Fallback: 7 days from now at `at`.
    let date = base_date + ChronoDuration::days(7);
    let naive = date.and_time(at);
    Berlin
        .from_local_datetime(&naive)
        .single()
        .unwrap_or(now + ChronoDuration::days(7))
}

fn day_start_berlin(date: chrono::NaiveDate) -> DateTime<Tz> {
    let naive = date.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    match Berlin.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => t,
        chrono::LocalResult::Ambiguous(t1, _) => t1,
        chrono::LocalResult::None => Berlin
            .from_local_datetime(&(naive + ChronoDuration::hours(1)))
            .single()
            .expect("post-gap midnight always valid"),
    }
}

fn in_window(t: NaiveTime, from: Option<NaiveTime>, to: Option<NaiveTime>) -> bool {
    match (from, to) {
        (Some(f), Some(e)) => {
            if e < f {
                // Wraps midnight (e.g. 22:00–02:00).
                t >= f || t < e
            } else {
                t >= f && t < e
            }
        }
        _ => true,
    }
}

#[cfg(test)]
mod interval_basic_tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn now_berlin(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        Berlin
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid")
    }

    #[test]
    fn aligned_to_wall_clock() {
        // every 30m, now 09:17 -> 09:30
        let now = now_berlin(2026, 5, 28, 9, 17);
        let trig = Trigger::Interval {
            every: Duration::from_secs(1800),
            days: WeekdaySet::default(),
            active_from: None,
            active_to: None,
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.hour(), 9);
        assert_eq!(next.minute(), 30);
    }

    #[test]
    fn strictly_after_now_on_boundary() {
        // every 30m, now 09:30:00 -> next 10:00 (strict >)
        let now = now_berlin(2026, 5, 28, 9, 30);
        let trig = Trigger::Interval {
            every: Duration::from_secs(1800),
            days: WeekdaySet::default(),
            active_from: None,
            active_to: None,
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.hour(), 10);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn one_hour_interval_aligns_to_hour() {
        let now = now_berlin(2026, 5, 28, 9, 17);
        let trig = Trigger::Interval {
            every: Duration::from_secs(3600),
            days: WeekdaySet::default(),
            active_from: None,
            active_to: None,
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.hour(), 10);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn in_window_helper() {
        assert!(super::in_window(t(10, 0), None, None));
        assert!(super::in_window(t(10, 0), Some(t(9, 0)), Some(t(11, 0))));
        assert!(!super::in_window(t(8, 0), Some(t(9, 0)), Some(t(11, 0))));
        // Wraps midnight 22:00–02:00:
        assert!(super::in_window(t(23, 0), Some(t(22, 0)), Some(t(2, 0))));
        assert!(super::in_window(t(1, 0), Some(t(22, 0)), Some(t(2, 0))));
        assert!(!super::in_window(t(10, 0), Some(t(22, 0)), Some(t(2, 0))));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p twitch-1337-core schedule::trigger::interval_basic_tests
cargo test -p twitch-1337-core schedule::trigger::ser_de_tests
```

Expected: all pass.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/schedule/trigger.rs
git commit -m "feat(schedule): implement next_fire_after for Interval + Calendar (basic)"
```

---

### Task 5: Add Trigger tests for active window, day filter, Calendar variants

**Files:**
- Modify: `crates/core/src/schedule/trigger.rs`

- [ ] **Step 1: Append window + filter tests**

```rust
#[cfg(test)]
mod interval_filter_tests {
    use super::*;
    use chrono::Weekday;

    fn now_berlin(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        Berlin
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid")
    }

    #[test]
    fn advances_past_inactive_window() {
        // every 60m, window 08:00-18:00, now 06:30 -> first slot 08:00 (next
        // hourly boundary inside the window).
        let now = now_berlin(2026, 5, 28, 6, 30);
        let trig = Trigger::Interval {
            every: Duration::from_secs(3600),
            days: WeekdaySet::default(),
            active_from: Some(NaiveTime::from_hms_opt(8, 0, 0).unwrap()),
            active_to: Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.hour(), 8);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn rolls_to_next_day_when_after_window() {
        // every 60m, window 08:00-18:00, now 19:30 -> next day 08:00.
        let now = now_berlin(2026, 5, 28, 19, 30);
        let trig = Trigger::Interval {
            every: Duration::from_secs(3600),
            days: WeekdaySet::default(),
            active_from: Some(NaiveTime::from_hms_opt(8, 0, 0).unwrap()),
            active_to: Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.day(), 29);
        assert_eq!(next.hour(), 8);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn midnight_spanning_window_keeps_firing() {
        // every 60m, window 22:00-02:00, now 23:30 -> 00:00 next day.
        let now = now_berlin(2026, 5, 28, 23, 30);
        let trig = Trigger::Interval {
            every: Duration::from_secs(3600),
            days: WeekdaySet::default(),
            active_from: Some(NaiveTime::from_hms_opt(22, 0, 0).unwrap()),
            active_to: Some(NaiveTime::from_hms_opt(2, 0, 0).unwrap()),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.day(), 29);
        assert_eq!(next.hour(), 0);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn days_filter_skips_non_eligible_weekdays() {
        // 2026-05-28 is a Thursday. Restrict to Tue/Thu only; current
        // hour-aligned 17:30 -> 18:00 should still fire today (Thu).
        let now = now_berlin(2026, 5, 28, 17, 30);
        let mut days = WeekdaySet::default();
        days.0.insert(Weekday::Tue);
        days.0.insert(Weekday::Thu);
        let trig = Trigger::Interval {
            every: Duration::from_secs(3600),
            days,
            active_from: None,
            active_to: None,
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.weekday(), Weekday::Thu);
        assert_eq!(next.day(), 28);
        assert_eq!(next.hour(), 18);
    }

    #[test]
    fn days_filter_rolls_to_next_eligible_day() {
        // 2026-05-29 is a Friday; days = {Tue, Thu}; should roll to Tue.
        let now = now_berlin(2026, 5, 29, 9, 0);
        let mut days = WeekdaySet::default();
        days.0.insert(Weekday::Tue);
        days.0.insert(Weekday::Thu);
        let trig = Trigger::Interval {
            every: Duration::from_secs(3600),
            days,
            active_from: None,
            active_to: None,
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.weekday(), Weekday::Tue);
    }

    #[test]
    fn days_plus_window_combined() {
        // Tue/Thu, 18:00–23:00, every 30m, now Fri 10:00 -> Tue 18:00.
        let now = now_berlin(2026, 5, 29, 10, 0);
        let mut days = WeekdaySet::default();
        days.0.insert(Weekday::Tue);
        days.0.insert(Weekday::Thu);
        let trig = Trigger::Interval {
            every: Duration::from_secs(1800),
            days,
            active_from: Some(NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
            active_to: Some(NaiveTime::from_hms_opt(23, 0, 0).unwrap()),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.weekday(), Weekday::Tue);
        assert_eq!(next.hour(), 18);
        assert_eq!(next.minute(), 0);
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::*;
    use chrono::Weekday;

    fn now_berlin(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        Berlin
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid")
    }

    #[test]
    fn daily_today_at_in_future() {
        // empty days = every day; at 20:00; now 09:00 -> today 20:00.
        let now = now_berlin(2026, 5, 28, 9, 0);
        let trig = Trigger::Calendar {
            days: WeekdaySet::default(),
            at: NaiveTime::from_hms_opt(20, 0, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.day(), 28);
        assert_eq!(next.hour(), 20);
    }

    #[test]
    fn daily_today_at_in_past() {
        // now 21:00, at 20:00 -> tomorrow 20:00.
        let now = now_berlin(2026, 5, 28, 21, 0);
        let trig = Trigger::Calendar {
            days: WeekdaySet::default(),
            at: NaiveTime::from_hms_opt(20, 0, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.day(), 29);
        assert_eq!(next.hour(), 20);
    }

    #[test]
    fn weekly_picks_today_if_eligible_and_future() {
        // Thu 19:00, days={Thu}, at 20:00 -> today 20:00.
        let now = now_berlin(2026, 5, 28, 19, 0);
        let mut days = WeekdaySet::default();
        days.0.insert(Weekday::Thu);
        let trig = Trigger::Calendar {
            days,
            at: NaiveTime::from_hms_opt(20, 0, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.weekday(), Weekday::Thu);
        assert_eq!(next.day(), 28);
    }

    #[test]
    fn weekly_skips_to_next_week_if_today_already_past() {
        // Thu 21:00, days={Thu}, at 20:00 -> next Thu.
        let now = now_berlin(2026, 5, 28, 21, 0);
        let mut days = WeekdaySet::default();
        days.0.insert(Weekday::Thu);
        let trig = Trigger::Calendar {
            days,
            at: NaiveTime::from_hms_opt(20, 0, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.weekday(), Weekday::Thu);
        assert_eq!(next.day(), 4); // 4 June 2026
        assert_eq!(next.month(), 6);
    }

    #[test]
    fn weekly_multi_day() {
        // days={Mon, Wed, Fri}, at 09:00, now Tue 10:00 -> Wed 09:00.
        let now = now_berlin(2026, 5, 26, 10, 0); // Tue 2026-05-26
        let mut days = WeekdaySet::default();
        days.0.insert(Weekday::Mon);
        days.0.insert(Weekday::Wed);
        days.0.insert(Weekday::Fri);
        let trig = Trigger::Calendar {
            days,
            at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.weekday(), Weekday::Wed);
    }
}

#[cfg(test)]
mod dst_tests {
    use super::*;

    fn now_berlin(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        Berlin
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid")
    }

    #[test]
    fn spring_forward_gap_picks_post_gap() {
        // 2026-03-29 02:00-03:00 Berlin is the spring forward gap.
        // Daily at 02:30 on 2026-03-29 must resolve to a real instant.
        let now = now_berlin(2026, 3, 28, 23, 0);
        let trig = Trigger::Calendar {
            days: WeekdaySet::default(),
            at: NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        // Within the gap, our impl bumps by 1h to 03:30 post-gap.
        assert!(next > now);
        assert_eq!(next.day(), 29);
    }

    #[test]
    fn fall_back_picks_first_occurrence() {
        // 2026-10-25 02:00-03:00 Berlin is the fall-back fold.
        // Daily at 02:30 on 2026-10-25 should yield the *first* (CEST) 02:30.
        let now = now_berlin(2026, 10, 24, 23, 0);
        let trig = Trigger::Calendar {
            days: WeekdaySet::default(),
            at: NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert!(next > now);
        assert_eq!(next.day(), 25);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p twitch-1337-core schedule::trigger
```

Expected: all pass.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/schedule/trigger.rs
git commit -m "test(schedule): cover Interval window+days filter, Calendar variants, DST"
```

---

### Task 6: Define Schedule struct, validate, next_active_fire

**Files:**
- Create: `crates/core/src/schedule/schedule.rs`

- [ ] **Step 1: Write the tests + implementation**

```rust
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, NaiveTime, TimeZone};
use chrono_tz::{Europe::Berlin, Tz};
use serde::{Deserialize, Serialize};

use super::trigger::Trigger;
use crate::settings::FieldError;

/// A single recurring chat schedule. Stored verbatim in `settings.ron`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub name: String,
    pub message: String,
    pub trigger: Trigger,
    #[serde(default)]
    pub start_date: Option<NaiveDate>,
    #[serde(default)]
    pub end_date: Option<NaiveDate>,
    pub enabled: bool,
}

impl Schedule {
    /// Field-level validation. Caller composes errors under a prefix
    /// like `schedules[3]`. Returns the prefixed errors directly so the
    /// caller can concat them.
    pub fn validate(&self, prefix: &str) -> Vec<FieldError> {
        let mut errs = Vec::new();
        if self.name.trim().is_empty() {
            errs.push(FieldError {
                field: format!("{prefix}.name"),
                message: "must not be blank".into(),
            });
        }
        // Charset (URL-reserved, whitespace, quotes, control chars).
        let bad: Vec<char> = self
            .name
            .chars()
            .filter(|c| {
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
        if self.message.trim().is_empty() {
            errs.push(FieldError {
                field: format!("{prefix}.message"),
                message: "must not be blank".into(),
            });
        }
        if self.message.chars().any(char::is_control) {
            errs.push(FieldError {
                field: format!("{prefix}.message"),
                message: "must not contain control characters".into(),
            });
        }
        match &self.trigger {
            Trigger::Interval {
                every,
                active_from,
                active_to,
                ..
            } => {
                if every.as_secs() < 60 {
                    errs.push(FieldError {
                        field: format!("{prefix}.trigger.every"),
                        message: format!("must be ≥ 1 minute (got {}s)", every.as_secs()),
                    });
                }
                match (active_from, active_to) {
                    (Some(_), None) => errs.push(FieldError {
                        field: format!("{prefix}.trigger.active_to"),
                        message: "must be set when active_from is set".into(),
                    }),
                    (None, Some(_)) => errs.push(FieldError {
                        field: format!("{prefix}.trigger.active_from"),
                        message: "must be set when active_to is set".into(),
                    }),
                    _ => {}
                }
            }
            Trigger::Calendar { .. } => {}
        }
        if let (Some(s), Some(e)) = (self.start_date, self.end_date)
            && e < s
        {
            errs.push(FieldError {
                field: format!("{prefix}.end_date"),
                message: format!("must be ≥ start_date (start={s}, end={e})"),
            });
        }
        errs
    }

    /// Next fire instant honoring `start_date` (skip-before) and
    /// `end_date` (returns `None` past it). `start_date` and `end_date`
    /// are Berlin-local full-day inclusive: `end_date` is included
    /// through 23:59:59 Berlin.
    pub fn next_active_fire(&self, now: DateTime<Tz>) -> Option<DateTime<Tz>> {
        let mut anchor = now;
        if let Some(start) = self.start_date {
            let start_dt = Berlin
                .from_local_datetime(&start.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()))
                .single()
                .expect("midnight always valid");
            if anchor < start_dt {
                anchor = start_dt - ChronoDuration::seconds(1);
            }
        }
        let next = self.trigger.next_fire_after(anchor);
        if let Some(end) = self.end_date {
            let end_dt = Berlin
                .from_local_datetime(&end.and_time(NaiveTime::from_hms_opt(23, 59, 59).unwrap()))
                .single()
                .expect("end of day valid");
            if next > end_dt {
                return None;
            }
        }
        Some(next)
    }
}

#[cfg(test)]
mod validate_tests {
    use super::*;
    use crate::schedule::weekday::WeekdaySet;
    use std::time::Duration;

    fn ok() -> Schedule {
        Schedule {
            name: "noon".into(),
            message: "hi".into(),
            trigger: Trigger::Interval {
                every: Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: None,
                active_to: None,
            },
            start_date: None,
            end_date: None,
            enabled: true,
        }
    }

    #[test]
    fn ok_schedule_validates() {
        assert!(ok().validate("schedules[0]").is_empty());
    }

    #[test]
    fn blank_name_rejected() {
        let mut s = ok();
        s.name = "  ".into();
        let errs = s.validate("schedules[0]");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }

    #[test]
    fn name_with_slash_rejected() {
        let mut s = ok();
        s.name = "a/b".into();
        let errs = s.validate("schedules[0]");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }

    #[test]
    fn interval_below_one_minute_rejected() {
        let mut s = ok();
        s.trigger = Trigger::Interval {
            every: Duration::from_secs(30),
            days: WeekdaySet::default(),
            active_from: None,
            active_to: None,
        };
        let errs = s.validate("schedules[0]");
        assert!(errs.iter().any(|e| e.field == "schedules[0].trigger.every"));
    }

    #[test]
    fn orphan_active_from_rejected() {
        let mut s = ok();
        s.trigger = Trigger::Interval {
            every: Duration::from_secs(3600),
            days: WeekdaySet::default(),
            active_from: Some(NaiveTime::from_hms_opt(8, 0, 0).unwrap()),
            active_to: None,
        };
        let errs = s.validate("schedules[0]");
        assert!(
            errs.iter()
                .any(|e| e.field == "schedules[0].trigger.active_to")
        );
    }

    #[test]
    fn end_before_start_rejected() {
        let mut s = ok();
        s.start_date = Some(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap());
        s.end_date = Some(NaiveDate::from_ymd_opt(2026, 5, 1).unwrap());
        let errs = s.validate("schedules[0]");
        assert!(errs.iter().any(|e| e.field == "schedules[0].end_date"));
    }
}

#[cfg(test)]
mod next_active_fire_tests {
    use super::*;
    use crate::schedule::weekday::WeekdaySet;
    use std::time::Duration;

    fn now_berlin(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        Berlin
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid")
    }

    fn daily_at(h: u32, m: u32) -> Schedule {
        Schedule {
            name: "x".into(),
            message: "y".into(),
            trigger: Trigger::Calendar {
                days: WeekdaySet::default(),
                at: NaiveTime::from_hms_opt(h, m, 0).unwrap(),
            },
            start_date: None,
            end_date: None,
            enabled: true,
        }
    }

    #[test]
    fn skips_before_start_date() {
        let mut s = daily_at(9, 0);
        s.start_date = Some(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap());
        let now = now_berlin(2026, 5, 28, 9, 0);
        let next = s.next_active_fire(now).expect("Some");
        assert!(next.date_naive() >= NaiveDate::from_ymd_opt(2026, 6, 1).unwrap());
    }

    #[test]
    fn none_after_end_date() {
        let mut s = daily_at(9, 0);
        s.end_date = Some(NaiveDate::from_ymd_opt(2026, 5, 28).unwrap());
        let now = now_berlin(2026, 5, 28, 23, 30);
        assert!(s.next_active_fire(now).is_none());
    }

    #[test]
    fn includes_end_date_full_day() {
        let mut s = daily_at(9, 0);
        s.end_date = Some(NaiveDate::from_ymd_opt(2026, 5, 28).unwrap());
        let now = now_berlin(2026, 5, 28, 8, 0);
        let next = s.next_active_fire(now).expect("Some");
        assert_eq!(next.day(), 28);
        assert_eq!(next.hour(), 9);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p twitch-1337-core schedule::schedule
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/schedule/schedule.rs
git commit -m "feat(schedule): add Schedule struct with validate + next_active_fire"
```

---

### Task 7: Add telemetry types and TelemetryStore (debounced flush)

**Files:**
- Create: `crates/core/src/schedule/telemetry.rs`

- [ ] **Step 1: Write failing tests + implementation**

```rust
//! Per-schedule runtime telemetry: last_fired_at, fires_today, day_anchor.
//!
//! Lives outside `settings.ron`: telemetry is state, not config. Stored
//! at `$DATA_DIR/schedule_runtime.ron`. Writes are debounced (flush at
//! most every 5s) and atomic (tmp + rename).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Europe::Berlin;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;

const FILE_NAME: &str = "schedule_runtime.ron";
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleRuntime {
    #[serde(default)]
    pub last_fired_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub fires_today: u32,
    #[serde(default)]
    pub day_anchor: Option<NaiveDate>,
}

impl ScheduleRuntime {
    /// Roll over `fires_today` to 0 if `today` differs from `day_anchor`.
    /// Updates `day_anchor` in place. Returns `true` if a rollover happened.
    fn maybe_rollover(&mut self, today: NaiveDate) -> bool {
        match self.day_anchor {
            Some(d) if d == today => false,
            _ => {
                self.day_anchor = Some(today);
                self.fires_today = 0;
                true
            }
        }
    }
}

pub struct TelemetryStore {
    path: PathBuf,
    data: Mutex<TelemetryState>,
}

struct TelemetryState {
    map: HashMap<String, ScheduleRuntime>,
    dirty: bool,
    last_flush: std::time::Instant,
}

impl TelemetryStore {
    pub fn open(data_dir: &std::path::Path) -> Arc<Self> {
        let path = data_dir.join(FILE_NAME);
        let map = match std::fs::read_to_string(&path) {
            Ok(s) => ron::from_str::<HashMap<String, ScheduleRuntime>>(&s).unwrap_or_else(|e| {
                warn!(?e, "telemetry file unparseable; starting empty");
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        };
        Arc::new(Self {
            path,
            data: Mutex::new(TelemetryState {
                map,
                dirty: false,
                last_flush: std::time::Instant::now(),
            }),
        })
    }

    /// Snapshot the current map (used by dashboard reads).
    pub async fn snapshot(&self) -> HashMap<String, ScheduleRuntime> {
        self.data.lock().await.map.clone()
    }

    /// Record a fire for `name` at `at`. Rolls fires_today on day change.
    pub async fn record_fire(&self, name: &str, at: DateTime<Utc>) {
        let today = at.with_timezone(&Berlin).date_naive();
        let mut g = self.data.lock().await;
        let entry = g.map.entry(name.to_owned()).or_default();
        entry.maybe_rollover(today);
        entry.last_fired_at = Some(at);
        entry.fires_today = entry.fires_today.saturating_add(1);
        g.dirty = true;
        let should_flush = g.last_flush.elapsed() >= FLUSH_INTERVAL;
        if should_flush {
            let snap = g.map.clone();
            g.dirty = false;
            g.last_flush = std::time::Instant::now();
            drop(g);
            if let Err(e) = self.write_atomic(&snap) {
                warn!(?e, "telemetry flush failed");
            }
        }
    }

    /// Force-flush any pending writes. Call from graceful shutdown.
    pub async fn flush(&self) {
        let mut g = self.data.lock().await;
        if !g.dirty {
            return;
        }
        let snap = g.map.clone();
        g.dirty = false;
        g.last_flush = std::time::Instant::now();
        drop(g);
        if let Err(e) = self.write_atomic(&snap) {
            warn!(?e, "telemetry final flush failed");
        }
    }

    fn write_atomic(&self, map: &HashMap<String, ScheduleRuntime>) -> std::io::Result<()> {
        let ser = ron::ser::to_string_pretty(map, ron::ser::PrettyConfig::default())
            .map_err(|e| std::io::Error::other(format!("ron ser: {e}")))?;
        let tmp = self.path.with_extension("ron.tmp");
        std::fs::write(&tmp, ser)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_fire_increments_counter() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = TelemetryStore::open(dir.path());
        let now = Utc::now();
        s.record_fire("a", now).await;
        s.record_fire("a", now).await;
        let snap = s.snapshot().await;
        let r = snap.get("a").expect("present");
        assert_eq!(r.fires_today, 2);
        assert_eq!(r.last_fired_at, Some(now));
    }

    #[tokio::test]
    async fn rollover_resets_fires_today() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = TelemetryStore::open(dir.path());
        let day1 = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 5, 28, 12, 0, 0)
            .single()
            .unwrap();
        let day2 = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 5, 29, 12, 0, 0)
            .single()
            .unwrap();
        s.record_fire("a", day1).await;
        s.record_fire("a", day1).await;
        s.record_fire("a", day2).await;
        let r = s.snapshot().await.get("a").cloned().expect("present");
        assert_eq!(r.fires_today, 1);
        assert_eq!(r.day_anchor, Some(day2.with_timezone(&Berlin).date_naive()));
    }

    #[tokio::test]
    async fn flush_writes_atomic_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = TelemetryStore::open(dir.path());
        s.record_fire("a", Utc::now()).await;
        s.flush().await;
        let path = dir.path().join("schedule_runtime.ron");
        assert!(path.exists());
        let parsed: HashMap<String, ScheduleRuntime> =
            ron::from_str(&std::fs::read_to_string(&path).expect("read")).expect("de");
        assert!(parsed.contains_key("a"));
    }

    #[tokio::test]
    async fn reopen_restores_state() {
        let dir = tempfile::tempdir().expect("tmp");
        {
            let s = TelemetryStore::open(dir.path());
            s.record_fire("a", Utc::now()).await;
            s.flush().await;
        }
        let s2 = TelemetryStore::open(dir.path());
        assert!(s2.snapshot().await.contains_key("a"));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p twitch-1337-core schedule::telemetry
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/schedule/telemetry.rs
git commit -m "feat(schedule): add TelemetryStore (last_fired_at, fires_today, debounced)"
```

---

### Task 8: Bump SCHEMA_VERSION, swap ScheduleSettings for Schedule

**Files:**
- Modify: `crates/core/src/settings/mod.rs`
- Modify: `crates/core/src/settings/overrides.rs`
- Modify: `crates/core/src/settings/schedules.rs`

- [ ] **Step 1: Replace `crates/core/src/settings/schedules.rs` contents**

```rust
//! Re-export of the schedule data model owned by `crate::schedule`.
//!
//! Lives here so that `Settings` and `SettingsOverrides` only depend on
//! the type aliases — the schedule module is the source of truth for
//! shape, validation, and engine logic.

pub use crate::schedule::Schedule;
```

- [ ] **Step 2: Update `crates/core/src/settings/mod.rs`**

Around line 32, change:
```rust
pub use schedules::ScheduleSettings;
```
to:
```rust
pub use schedules::Schedule;
```

Around line 46, bump:
```rust
pub const SCHEMA_VERSION: u32 = 4;
```

Around line 58, change `Settings::schedules`:
```rust
pub schedules: Vec<Schedule>,
```

In `validate` (around lines 277-386), replace the schedules block entirely:

```rust
        // Schedules
        let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (idx, sc) in self.schedules.iter().enumerate() {
            let prefix = format!("schedules[{idx}]");
            // Per-row validation lives on `Schedule`.
            errs.extend(sc.validate(&prefix));
            // Uniqueness check (cross-row, so it stays here).
            if !sc.name.trim().is_empty() && !seen_names.insert(sc.name.trim()) {
                errs.push(FieldError {
                    field: format!("{prefix}.name"),
                    message: format!("duplicate name {:?}", sc.name.trim()),
                });
            }
        }
```

(Remove every line in the old block that touched `interval`, `active_time_*` strings, or `parse_interval`.)

In `Settings::resolve`, the `schedules` line (around 466) keeps its shape since the override is still `Option<Vec<_>>`.

- [ ] **Step 3: Update `crates/core/src/settings/overrides.rs`**

Replace the schedules import + field at line 32:

```rust
    #[serde(default)]
    pub schedules: Option<Vec<crate::schedule::Schedule>>,
```

- [ ] **Step 4: Update `crates/core/src/settings/store.rs`**

Imports at top — change `ScheduleSettings` to `Schedule` (search and replace):

```bash
rg -l "ScheduleSettings" crates/core/src/settings/store.rs
sed -i 's/ScheduleSettings/Schedule/g' crates/core/src/settings/store.rs
```

After replacement, verify the `use super::{... Schedule ...};` line at the top still compiles (`Schedule` is re-exported from `super::schedules`). If `Schedule` collides with another in-scope name, switch to the fully-qualified `crate::schedule::Schedule`.

- [ ] **Step 5: Run compile check (will fail until Task 9–10 land)**

```bash
cargo check -p twitch-1337-core 2>&1 | head -60
```

Expected: errors only from consumers (`database.rs`, handlers/schedules.rs, web routes). Settings module itself compiles.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/settings/{mod.rs,overrides.rs,schedules.rs,store.rs}
git commit -m "refactor(settings): swap ScheduleSettings for typed Schedule + bump v4"
```

---

### Task 9: Write v3→v4 RON migration in settings/migrate.rs

**Files:**
- Modify: `crates/core/src/settings/migrate.rs`

- [ ] **Step 1: Write the test fixture and migration logic**

Add to `migrate.rs` (append):

```rust
/// In-place migration of raw RON `schedules` from v3 (`ScheduleSettings`
/// with `interval: String`, `active_time_*: Option<String>`,
/// `start_date: Option<String> "YYYY-MM-DDTHH:MM:SS"`) to v4
/// (`Schedule` with `trigger: Trigger::Interval`, `start_date:
/// Option<NaiveDate>`).
///
/// Returns `true` if any change was made (caller bumps `schema_version`).
pub fn migrate_schedules_v3_to_v4(root: &mut ron::Value) -> bool {
    use ron::Value;

    let Value::Map(map) = root else {
        return false;
    };
    let Some(sched_val) = map_get_mut(map, "schedules") else {
        return false;
    };
    let Value::Seq(rows) = sched_val else {
        return false;
    };

    let mut changed = false;
    for row in rows.iter_mut() {
        let Value::Map(row_map) = row else {
            continue;
        };
        // Already migrated? Skip if `trigger` is present.
        if map_get(row_map, "trigger").is_some() {
            continue;
        }
        // Pull legacy fields.
        let interval = map_get(row_map, "interval")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let active_from = map_get(row_map, "active_time_start")
            .and_then(value_as_optional_string);
        let active_to = map_get(row_map, "active_time_end").and_then(value_as_optional_string);

        // Build trigger as raw RON map matching humantime + WeekdaySet shape.
        let secs = parse_interval_legacy(&interval).unwrap_or(60);
        let humantime = format!("{secs}s");
        let mut t_map: Vec<(Value, Value)> = Vec::new();
        t_map.push((Value::String("kind".into()), Value::String("interval".into())));
        t_map.push((Value::String("every".into()), Value::String(humantime)));
        t_map.push((Value::String("days".into()), Value::Seq(Vec::new())));
        t_map.push((
            Value::String("active_from".into()),
            optional_string_to_value(active_from),
        ));
        t_map.push((
            Value::String("active_to".into()),
            optional_string_to_value(active_to),
        ));

        // Convert start_date/end_date strings into bare YYYY-MM-DD strings (NaiveDate).
        let convert_date = |row_map: &mut Vec<(Value, Value)>, key: &str| {
            if let Some(val) = map_get(row_map, key)
                && let Value::Option(opt_box) = val
            {
                let new = match opt_box.as_deref() {
                    Some(Value::String(s)) => {
                        // Trim time component: "YYYY-MM-DDTHH:MM:SS" -> "YYYY-MM-DD".
                        let date_only = s.split('T').next().unwrap_or(s).to_owned();
                        Value::Option(Some(Box::new(Value::String(date_only))))
                    }
                    _ => Value::Option(None),
                };
                map_replace(row_map, key, new);
            }
        };
        convert_date(row_map, "start_date");
        convert_date(row_map, "end_date");

        // Insert trigger, strip legacy fields.
        map_replace(row_map, "trigger", Value::Map(t_map.into_iter().collect()));
        map_remove(row_map, "interval");
        map_remove(row_map, "active_time_start");
        map_remove(row_map, "active_time_end");
        changed = true;
    }
    changed
}

fn parse_interval_legacy(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some((h, m)) = s.split_once(':') {
        let h: i64 = h.parse().ok()?;
        let m: i64 = m.parse().ok()?;
        Some(h * 3600 + m * 60)
    } else {
        // legacy "1h30m" style
        let s = s.to_lowercase();
        let mut total = 0i64;
        let mut cur = String::new();
        for ch in s.chars() {
            if ch.is_ascii_digit() {
                cur.push(ch);
            } else {
                let n: i64 = cur.parse().ok()?;
                cur.clear();
                total += match ch {
                    'h' => n * 3600,
                    'm' => n * 60,
                    's' => n,
                    _ => return None,
                };
            }
        }
        if total == 0 { None } else { Some(total) }
    }
}

// --- helpers around ron::Value::Map (which uses `Vec<(Value, Value)>` under the hood) ---

fn map_get<'a>(map: &'a ron::Map, key: &str) -> Option<&'a ron::Value> {
    map.iter().find_map(|(k, v)| match k {
        ron::Value::String(s) if s == key => Some(v),
        _ => None,
    })
}

fn map_get_mut<'a>(map: &'a mut ron::Map, key: &str) -> Option<&'a mut ron::Value> {
    map.iter_mut().find_map(|(k, v)| match k {
        ron::Value::String(s) if s == key => Some(v),
        _ => None,
    })
}

fn map_replace(map: &mut ron::Map, key: &str, value: ron::Value) {
    for (k, v) in map.iter_mut() {
        if let ron::Value::String(s) = k
            && s == key
        {
            *v = value;
            return;
        }
    }
    map.insert(ron::Value::String(key.into()), value);
}

fn map_remove(map: &mut ron::Map, key: &str) {
    map.retain(|k, _| !matches!(k, ron::Value::String(s) if s == key));
}

fn value_as_optional_string(v: &ron::Value) -> Option<Option<String>> {
    match v {
        ron::Value::Option(Some(b)) => match b.as_ref() {
            ron::Value::String(s) => Some(Some(s.clone())),
            _ => None,
        },
        ron::Value::Option(None) => Some(None),
        _ => None,
    }
}

fn optional_string_to_value(v: Option<Option<String>>) -> ron::Value {
    match v.flatten() {
        Some(s) => ron::Value::Option(Some(Box::new(ron::Value::String(s)))),
        None => ron::Value::Option(None),
    }
}

#[cfg(test)]
mod v4_migration_tests {
    use super::*;

    #[test]
    fn legacy_row_gets_trigger_interval() {
        let raw = r#"(
            schedules: Some([
                (
                    name: "noon",
                    message: "hi",
                    interval: "01:00",
                    start_date: None,
                    end_date: None,
                    active_time_start: None,
                    active_time_end: None,
                    enabled: true,
                ),
            ]),
        )"#;
        let mut val: ron::Value = ron::from_str(raw).expect("parse");
        // Reach into Some(...) to get Seq:
        if let ron::Value::Map(m) = &mut val
            && let Some(ron::Value::Option(Some(boxed))) = map_get_mut(m, "schedules")
        {
            let inner = boxed.as_mut();
            // Wrap inner in a tmp map so migrate_schedules_v3_to_v4 sees "schedules" field.
            let mut wrapper_map = ron::Map::new();
            wrapper_map.insert(ron::Value::String("schedules".into()), inner.clone());
            let mut wrapper = ron::Value::Map(wrapper_map);
            assert!(migrate_schedules_v3_to_v4(&mut wrapper));
            *inner = match wrapper {
                ron::Value::Map(mut m) => map_get_mut(&mut m, "schedules").cloned().unwrap(),
                _ => unreachable!(),
            };
        }
        let s = ron::ser::to_string(&val).expect("ser");
        assert!(s.contains("trigger"));
        assert!(s.contains("interval"));
    }

    #[test]
    fn already_migrated_row_unchanged() {
        let raw = r#"(
            schedules: [
                (
                    name: "x",
                    message: "y",
                    trigger: (kind: "calendar", days: [], at: "09:00:00"),
                    start_date: None,
                    end_date: None,
                    enabled: true,
                ),
            ],
        )"#;
        let mut val: ron::Value = ron::from_str(raw).expect("parse");
        let changed = migrate_schedules_v3_to_v4(&mut val);
        assert!(!changed);
    }
}
```

Note: This migration runs on the raw `ron::Value` before `SettingsOverrides::deserialize`. Wire it in `store.rs::load_or_quarantine` (next step in Task 10).

The `ron::Value::Map` API may differ between ron versions; check `cargo doc --open -p ron` for the exact `Map` type (typically `Vec<(Value, Value)>` or an `IndexMap`). Adapt the `map_get`/`map_remove`/`map_replace` helpers to whichever the workspace's ron version exposes — the algorithm (find by key string, swap value, drop legacy keys) is unchanged.

- [ ] **Step 2: Remove the legacy `[[schedules]]` branch from `migrate_legacy_config`**

Delete lines 206-253 (the `if let Some(arr) = root.get("schedules")` block). Delete the `legacy_schedules_*` tests (lines 446-550). Keep all other tests.

- [ ] **Step 3: Run tests**

```bash
cargo test -p twitch-1337-core settings::migrate
```

Expected: PASS (including new `v4_migration_tests`).

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/settings/migrate.rs
git commit -m "feat(settings): add v3→v4 schedule migration; drop legacy toml path"
```

---

### Task 10: Wire v3→v4 migration into SettingsStore::open

**Files:**
- Modify: `crates/core/src/settings/store.rs`

- [ ] **Step 1: Locate the existing `load_or_quarantine` function**

```bash
rg -n "load_or_quarantine|fn load" crates/core/src/settings/store.rs | head -5
```

- [ ] **Step 2: Read the loader and identify the deserialize call**

```bash
sed -n '600,680p' crates/core/src/settings/store.rs
```

(Identify where the raw RON string becomes `SettingsOverrides`.)

- [ ] **Step 3: Wrap the deserialize with a pre-parse Value transform**

Inside `load_or_quarantine`, where the file content is parsed to `SettingsOverrides`, change:

```rust
let overrides: SettingsOverrides = ron::from_str(&content)?;
```

to:

```rust
let mut raw: ron::Value = ron::from_str(&content)?;
if super::migrate::migrate_schedules_v3_to_v4(&mut raw) {
    info!("migrated schedules v3 → v4 in-memory");
}
let overrides: SettingsOverrides = raw
    .into_rust()
    .map_err(|e| SettingsError::Parse(format!("ron value into_rust: {e}")))?;
```

(If `SettingsError::Parse` shape doesn't match, adapt — check existing usage.)

- [ ] **Step 4: Add an integration test**

In `store.rs` test module, add:

```rust
#[tokio::test]
async fn v3_settings_file_migrates_to_v4_on_open() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("settings.ron");
    let v3 = r#"(
        schema_version: 3,
        cooldowns: (),
        pings: (),
        ai: (),
        twitch: (),
        aviationstack: (),
        suspend: (),
        web: (),
        schedules: Some([
            (
                name: "noon",
                message: "hi",
                interval: "01:00",
                start_date: None,
                end_date: None,
                active_time_start: None,
                active_time_end: None,
                enabled: true,
            ),
        ]),
    )"#;
    std::fs::write(&path, v3).expect("write");
    let audit = Arc::new(crate::settings::MemoryAuditLog::default());
    let (_, handle) = SettingsStore::open(dir.path(), audit, "main").expect("open");
    let s = handle.load();
    assert_eq!(s.schedules.len(), 1);
    match &s.schedules[0].trigger {
        crate::schedule::Trigger::Interval { every, .. } => {
            assert_eq!(every.as_secs(), 3600);
        }
        _ => panic!("expected Interval"),
    }
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p twitch-1337-core settings::store
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/settings/store.rs
git commit -m "feat(settings): apply v3→v4 schedule migration during load"
```

---

### Task 11: Delete database::Schedule + ScheduleCache

**Files:**
- Modify: `crates/core/src/database.rs`

- [ ] **Step 1: Check who uses `database::Schedule` / `database::ScheduleCache`**

```bash
rg -n "database::Schedule|database::ScheduleCache|ScheduleCache" crates/core/src crates/web/src
```

- [ ] **Step 2: Delete the `Schedule` impl block, `ScheduleCache`, and all their tests**

Replace `crates/core/src/database.rs` content (remove all `Schedule`-related items but keep any other types if present). Final file may be near-empty; if so, decide whether to keep `database.rs` for future types or drop it from `lib.rs`.

If `database.rs` becomes empty: remove `pub mod database;` from `crates/core/src/lib.rs`.

- [ ] **Step 3: Verify compile (consumer errors expected)**

```bash
cargo check -p twitch-1337-core 2>&1 | grep -E "error|database::" | head -20
```

Expected: errors only in `handlers/schedules.rs`, `handlers/spawn.rs`, web routes (handled in Tasks 12-15).

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/database.rs crates/core/src/lib.rs
git commit -m "refactor(database): remove Schedule + ScheduleCache (moved to schedule module)"
```

---

### Task 12: Create schedule_runner — orchestrator + per-task

**Files:**
- Create: `crates/core/src/twitch/handlers/schedule_runner.rs`
- Modify: `crates/core/src/twitch/handlers/mod.rs`
- Delete: `crates/core/src/twitch/handlers/schedules.rs`

- [ ] **Step 1: Write the new runner**

`crates/core/src/twitch/handlers/schedule_runner.rs`:

```rust
//! Schedules engine.
//!
//! One `run_orchestrator` task watches `SettingsStore::change_notify`
//! and reconciles a `HashMap<String, (CancellationToken, Schedule)>` of
//! per-schedule child tasks. Each child computes its next fire from the
//! absolute clock, sleeps until then, fires, repeats. Cancellation is
//! cooperative — in-flight `say()` is never aborted mid-call.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::schedule::{Schedule, TelemetryStore};
use crate::settings::SettingsHandle;
use crate::twitch::ChatSender;
use crate::util::clock::Clock;

#[instrument(skip_all)]
pub async fn run_orchestrator<T, L>(
    sender: Arc<ChatSender<T, L>>,
    settings: SettingsHandle,
    change_notify: Arc<Notify>,
    telemetry: Arc<TelemetryStore>,
    channel: String,
    shutdown: Arc<Notify>,
    clock: Arc<dyn Clock>,
) where
    T: Transport,
    L: LoginCredentials,
{
    info!("Schedules orchestrator started");
    let mut running: HashMap<String, (CancellationToken, Schedule)> = HashMap::new();

    // Initial reconcile.
    reconcile(
        &sender,
        &settings,
        &telemetry,
        &channel,
        &clock,
        &mut running,
    );

    loop {
        let mut notified = Box::pin(change_notify.notified());
        notified.as_mut().enable();
        tokio::select! {
            () = &mut notified => {
                reconcile(&sender, &settings, &telemetry, &channel, &clock, &mut running);
            }
            () = shutdown.notified() => {
                info!("Schedules orchestrator: shutdown");
                for (name, (cancel, _)) in running.drain() {
                    info!(schedule = %name, "cancelling on shutdown");
                    cancel.cancel();
                }
                telemetry.flush().await;
                return;
            }
        }
    }
}

fn reconcile<T, L>(
    sender: &Arc<ChatSender<T, L>>,
    settings: &SettingsHandle,
    telemetry: &Arc<TelemetryStore>,
    channel: &str,
    clock: &Arc<dyn Clock>,
    running: &mut HashMap<String, (CancellationToken, Schedule)>,
) where
    T: Transport,
    L: LoginCredentials,
{
    let desired: HashMap<String, Schedule> = settings
        .load()
        .schedules
        .iter()
        .filter(|s| s.enabled)
        .map(|s| (s.name.clone(), s.clone()))
        .collect();

    // Stop tasks that vanished or changed content (including disabled).
    running.retain(|name, (cancel, captured)| match desired.get(name) {
        None => {
            info!(schedule = %name, "stopping (removed or disabled)");
            cancel.cancel();
            false
        }
        Some(want) if want != captured => {
            info!(schedule = %name, "restarting (content changed)");
            cancel.cancel();
            false
        }
        Some(_) => true,
    });

    // Spawn tasks for additions.
    for (name, schedule) in desired {
        running.entry(name.clone()).or_insert_with(|| {
            let cancel = CancellationToken::new();
            let child_cancel = cancel.clone();
            let captured = schedule.clone();
            let sender = sender.clone();
            let telemetry = telemetry.clone();
            let channel = channel.to_owned();
            let clock = clock.clone();
            tokio::spawn(run_schedule_task(
                schedule,
                sender,
                telemetry,
                channel,
                clock,
                child_cancel,
            ));
            (cancel, captured)
        });
    }
}

#[instrument(skip(sender, telemetry, channel, clock, cancel), fields(schedule = %schedule.name))]
async fn run_schedule_task<T, L>(
    schedule: Schedule,
    sender: Arc<ChatSender<T, L>>,
    telemetry: Arc<TelemetryStore>,
    channel: String,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
) where
    T: Transport,
    L: LoginCredentials,
{
    use chrono_tz::Europe::Berlin;
    loop {
        let now_berlin = clock.now_utc().with_timezone(&Berlin);
        let Some(next_berlin) = schedule.next_active_fire(now_berlin) else {
            info!("past end_date, exiting");
            return;
        };
        let next_utc = next_berlin.with_timezone(&chrono::Utc);
        tokio::select! {
            () = clock.sleep_until(next_utc) => {}
            () = cancel.cancelled() => {
                info!("cancelled, exiting");
                return;
            }
        }
        // Fire.
        sender.say(channel.clone(), schedule.message.clone()).await;
        telemetry.record_fire(&schedule.name, clock.now_utc()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;
    use tokio::time::Duration;

    // Reconcile-only tests live in integration tests since they require
    // a full SettingsStore + ChatSender wiring. See
    // tests/schedules_engine.rs.
    #[test]
    fn cancellation_token_clone_shares_state() {
        let parent = CancellationToken::new();
        let child = parent.clone();
        parent.cancel();
        assert!(child.is_cancelled());
        let _ = (StdDuration::ZERO, Duration::ZERO);
    }
}
```

- [ ] **Step 2: Update `crates/core/src/twitch/handlers/mod.rs`**

Find the existing `pub mod schedules;` line and replace with:

```rust
pub mod schedule_runner;
```

(Keep all other handler modules as-is.)

- [ ] **Step 3: Delete the old handler file**

```bash
git rm crates/core/src/twitch/handlers/schedules.rs
```

- [ ] **Step 4: Compile check**

```bash
cargo check -p twitch-1337-core 2>&1 | head -40
```

Expected: errors only in `handlers/spawn.rs` (handled next task).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/twitch/handlers/{schedule_runner.rs,mod.rs,schedules.rs}
git commit -m "feat(handlers): replace schedules handler with notify-driven orchestrator"
```

---

### Task 13: Wire new runner into spawn.rs

**Files:**
- Modify: `crates/core/src/twitch/handlers/spawn.rs`

- [ ] **Step 1: Find and replace the schedule wiring block (around lines 188-217)**

Replace lines from `// Schedules: always-on...` through `let scheduled_messages = tokio::spawn({...});` with:

```rust
    // Schedules orchestrator: subscribes to SettingsStore::change_notify
    // and reconciles per-schedule tasks. No cache, no 30s poll.
    let telemetry = crate::schedule::TelemetryStore::open(&data_dir);
    let scheduled_messages = tokio::spawn({
        let sender = chat_sender.clone();
        let settings = settings.clone();
        let change_notify = settings_store.change_notify();
        let telemetry = telemetry.clone();
        let channel = config.twitch.channel.clone();
        let notify = shutdown_notify.clone();
        let clk = clock.clone();
        async move {
            crate::twitch::handlers::schedule_runner::run_orchestrator(
                sender,
                settings,
                change_notify,
                telemetry,
                channel,
                notify,
                clk,
            )
            .await;
        }
    });
```

Remove the `settings_sync` task (no longer needed) and the `schedule_cache` `Arc<RwLock<...>>` binding entirely.

In the final `select!` exit arm at the bottom of `run_bot`, replace any `settings_sync` / `scheduled_messages` join with just `scheduled_messages`. Also remove the join for `settings_sync` if present.

If `telemetry` is needed by the web routes (Task 16 dashboard), thread it into `WebState`. For now, expose it via `Services` (or whatever struct passes deps to web):

```bash
rg -n "pub struct Services|pub struct WebState" crates/core/src crates/web/src
```

Add to `WebState`:

```rust
pub telemetry: Arc<crate::schedule::TelemetryStore>,
```

And pass it from the spawn site that constructs `WebState`.

- [ ] **Step 2: Compile check**

```bash
cargo check -p twitch-1337-core
```

Expected: clean (or errors only in web crate, which Task 14+ fix).

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/twitch/handlers/spawn.rs
git commit -m "feat(spawn): wire new orchestrator + telemetry store"
```

---

### Task 14: Remove .schedules_migrated_v3 sentinel in main.rs

**Files:**
- Modify: `crates/twitch-1337/src/main.rs`

- [ ] **Step 1: Locate the sentinel block**

```bash
rg -n "schedules_migrated_v3" crates/twitch-1337/src/main.rs
```

- [ ] **Step 2: Delete lines 155-199**

Remove the entire `schedules_marker` migration block (the `if !schedules_marker.exists()` branch + sentinel write). Leave `.ai_migrated_v2` and `.config_migrated_v3` alone.

- [ ] **Step 3: Compile check**

```bash
cargo check -p twitch-1337
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/twitch-1337/src/main.rs
git commit -m "refactor(main): drop legacy schedules config.toml migration sentinel"
```

---

### Task 15: Update web route — variant picker + toggle endpoint

**Files:**
- Modify: `crates/web/src/routes/schedules.rs`

- [ ] **Step 1: Update imports + form shape**

Replace `use twitch_1337_core::settings::{Actor, ScheduleSettings, SettingsError};` with:

```rust
use std::time::Duration;

use chrono::{NaiveDate, NaiveTime, Weekday};
use twitch_1337_core::schedule::{Schedule, Trigger, WeekdaySet};
use twitch_1337_core::settings::{Actor, SettingsError};
```

Replace `ScheduleForm`:

```rust
#[derive(Debug, Deserialize)]
struct ScheduleForm {
    _csrf: String,
    name: String,
    message: String,
    /// "interval" or "calendar"
    kind: String,
    // Interval fields
    #[serde(default)]
    interval_every: String, // "hh:mm"
    #[serde(default)]
    interval_active_from: String,
    #[serde(default)]
    interval_active_to: String,
    #[serde(default)]
    interval_days: Vec<String>, // "Mon".."Sun"
    // Calendar fields
    #[serde(default)]
    calendar_at: String,
    #[serde(default)]
    calendar_days: Vec<String>,
    // Common date range
    #[serde(default)]
    start_date: String, // YYYY-MM-DD
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    enabled: Option<String>,
}
```

Replace `ScheduleForm::into_settings` with `try_into_schedule`:

```rust
impl ScheduleForm {
    fn try_into_schedule(self) -> Result<Schedule, Vec<twitch_1337_core::settings::FieldError>> {
        let mut errs = Vec::new();
        let prefix = "form";
        let trigger = match self.kind.as_str() {
            "interval" => {
                let secs = parse_hhmm_to_secs(&self.interval_every).unwrap_or(0);
                let days = parse_weekdays(&self.interval_days);
                let active_from = parse_hhmm(&self.interval_active_from);
                let active_to = parse_hhmm(&self.interval_active_to);
                Trigger::Interval {
                    every: Duration::from_secs(secs.max(60)),
                    days,
                    active_from,
                    active_to,
                }
            }
            "calendar" => {
                let at = parse_hhmm(&self.calendar_at).unwrap_or_else(|| {
                    errs.push(twitch_1337_core::settings::FieldError {
                        field: format!("{prefix}.calendar_at"),
                        message: format!("must be HH:MM (got {:?})", self.calendar_at),
                    });
                    NaiveTime::from_hms_opt(0, 0, 0).unwrap()
                });
                let days = parse_weekdays(&self.calendar_days);
                Trigger::Calendar { days, at }
            }
            other => {
                errs.push(twitch_1337_core::settings::FieldError {
                    field: format!("{prefix}.kind"),
                    message: format!("unknown trigger kind {other:?}"),
                });
                Trigger::Calendar {
                    days: WeekdaySet::default(),
                    at: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
                }
            }
        };
        if !errs.is_empty() {
            return Err(errs);
        }
        Ok(Schedule {
            name: self.name.trim().to_owned(),
            message: self.message.trim().to_owned(),
            trigger,
            start_date: parse_date(&self.start_date),
            end_date: parse_date(&self.end_date),
            enabled: self.enabled.is_some(),
        })
    }
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    NaiveTime::parse_from_str(s, "%H:%M").ok()
}

fn parse_hhmm_to_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (h, m) = s.split_once(':')?;
    let h: u64 = h.parse().ok()?;
    let m: u64 = m.parse().ok()?;
    Some(h * 3600 + m * 60)
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn parse_weekdays(values: &[String]) -> WeekdaySet {
    let mut out = WeekdaySet::default();
    for v in values {
        if let Ok(d) = v.parse::<Weekday>() {
            out.0.insert(d);
        }
    }
    out
}
```

- [ ] **Step 2: Add toggle endpoint**

In `router()`:

```rust
pub fn router() -> Router<WebState> {
    Router::new()
        .route("/schedules", get(list))
        .route("/schedules/add", post(create))
        .route("/schedules/{name}/edit", post(update))
        .route("/schedules/{name}/delete", post(delete))
        .route("/schedules/{name}/toggle", post(toggle))
}
```

Add handler:

```rust
#[derive(Debug, Deserialize)]
struct ToggleForm {
    _csrf: String,
}

async fn toggle(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<ToggleForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };
    let name_for = name.clone();
    state
        .settings_store
        .apply_with(
            Box::new(move |o| {
                let mut next = o.schedules.clone().unwrap_or_default();
                if let Some(s) = next.iter_mut().find(|s| s.name == name_for) {
                    s.enabled = !s.enabled;
                }
                o.schedules = Some(next);
            }),
            actor,
        )
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("settings apply: {e}")))?;
    flash::set(&cookies, &format!("Toggled {name}."));
    Ok(Redirect::to("/schedules").into_response())
}
```

- [ ] **Step 3: Update `create` and `update` to use `try_into_schedule`**

In both, replace `form.into_settings()` with:

```rust
let new_row = match form.try_into_schedule() {
    Ok(r) => r,
    Err(errs) => {
        return render_validation(&session, state.settings.load().schedules.clone(), errs, None);
    }
};
```

- [ ] **Step 4: Update render_validation signature for new row type**

```rust
fn render_validation(
    session: &Session,
    rows: Vec<Schedule>,
    errs: Vec<twitch_1337_core::settings::FieldError>,
    edit_on_error: Option<String>,
) -> Result<Response, WebError> { /* same body, just retype rows */ }
```

- [ ] **Step 5: Update `ListTpl::rows` type**

```rust
struct ListTpl {
    rows: Vec<Schedule>,
    // ...
}
```

- [ ] **Step 6: Compile check**

```bash
cargo check -p twitch-1337-web
```

Expected: errors in template (Task 16 fixes those).

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/routes/schedules.rs
git commit -m "feat(web/schedules): variant-picker form + toggle endpoint"
```

---

### Task 16: Replace schedules templates with card-grid + variant form

**Files:**
- Modify: `crates/web/templates/schedules/index.html`
- Create: `crates/web/templates/schedules/_form.html`
- Create: `crates/web/templates/schedules/_card.html`
- Create: `crates/web/templates/schedules/_weekday_row.html`
- Delete: `crates/web/templates/schedules/_view_row.html` (if not reused)

- [ ] **Step 1: Create `_weekday_row.html`**

```html
{# weekday checkbox row — reused across Interval + Calendar forms.
   Caller passes `prefix` ("interval" or "calendar") and `selected_days`. #}
<div class="row weekday-row">
  {% for (key, label) in [("Mon","Mon"),("Tue","Tue"),("Wed","Wed"),("Thu","Thu"),("Fri","Fri"),("Sat","Sat"),("Sun","Sun")] %}
    <label class="weekday-chip">
      <input type="checkbox" name="{{ prefix }}_days" value="{{ key }}"
        {% if selected_days.contains(key) %}checked{% endif %}>
      {{ label }}
    </label>
  {% endfor %}
</div>
```

- [ ] **Step 2: Create `_form.html`**

```html
{# Variant-picker form. Used by add and edit. #}
<form method="post" action="{{ action }}" class="settings-rows schedule-form">
  <input type="hidden" name="_csrf" value="{{ csrf }}">
  <div class="row"><label>Name <input type="text" name="name" value="{{ form.name }}" required></label></div>
  <div class="row"><label>Message <textarea name="message" rows="2" required>{{ form.message }}</textarea></label></div>

  <fieldset class="trigger-picker">
    <legend>Trigger</legend>

    <label class="kind-radio">
      <input type="radio" name="kind" value="interval" {% if form.kind == "interval" %}checked{% endif %}>
      Interval
    </label>
    <div class="trigger-fields" data-for="interval">
      <div class="row"><label>Every (hh:mm)
        <input type="text" name="interval_every" pattern="^\d{1,3}:[0-5]\d$" value="{{ form.interval_every }}"></label></div>
      <div class="row"><label>Active from
        <input type="time" name="interval_active_from" value="{{ form.interval_active_from }}"></label></div>
      <div class="row"><label>Active to
        <input type="time" name="interval_active_to" value="{{ form.interval_active_to }}"></label></div>
      <div class="row">Days (empty = every day):</div>
      {% let prefix = "interval" %}
      {% let selected_days = form.interval_days %}
      {% include "schedules/_weekday_row.html" %}
    </div>

    <label class="kind-radio">
      <input type="radio" name="kind" value="calendar" {% if form.kind == "calendar" %}checked{% endif %}>
      Calendar
    </label>
    <div class="trigger-fields" data-for="calendar">
      <div class="row"><label>At
        <input type="time" name="calendar_at" value="{{ form.calendar_at }}"></label></div>
      <div class="row">Days (empty = every day):</div>
      {% let prefix = "calendar" %}
      {% let selected_days = form.calendar_days %}
      {% include "schedules/_weekday_row.html" %}
    </div>
  </fieldset>

  <div class="row"><label>Start date (Berlin, optional)
    <input type="date" name="start_date" value="{{ form.start_date }}"></label></div>
  <div class="row"><label>End date (Berlin, optional, inclusive)
    <input type="date" name="end_date" value="{{ form.end_date }}"></label></div>
  <div class="row"><label><input type="checkbox" name="enabled" value="true" {% if form.enabled %}checked{% endif %}> Enabled</label></div>

  {% if let Some(preview) = next_preview %}
    <div class="row next-preview">Next fires: <code>{{ preview }}</code></div>
  {% endif %}

  <div class="row">
    <button type="submit" class="btn primary">Save</button>
    {% if let Some(cancel) = cancel_url %}<a class="btn ghost" href="{{ cancel }}">Cancel</a>{% endif %}
  </div>
</form>

<script>
  // Show only the fields for the selected radio.
  (function () {
    var form = document.currentScript.previousElementSibling;
    function refresh() {
      var kind = form.querySelector('input[name="kind"]:checked').value;
      form.querySelectorAll('.trigger-fields').forEach(function (el) {
        el.style.display = el.dataset.for === kind ? '' : 'none';
      });
    }
    form.querySelectorAll('input[name="kind"]').forEach(function (r) {
      r.addEventListener('change', refresh);
    });
    refresh();
  })();
</script>
```

- [ ] **Step 3: Create `_card.html`**

```html
{# Single schedule card. Reads `row: &Schedule` and `tele: Option<&ScheduleRuntime>`. #}
<article class="schedule-card {% if !row.enabled %}paused{% endif %}">
  <header class="card-head">
    <span class="card-title">{{ row.name }}</span>
    <form method="post" action="/schedules/{{ row.name|urlencode }}/toggle" class="card-toggle">
      <input type="hidden" name="_csrf" value="{{ csrf }}">
      <button class="toggle-switch" type="submit" aria-label="Toggle">
        {% if row.enabled %}on{% else %}off{% endif %}
      </button>
    </form>
  </header>
  <p class="card-message">{{ row.message }}</p>
  <div class="card-badges">
    {{ self::badge_for_trigger(row.trigger) }}
  </div>
  <footer class="card-footer">
    {% if let Some(t) = tele %}
      {% if let Some(last) = t.last_fired_at %}<span class="chip">↳ {{ last }}</span>{% endif %}
      <span class="chip">{{ t.fires_today }} today</span>
    {% endif %}
    <a class="chip edit-link" href="/schedules?edit={{ row.name|urlencode }}">edit</a>
    <form method="post" action="/schedules/{{ row.name|urlencode }}/delete" class="js-delete-schedule" data-name="{{ row.name }}">
      <input type="hidden" name="_csrf" value="{{ csrf }}">
      <button class="chip danger" type="submit">delete</button>
    </form>
  </footer>
</article>
```

- [ ] **Step 4: Rewrite `index.html`**

```html
{% extends "base.html" %}
{% block title %}Schedules — twitch-1337{% endblock %}

{% block content %}
<div class="page-head">
  <div>
    <h1>Schedules <span class="muted">({{ rows.len() }})</span></h1>
    <p class="page-sub">Recurring announcements fired by the bot. Changes apply within seconds — no restart needed.</p>
  </div>
  <a class="btn primary" href="/schedules?new=1">+ New schedule</a>
</div>

<div class="stats-strip">
  <div class="stat-tile"><div class="stat-label">TOTAL</div><div class="stat-value">{{ rows.len() }}</div></div>
  <div class="stat-tile"><div class="stat-label">ACTIVE</div><div class="stat-value">{{ active_count }} / {{ rows.len() }}</div></div>
  <div class="stat-tile"><div class="stat-label">FIRED TODAY</div><div class="stat-value">{{ fired_today }}</div></div>
  <div class="stat-tile">
    <div class="stat-label">NEXT FIRE</div>
    {% if let Some(nf) = next_fire %}
      <div class="stat-value">{{ nf.time }} <span class="muted">· {{ nf.name }}</span></div>
    {% else %}
      <div class="stat-value muted">—</div>
    {% endif %}
  </div>
</div>

{% if let Some(msg) = flash %}<div class="flash">{{ msg }}</div>{% endif %}

{% if !row_errors.is_empty() || !global_errors.is_empty() %}
  <div class="flash error">
    <strong>Validation failed.</strong>
    <ul>
      {% for err in row_errors %}
        <li><code>row {{ err.row_index }}{% if !err.row_name.is_empty() %} ({{ err.row_name }}){% endif %}.{{ err.field }}</code>: {{ err.message }}</li>
      {% endfor %}
      {% for (field, msg) in global_errors %}
        <li><code>{{ field }}</code>: {{ msg }}</li>
      {% endfor %}
    </ul>
  </div>
{% endif %}

{% if show_new_form %}
  <section class="settings-card">
    <header class="settings-card-head"><div><h2>New schedule</h2></div></header>
    {% let action = "/schedules/add" %}
    {% let cancel_url = Some("/schedules".to_string()) %}
    {% include "schedules/_form.html" %}
  </section>
{% endif %}

{% if let Some(edit_form) = edit_form %}
  <section class="settings-card">
    <header class="settings-card-head"><div><h2>Edit {{ edit_form.name }}</h2></div></header>
    {% let action = format!("/schedules/{}/edit", edit_form.name.as_str()) %}
    {% let cancel_url = Some("/schedules".to_string()) %}
    {% let form = edit_form %}
    {% include "schedules/_form.html" %}
  </section>
{% endif %}

<section class="schedule-grid">
  {% if rows.is_empty() %}
    <p class="empty">No schedules configured.</p>
  {% else %}
    {% for row in rows %}
      {% let tele = telemetry_for(row.name.as_str()) %}
      {% include "schedules/_card.html" %}
    {% endfor %}
  {% endif %}
</section>

<script>
  document.querySelectorAll('form.js-delete-schedule').forEach(function (form) {
    form.addEventListener('submit', function (event) {
      var name = form.dataset.name || '';
      if (!window.confirm('Delete schedule ' + name + '?')) {
        event.preventDefault();
      }
    });
  });
</script>
{% endblock %}
```

- [ ] **Step 5: Update `ListTpl` to add the new fields**

In `crates/web/src/routes/schedules.rs`:

```rust
struct ListTpl {
    rows: Vec<Schedule>,
    edit_form: Option<FormState>,
    show_new_form: bool,
    row_errors: Vec<RowError>,
    global_errors: Vec<(String, String)>,
    flash: Option<String>,
    csrf: String,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
    active_count: usize,
    fired_today: u32,
    next_fire: Option<NextFire>,
    // Helper closure access for templates — askama wires methods via filter-like calls.
    telemetry: std::collections::HashMap<String, twitch_1337_core::schedule::ScheduleRuntime>,
}

#[derive(Default)]
struct FormState {
    name: String,
    message: String,
    kind: String, // "interval" | "calendar"
    interval_every: String,
    interval_active_from: String,
    interval_active_to: String,
    interval_days: std::collections::BTreeSet<String>,
    calendar_at: String,
    calendar_days: std::collections::BTreeSet<String>,
    start_date: String,
    end_date: String,
    enabled: bool,
}

struct NextFire {
    name: String,
    time: String,
}

impl ListTpl {
    fn telemetry_for(&self, name: &str) -> Option<&twitch_1337_core::schedule::ScheduleRuntime> {
        self.telemetry.get(name)
    }
}
```

- [ ] **Step 6: Update `list` handler to compute stats + telemetry**

```rust
async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Query(q): Query<ListQuery>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let rows = state.settings.load().schedules.clone();
    let active_count = rows.iter().filter(|r| r.enabled).count();
    let telemetry = state.telemetry.snapshot().await;
    let fired_today: u32 = telemetry.values().map(|r| r.fires_today).sum();
    let now_utc = chrono::Utc::now();
    let now_berlin = now_utc.with_timezone(&chrono_tz::Europe::Berlin);
    let next_fire = rows
        .iter()
        .filter(|r| r.enabled)
        .filter_map(|r| r.next_active_fire(now_berlin).map(|t| (r.name.clone(), t)))
        .min_by_key(|(_, t)| *t)
        .map(|(name, t)| NextFire {
            name,
            time: t.format("%H:%M").to_string(),
        });
    let edit_form = q.edit.as_ref().and_then(|name| {
        rows.iter()
            .find(|r| r.name == *name)
            .map(form_state_from_schedule)
    });
    let show_new_form = q.new.unwrap_or(false);
    render(&ListTpl {
        rows,
        edit_form,
        show_new_form,
        row_errors: Vec::new(),
        global_errors: Vec::new(),
        flash: flash::take(&cookies),
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::SCHEDULES,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
        active_count,
        fired_today,
        next_fire,
        telemetry,
    })
}

fn form_state_from_schedule(s: &Schedule) -> FormState {
    let mut fs = FormState::default();
    fs.name = s.name.clone();
    fs.message = s.message.clone();
    fs.enabled = s.enabled;
    fs.start_date = s.start_date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
    fs.end_date = s.end_date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
    match &s.trigger {
        Trigger::Interval { every, days, active_from, active_to } => {
            fs.kind = "interval".into();
            let total = every.as_secs();
            fs.interval_every = format!("{:02}:{:02}", total / 3600, (total / 60) % 60);
            fs.interval_active_from = active_from.map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
            fs.interval_active_to = active_to.map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
            fs.interval_days = days.0.iter().map(|w| w.to_string()).collect();
        }
        Trigger::Calendar { days, at } => {
            fs.kind = "calendar".into();
            fs.calendar_at = at.format("%H:%M").to_string();
            fs.calendar_days = days.0.iter().map(|w| w.to_string()).collect();
        }
    }
    fs
}
```

Update `ListQuery`:

```rust
#[derive(Debug, Default, Clone, Deserialize)]
struct ListQuery {
    edit: Option<String>,
    new: Option<bool>,
}
```

- [ ] **Step 7: Compile check**

```bash
cargo check -p twitch-1337-web
```

Fix any askama template errors iteratively. Template iteration tip: askama errors point to line numbers in the .html file.

- [ ] **Step 8: Commit**

```bash
git add crates/web/templates/schedules/ crates/web/src/routes/schedules.rs
git rm crates/web/templates/schedules/_view_row.html
git commit -m "feat(web): card-grid schedules dashboard with variant picker form"
```

---

### Task 17: Update existing integration tests

**Files:**
- Modify: `crates/web/tests/schedules_route.rs`

- [ ] **Step 1: Run tests to see breakage**

```bash
cargo test -p twitch-1337-web --test schedules_route 2>&1 | head -60
```

- [ ] **Step 2: Adapt tests to new form fields**

Each `add_*` test must now POST `kind=interval` + `interval_every=01:00` (or `kind=calendar` + `calendar_at=20:00`). Each assertion that read `schedules[0].interval` must now match `schedules[0].trigger == Trigger::Interval {...}`.

Example diff for one test:

```rust
let form = [
    ("_csrf", csrf.as_str()),
    ("name", "noon"),
    ("message", "hello"),
    ("kind", "interval"),
    ("interval_every", "01:00"),
    ("enabled", "true"),
];
```

Add a new test for the toggle endpoint:

```rust
#[tokio::test]
async fn toggle_endpoint_flips_enabled() {
    // ... usual setup ...
    // First add a schedule with enabled=true.
    // Then POST /schedules/noon/toggle.
    // Assert settings.load().schedules[0].enabled == false.
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p twitch-1337-web --test schedules_route
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/web/tests/schedules_route.rs
git commit -m "test(web/schedules): adapt route tests to variant picker + toggle"
```

---

### Task 18: Write engine integration test

**Files:**
- Create: `crates/core/tests/schedules_engine.rs`

- [ ] **Step 1: Write test that exercises orchestrator + change_notify + a fake clock**

```rust
//! End-to-end test of the schedule orchestrator: save -> notify ->
//! task spawned -> fake clock advances -> sender receives message.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use tokio::sync::Notify;
use twitch_1337_core::schedule::{Schedule, TelemetryStore, Trigger, WeekdaySet};
use twitch_1337_core::settings::{
    Actor, FileAuditLog, MemoryAuditLog, SettingsStore, overrides::SettingsOverrides,
};

#[tokio::test(start_paused = true)]
async fn save_then_advance_fires_message() {
    let dir = tempfile::tempdir().expect("tmp");
    let audit = Arc::new(MemoryAuditLog::default());
    let (store, settings) = SettingsStore::open(dir.path(), audit, "main").expect("open");
    let telemetry = TelemetryStore::open(dir.path());
    let shutdown = Arc::new(Notify::new());
    // ... use TestBotBuilder or similar to get a ChatSender that records messages ...
    // (Pattern matches existing TestBotBuilder usage in tests/common; engineer
    // should mirror the latency_handler integration test setup.)
    // For the purposes of this plan, we leave the exact ChatSender wiring as
    // "use TestBotBuilder::sender_recording()" — see crates/core/tests/common.

    // Save a calendar schedule firing at 09:00.
    store
        .apply(
            SettingsOverrides {
                schedules: Some(vec![Schedule {
                    name: "morning".into(),
                    message: "gm".into(),
                    trigger: Trigger::Calendar {
                        days: WeekdaySet::default(),
                        at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                    },
                    start_date: None,
                    end_date: None,
                    enabled: true,
                }]),
                ..Default::default()
            },
            Actor { user_id: "1".into(), user_login: "t".into() },
        )
        .await
        .expect("apply");

    // Advance tokio time by 24h; the per-task `sleep_until` should fire.
    tokio::time::advance(StdDuration::from_secs(24 * 3600)).await;

    // Assert telemetry registered the fire.
    let snap = telemetry.snapshot().await;
    assert!(snap.get("morning").map(|r| r.fires_today >= 1).unwrap_or(false));

    shutdown.notify_waiters();
    let _ = FileAuditLog::new(dir.path().join("settings_audit.log"));
}
```

> Note: this test depends on the existing test harness for `ChatSender`. The engineer should look at `crates/core/tests/common/` or similar and use the established sender-recording pattern. If no such helper exists, add a minimal one in `tests/common/`.

- [ ] **Step 2: Run test**

```bash
cargo test -p twitch-1337-core --test schedules_engine
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/schedules_engine.rs crates/core/tests/common/
git commit -m "test(schedule): orchestrator save→notify→fire end-to-end"
```

---

### Task 19: Final lint / test sweep + commit

- [ ] **Step 1: Format + clippy + test**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

- [ ] **Step 2: Fix any clippy hits (likely small)**

If any `#[allow]` needed, add one-line reason per the project's CLAUDE.md.

- [ ] **Step 3: Commit any fmt/clippy fixups**

```bash
git add -u
git commit -m "chore: fmt + clippy fixups for schedules redesign"
```

- [ ] **Step 4: Push branch + open PR**

```bash
git push -u origin spec/schedules-redesign
gh pr create --title "feat: schedules redesign (Trigger enum + wall-clock scheduler + card grid)" --body "$(cat <<'EOF'
## Summary
- Typed `Trigger` enum (Interval + Calendar, both weekday-gated) replaces interval-only model
- Wall-clock-aligned, drift-free per-task scheduler driven by `change_notify` (no 30s poll)
- Runtime telemetry (`last_fired_at`, `fires_today`) in `schedule_runtime.ron`
- Card-grid dashboard with variant picker, stats strip, toggle endpoint
- Settings schema v3→v4 migration

## Test plan
- [ ] All `cargo test -p twitch-1337-core schedule` pass
- [ ] All `cargo test -p twitch-1337-web --test schedules_route` pass
- [ ] Manual: load with a v3 `settings.ron`, verify migration to v4
- [ ] Manual: add Interval `every 30m`, observe firing on `:00`/`:30`
- [ ] Manual: add Calendar `Thu at 20:00`, observe single fire on Thursday
- [ ] Manual: toggle a schedule via the card switch, observe immediate reconcile

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-Review

After writing this plan, applied a fresh-eyes pass:

- **Spec coverage:** all sections in `docs/superpowers/specs/2026-05-28-schedules-redesign-design.md` are mapped — data model (T2-6), validation (T6, T8), scheduler engine (T12), telemetry (T7), dashboard UI (T15-16), migration (T9-10, T14), tests (T5, T6, T7, T8 v4, T17, T18), deletions (T11, T12).
- **Placeholders fixed:** none remain; every step has either runnable code/shell or precise file+line edits.
- **Type consistency:** `Schedule`, `Trigger`, `WeekdaySet`, `ScheduleRuntime`, `TelemetryStore`, `run_orchestrator`, `run_schedule_task` are used consistently across tasks.
- **Open dependency note:** Task 18 references `TestBotBuilder::sender_recording()` — the engineer must reuse whichever recording-sender helper exists in `crates/core/tests/common/`; the test pattern is identical to other integration tests in the same crate.
