mod common;

use std::time::Duration;

use common::TestBotBuilder;

const TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::test]
async fn startup_announce_posts_to_admin_channel() {
    let mut bot = TestBotBuilder::new()
        .with_settings(|o| o.twitch.admin_channel = Some(Some("adminchan".into())))
        .spawn()
        .await;

    let (channel, body) = bot.expect_say_full(TIMEOUT).await;
    assert_eq!(
        channel, "adminchan",
        "startup announce went to wrong channel"
    );
    // TwitchIRCClient::say() prepends ". " to defeat command injection.
    let stripped = body.strip_prefix(". ").unwrap_or(&body);
    assert!(
        stripped.starts_with("I'm up KOK · b"),
        "unexpected startup body: {body}"
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn v_command_replies_with_build_and_uptime() {
    // No admin_channel configured -> no startup announce to race with.
    let mut bot = TestBotBuilder::new().spawn().await;

    bot.send("someviewer", "!v").await;

    let body = bot.expect_reply(TIMEOUT).await;
    assert!(
        body.starts_with("billyReady · b"),
        "unexpected !v body: {body}"
    );
    assert!(body.contains(" · up "), "missing uptime in !v body: {body}");

    bot.shutdown().await;
}
