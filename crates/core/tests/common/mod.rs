#![allow(dead_code)]

pub mod fake_clock;
pub mod fake_llm;
pub mod fake_transport;
pub mod irc_line;
pub mod test_bot;

#[allow(unused_imports)]
pub use test_bot::{TestBot, TestBotBuilder};

/// Generous failure deadline for the "poll until a condition holds" and
/// "wait for output" waits scattered across the integration tests. These loops
/// return the instant the condition is met, so a large budget costs nothing on
/// the happy path — it only governs how long to wait before declaring failure.
/// Sized to absorb a heavily loaded CI runner (coverage instrumentation +
/// nextest parallelism) without spurious red; the waits measure liveness, not
/// correctness, so the tolerance is correctness-irrelevant.
// ponytail: one knob for every test wait deadline, not a per-call-site number.
pub const GENEROUS_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Build a leaderboard fixture from `(user, ms, date)` tuples for
/// [`TestBotBuilder::with_seeded_leaderboard`]. Replaces the per-file
/// hand-rolled `HashMap<String, PersonalBest>` seeds.
pub fn seed_leaderboard(
    entries: &[(&str, u64, chrono::NaiveDate)],
) -> std::collections::HashMap<String, twitch_1337_core::PersonalBest> {
    entries
        .iter()
        .map(|(user, ms, date)| {
            (
                (*user).to_string(),
                twitch_1337_core::PersonalBest {
                    ms: *ms,
                    date: *date,
                },
            )
        })
        .collect()
}

/// Assert that the next PRIVMSG from the bot contains `text`. Returns the full
/// line so callers can do further assertions.
#[allow(dead_code)]
pub async fn wait_for_say(bot: &mut TestBot, text: &str, timeout: std::time::Duration) -> String {
    let line = bot.expect_say(timeout).await;
    assert!(
        line.contains(text),
        "expected substring {text:?} in {line:?}"
    );
    line
}
