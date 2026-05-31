//! Build-identity helpers and lifecycle chat announces.
//!
//! `version_line` / `format_uptime` and the three message builders are pure and
//! unit-tested here. `announce_startup` / `announce_shutdown` are thin
//! `client.say` wrappers exercised by integration tests (`tests/announce.rs`).

use std::time::Duration;

use tokio::time::timeout;
use tracing::{debug, warn};
use twitch_irc::{TwitchIRCClient, login::LoginCredentials, transport::Transport};

/// Upper bound on a single announce `say`. The shutdown sequence is otherwise
/// bounded (web drain 5s, ping-actor drain 500ms); without this a saturated
/// outgoing buffer could stall the graceful-exit path on an unbounded send.
const ANNOUNCE_SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// `{BUILD_NUM} · {GIT_SHA_SHORT}` — baked at compile time by `build.rs`.
/// `BUILD_NUM` already carries the `b` prefix in CI (e.g. `b123`, matching the
/// image tag); emit it verbatim so we don't double it up (`bb123`).
pub fn version_line() -> String {
    format!(
        "{} · {}",
        option_env!("BUILD_NUM").unwrap_or("dev"),
        option_env!("GIT_SHA_SHORT").unwrap_or("unknown")
    )
}

/// Compact uptime: the first non-zero unit and up to two more non-zero units
/// (largest first), e.g. `2d 3h 14m`, `5m 12s`, `42s`. Zero → `0s`.
pub fn format_uptime(d: Duration) -> String {
    let secs = d.as_secs();
    let parts = [
        (secs / 86_400, "d"),
        ((secs % 86_400) / 3_600, "h"),
        ((secs % 3_600) / 60, "m"),
        (secs % 60, "s"),
    ];
    let mut out: Vec<String> = parts
        .iter()
        .filter(|(v, _)| *v > 0)
        .map(|(v, u)| format!("{v}{u}"))
        .collect();
    out.truncate(3);
    if out.is_empty() {
        "0s".to_owned()
    } else {
        out.join(" ")
    }
}

/// Startup announce body.
pub fn startup_message() -> String {
    format!("I'm up KOK · {}", version_line())
}

/// Graceful-shutdown announce body (static).
pub fn shutdown_message() -> &'static str {
    "Bravo 6 going dark RatgeNV"
}

/// Crash/fault announce body (static) — used when a handler exited unexpectedly,
/// so operators don't read the graceful [`shutdown_message`] as an intentional stop.
pub fn crash_message() -> &'static str {
    "Mayday — Bravo 6 is down PANIC"
}

/// `!v` reply body.
pub fn version_reply_message(uptime: Duration) -> String {
    format!(
        "billyReady · {} · up {}",
        version_line(),
        format_uptime(uptime)
    )
}

/// Post `body` to `admin_channel`. Empty/None channel → skip + debug log; send
/// errors are logged, never propagated (announces are best-effort). `context`
/// (e.g. "startup"/"shutdown") labels the log lines.
async fn announce<T, L>(
    client: &TwitchIRCClient<T, L>,
    admin_channel: Option<&str>,
    body: String,
    context: &str,
) where
    T: Transport,
    L: LoginCredentials,
{
    let Some(channel) = admin_channel.map(str::trim).filter(|c| !c.is_empty()) else {
        debug!("No admin_channel configured; skipping {context} announce");
        return;
    };
    match timeout(ANNOUNCE_SEND_TIMEOUT, client.say(channel.to_owned(), body)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(?error, "{context} announce failed"),
        Err(_) => warn!("{context} announce timed out"),
    }
}

/// Post the startup line to `admin_channel`. Empty/None channel → skip + debug log.
pub async fn announce_startup<T, L>(client: &TwitchIRCClient<T, L>, admin_channel: Option<&str>)
where
    T: Transport,
    L: LoginCredentials,
{
    announce(client, admin_channel, startup_message(), "startup").await;
}

/// Post the shutdown line to `admin_channel`. Empty/None channel → skip + debug log.
pub async fn announce_shutdown<T, L>(client: &TwitchIRCClient<T, L>, admin_channel: Option<&str>)
where
    T: Transport,
    L: LoginCredentials,
{
    announce(
        client,
        admin_channel,
        shutdown_message().to_owned(),
        "shutdown",
    )
    .await;
}

/// Post the crash/fault line to `admin_channel` (a handler exited unexpectedly).
/// Empty/None channel → skip + debug log.
pub async fn announce_crash<T, L>(client: &TwitchIRCClient<T, L>, admin_channel: Option<&str>)
where
    T: Transport,
    L: LoginCredentials,
{
    announce(client, admin_channel, crash_message().to_owned(), "crash").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uptime_zero_is_seconds() {
        assert_eq!(format_uptime(Duration::from_secs(0)), "0s");
    }

    #[test]
    fn format_uptime_seconds_only() {
        assert_eq!(format_uptime(Duration::from_secs(42)), "42s");
    }

    #[test]
    fn format_uptime_minutes_seconds() {
        assert_eq!(format_uptime(Duration::from_secs(5 * 60 + 12)), "5m 12s");
    }

    #[test]
    fn format_uptime_truncates_to_three_units() {
        let d = Duration::from_secs(2 * 86_400 + 3 * 3_600 + 14 * 60 + 7);
        assert_eq!(format_uptime(d), "2d 3h 14m");
    }

    #[test]
    fn format_uptime_skips_interior_zeros() {
        let d = Duration::from_secs(2 * 86_400 + 7);
        assert_eq!(format_uptime(d), "2d 7s");
    }

    #[test]
    fn version_line_shape() {
        let v = version_line();
        // `BUILD_NUM` is the canonical build identity verbatim (CI bakes the
        // `b` prefix, e.g. `b123`). `version_line` must not prepend another —
        // a regression once produced `bb123`.
        assert!(v.starts_with(env!("BUILD_NUM")), "got {v}");
        assert!(!v.starts_with("bb"), "double-b prefix: {v}");
        assert!(v.contains(" · "), "got {v}");
    }

    #[test]
    fn startup_message_shape() {
        let m = startup_message();
        assert!(
            m.starts_with(&format!("I'm up KOK · {}", env!("BUILD_NUM"))),
            "got {m}"
        );
    }

    #[test]
    fn shutdown_message_is_static() {
        assert_eq!(shutdown_message(), "Bravo 6 going dark RatgeNV");
    }

    #[test]
    fn crash_message_is_distinct_from_shutdown() {
        assert_eq!(crash_message(), "Mayday — Bravo 6 is down PANIC");
        assert_ne!(crash_message(), shutdown_message());
    }

    #[test]
    fn version_reply_shape() {
        let r = version_reply_message(Duration::from_secs(90));
        assert!(
            r.starts_with(&format!("billyReady · {}", env!("BUILD_NUM"))),
            "got {r}"
        );
        assert!(r.contains(" · up 1m 30s"), "got {r}");
    }
}
