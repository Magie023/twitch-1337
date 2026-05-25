//! Schedule entries persisted in `settings.ron`. Each entry is a row of
//! the dashboard's `/schedules` page. Validation lives in
//! `Settings::validate`; conversion to the runtime `database::Schedule`
//! lives in `build_schedules`.

use eyre::{Result, WrapErr as _};
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::database;

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

/// Convert a persisted `ScheduleSettings` into the runtime `database::Schedule`.
/// Parses interval, optional dates, optional times; runs `Schedule::validate`.
pub fn parse_to_schedule(s: &ScheduleSettings) -> Result<database::Schedule> {
    let interval = database::Schedule::parse_interval(&s.interval)
        .wrap_err_with(|| format!("interval {:?}", s.interval))?;
    let start_date = match s.start_date.as_deref() {
        Some(v) => Some(
            chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S")
                .wrap_err_with(|| format!("start_date {v:?}"))?,
        ),
        None => None,
    };
    let end_date = match s.end_date.as_deref() {
        Some(v) => Some(
            chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S")
                .wrap_err_with(|| format!("end_date {v:?}"))?,
        ),
        None => None,
    };
    let active_time_start = match s.active_time_start.as_deref() {
        Some(v) => Some(
            chrono::NaiveTime::parse_from_str(v, "%H:%M")
                .wrap_err_with(|| format!("active_time_start {v:?}"))?,
        ),
        None => None,
    };
    let active_time_end = match s.active_time_end.as_deref() {
        Some(v) => Some(
            chrono::NaiveTime::parse_from_str(v, "%H:%M")
                .wrap_err_with(|| format!("active_time_end {v:?}"))?,
        ),
        None => None,
    };
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
