//! Tracing / observability initialisation.

use chrono::{DateTime, Utc};
use chrono_tz::Europe::Berlin;
use tracing_error::ErrorLayer;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// `HH:MM:SS.mmm` in Europe/Berlin. The default `SystemTime` formatter emits
/// the full RFC3339 timestamp on every line (~30 chars including date + tz +
/// microseconds), which is overkill for a single-process bot whose logs
/// always belong to today. The short form preserves millisecond precision —
/// enough to order events within a handler — while leaving room for the
/// actual message content. Berlin local time keeps log timestamps on the
/// same clock as the bot's domain logic (1337 tracker, schedules, dreamer),
/// so the 13:37 event logs as 13:37.
struct ShortTimer;

fn short_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Berlin).format("%H:%M:%S%.3f").to_string()
}

impl FormatTime for ShortTimer {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", short_time(Utc::now()))
    }
}

/// Install the global tracing subscriber (format + env-filter + error layer).
///
/// Call once at program startup before any spans are created.
pub fn install_tracing() {
    // `with_target(true)` surfaces the emitting module (e.g. `twitch_1337`
    // vs `twitch_irc` vs `reqwest`) so an unexpected line can be traced
    // back to a crate without grepping. Pair with `ShortTimer` for the
    // line-budget we just freed by dropping the date prefix.
    let fmt_layer = fmt::layer().with_target(true).with_timer(ShortTimer);
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(ErrorLayer::default())
        .init();
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use super::short_time;

    #[test]
    fn short_time_formats_in_berlin_summer_time() {
        // 11:37 UTC in June is 13:37 CEST (UTC+2) — the 1337 event must
        // read as 13:37 in the logs.
        let t = Utc.with_ymd_and_hms(2026, 6, 10, 11, 37, 0).unwrap() + Duration::milliseconds(123);
        assert_eq!(short_time(t), "13:37:00.123");
    }

    #[test]
    fn short_time_formats_in_berlin_winter_time() {
        // 12:37 UTC in January is 13:37 CET (UTC+1).
        let t = Utc.with_ymd_and_hms(2026, 1, 15, 12, 37, 0).unwrap();
        assert_eq!(short_time(t), "13:37:00.000");
    }
}
