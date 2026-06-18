mod common;

use std::time::Duration;

use common::fake_transport;
use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::{ClientConfig, TwitchIRCClient};

#[tokio::test]
async fn test_bot_spawns_and_shuts_down_cleanly() {
    let bot = common::TestBotBuilder::new().spawn().await;
    bot.shutdown().await;
}

#[tokio::test]
async fn fake_transport_handshake_succeeds() {
    let mut handle = fake_transport::install().await;

    let mut cfg = ClientConfig::new_simple(StaticLoginCredentials::new(
        "bot".to_owned(),
        Some("test-token".to_owned()),
    ));
    // Prevent the client from trying a second connection if something fails.
    cfg.connection_rate_limiter = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

    let (_incoming, client) =
        TwitchIRCClient::<fake_transport::FakeTransport, StaticLoginCredentials>::new(cfg);

    client.join("test_chan".to_owned()).expect("join");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Collect the four handshake lines, waiting on the full remaining budget for
    // each rather than a tight per-line gap: under a loaded runner the lines can
    // arrive seconds apart without anything being wrong.
    let mut captured = Vec::new();
    let deadline = tokio::time::Instant::now() + common::GENEROUS_WAIT;
    while captured.len() < 4 {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        match tokio::time::timeout(deadline - now, handle.capture.recv()).await {
            Ok(Some(line)) => captured.push(line),
            _ => break, // channel closed or deadline elapsed
        }
    }
    drop(handle);
    drop(client);

    let joined = captured.join("\n");
    assert!(joined.contains("CAP REQ"), "captured: {joined}");
    assert!(joined.contains("PASS"), "captured: {joined}");
    assert!(joined.contains("NICK"), "captured: {joined}");
    assert!(joined.contains("JOIN"), "captured: {joined}");
}
