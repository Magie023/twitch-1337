use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::settings::{Settings, SettingsHandle};

/// Inner source of the cooldown duration — either a live settings handle or a
/// fixed value (used by commands that aren't yet wired to `SettingsHandle`).
enum CooldownSource {
    Live {
        settings: SettingsHandle,
        getter: fn(&Settings) -> Duration,
    },
    Fixed(Duration),
}

impl CooldownSource {
    fn duration(&self) -> Duration {
        match self {
            Self::Live { settings, getter } => getter(&settings.load()),
            Self::Fixed(d) => *d,
        }
    }
}

/// Per-user cooldown tracker.
///
/// Stores the last usage timestamp per user and checks whether the cooldown
/// period has elapsed. Thread-safe via internal `Mutex`.
///
/// Use [`PerUserCooldown::live`] so that dashboard edits take effect on the
/// next command invocation without a restart. The deprecated
/// [`PerUserCooldown::fixed`] constructor is retained only for tests that
/// don't need live updates.
pub struct PerUserCooldown {
    source: CooldownSource,
    last_use: Mutex<HashMap<String, Instant>>,
}

impl PerUserCooldown {
    /// Create a cooldown tracker whose duration is read live from `settings`
    /// via `getter` on every check. The per-user timestamp map persists across
    /// turns; only the configured duration is re-read each call.
    pub fn live(settings: SettingsHandle, getter: fn(&Settings) -> Duration) -> Self {
        Self {
            source: CooldownSource::Live { settings, getter },
            last_use: Mutex::new(HashMap::new()),
        }
    }

    /// Create a cooldown tracker with a fixed duration baked in at construction
    /// time. Dashboard edits do not affect it until the bot restarts.
    #[deprecated(note = "use ::live with SettingsHandle; remaining callers are test helpers")]
    pub fn fixed(duration: Duration) -> Self {
        Self {
            source: CooldownSource::Fixed(duration),
            last_use: Mutex::new(HashMap::new()),
        }
    }
}

impl PerUserCooldown {
    /// Returns `Some(remaining)` if the user is still on cooldown, `None` if clear.
    pub async fn check(&self, user: &str) -> Option<Duration> {
        self.check_with_duration(user, self.source.duration()).await
    }

    /// Like [`Self::check`], but uses a caller-supplied cooldown duration.
    pub async fn check_with_duration(&self, user: &str, duration: Duration) -> Option<Duration> {
        let guard = self.last_use.lock().await;
        let last = guard.get(user)?;
        let elapsed = last.elapsed();
        if elapsed < duration {
            Some(duration - elapsed)
        } else {
            None
        }
    }

    /// Records that the user just used the command.
    pub async fn record(&self, user: &str) {
        let mut guard = self.last_use.lock().await;
        guard.insert(user.to_string(), Instant::now());
    }
}

/// Formats a duration as compact hours+minutes (e.g., "1h12m", "45m").
/// Ignores seconds. Returns "0m" for durations under one minute.
pub fn format_duration_hm(d: Duration) -> String {
    let total_mins = d.as_secs() / 60;
    let hours = total_mins / 60;
    let mins = total_mins % 60;
    if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

pub fn format_cooldown_remaining(remaining: Duration) -> String {
    let total_secs = remaining.as_secs();

    // Sub-second or zero: clamp to display value
    if total_secs == 0 {
        return if remaining.is_zero() {
            "0s".to_string()
        } else {
            "1s".to_string()
        };
    }

    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        }
    } else if minutes > 0 {
        if seconds > 0 {
            format!("{minutes}m {seconds}s")
        } else {
            format!("{minutes}m")
        }
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_only() {
        assert_eq!(format_cooldown_remaining(Duration::from_secs(30)), "30s");
        assert_eq!(format_cooldown_remaining(Duration::from_secs(1)), "1s");
        assert_eq!(format_cooldown_remaining(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn minutes_and_seconds() {
        assert_eq!(format_cooldown_remaining(Duration::from_secs(63)), "1m 3s");
        assert_eq!(format_cooldown_remaining(Duration::from_secs(243)), "4m 3s");
        assert_eq!(
            format_cooldown_remaining(Duration::from_secs(3599)),
            "59m 59s"
        );
    }

    #[test]
    fn exact_minutes() {
        assert_eq!(format_cooldown_remaining(Duration::from_secs(60)), "1m");
        assert_eq!(format_cooldown_remaining(Duration::from_secs(120)), "2m");
        assert_eq!(format_cooldown_remaining(Duration::from_secs(300)), "5m");
    }

    #[test]
    fn hours_and_minutes() {
        assert_eq!(format_cooldown_remaining(Duration::from_secs(3600)), "1h");
        assert_eq!(
            format_cooldown_remaining(Duration::from_secs(3900)),
            "1h 5m"
        );
        assert_eq!(format_cooldown_remaining(Duration::from_secs(7200)), "2h");
    }

    #[test]
    fn sub_second_rounds_to_one() {
        assert_eq!(format_cooldown_remaining(Duration::from_millis(500)), "1s");
        assert_eq!(format_cooldown_remaining(Duration::from_millis(100)), "1s");
    }

    #[test]
    fn zero_duration() {
        assert_eq!(format_cooldown_remaining(Duration::ZERO), "0s");
    }
}
