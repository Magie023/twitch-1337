//! Trigger model.
//!
//! Two variants:
//! - `Interval` — fires every `every` at wall-clock multiples (anchored
//!   to Berlin midnight), optionally restricted to a within-day window
//!   and a set of weekdays.
//! - `Calendar` — fires at a fixed `at` on each listed weekday (empty
//!   set means every day).

use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, LocalResult, NaiveTime, TimeZone};
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

    // Safety bound: scan at most 14 days of slots before giving up.
    for _ in 0..(14 * 1440) {
        if in_window(candidate.time(), active_from, active_to) && days.allows(candidate.weekday()) {
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
            LocalResult::Single(t) => t,
            LocalResult::Ambiguous(t1, _t2) => t1,
            LocalResult::None => {
                // Spring-forward gap: pick post-gap by adding 1h.
                let bumped = naive + ChronoDuration::hours(1);
                Berlin.from_local_datetime(&bumped).single().unwrap_or(now)
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
        LocalResult::Single(t) => t,
        LocalResult::Ambiguous(t1, _) => t1,
        LocalResult::None => Berlin
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
        assert!(s.to_lowercase().contains("calendar"));
        let back: Trigger = ron::from_str(&s).expect("de");
        assert_eq!(t, back);
    }
}

#[cfg(test)]
mod interval_basic_tests {
    use chrono::Timelike;

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

#[cfg(test)]
mod interval_filter_tests {
    use super::*;
    use chrono::{Datelike, Timelike, Weekday};

    fn now_berlin(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        Berlin
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid")
    }

    #[test]
    fn advances_past_inactive_window() {
        // every 60m, window 08:00-18:00, now 06:30 -> first slot 08:00.
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
        // 2026-05-28 is a Thursday. Restrict to Tue/Thu only;
        // now 17:30 -> 18:00 should still fire today (Thu).
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
    use chrono::{Datelike, Timelike, Weekday};

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
        // Thu 21:00, days={Thu}, at 20:00 -> next Thu 4 June 2026.
        let now = now_berlin(2026, 5, 28, 21, 0);
        let mut days = WeekdaySet::default();
        days.0.insert(Weekday::Thu);
        let trig = Trigger::Calendar {
            days,
            at: NaiveTime::from_hms_opt(20, 0, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert_eq!(next.weekday(), Weekday::Thu);
        assert_eq!(next.day(), 4);
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
        let now = now_berlin(2026, 3, 28, 23, 0);
        let trig = Trigger::Calendar {
            days: WeekdaySet::default(),
            at: NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert!(next > now);
        use chrono::Datelike;
        assert_eq!(next.day(), 29);
    }

    #[test]
    fn fall_back_picks_first_occurrence() {
        // 2026-10-25 02:00-03:00 Berlin is the fall-back fold.
        let now = now_berlin(2026, 10, 24, 23, 0);
        let trig = Trigger::Calendar {
            days: WeekdaySet::default(),
            at: NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
        };
        let next = trig.next_fire_after(now);
        assert!(next > now);
        use chrono::Datelike;
        assert_eq!(next.day(), 25);
    }
}
