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
    use std::time::Duration;

    use super::*;
    use crate::schedule::weekday::WeekdaySet;

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
    use chrono::{Datelike, Timelike};

    use super::*;
    use crate::schedule::weekday::WeekdaySet;

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
