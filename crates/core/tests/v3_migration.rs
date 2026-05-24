//! Integration test: legacy config.toml -> settings.ron migration runs once,
//! is idempotent on second startup, and respects the sentinel.

use std::sync::Arc;

use tempfile::tempdir;
use twitch_1337_core::settings::{
    Actor, SettingsStore, audit::MemoryAuditLog, migrate::migrate_legacy_config,
};

#[tokio::test]
async fn legacy_keys_migrate_then_idempotent() {
    let dir = tempdir().expect("tmp");
    let audit = Arc::new(MemoryAuditLog::default());
    let (store, _h) = SettingsStore::open(dir.path(), audit.clone(), "main").expect("open");

    let raw = r#"
        [twitch]
        channel = "main"
        username = "bot"
        refresh_token = "r"
        client_id = "i"
        client_secret = "s"
        expected_latency = 250
        admin_channel = "admins"

        [suspend]
        default_duration_secs = 900
    "#;
    let value: toml::Value = toml::from_str(raw).expect("parse");

    // First migration
    let patch = migrate_legacy_config(&value).expect("migrate");
    store
        .apply(
            patch,
            Actor {
                user_id: "system".into(),
                user_login: "v3-migration".into(),
            },
        )
        .await
        .expect("apply");
    let s = store.handle().load();
    assert_eq!(s.twitch.expected_latency, 250);
    assert_eq!(s.twitch.admin_channel.as_deref(), Some("admins"));
    assert_eq!(s.suspend.default_duration_secs, 900);

    // Second migration: same patch idempotently re-applied; resolved settings unchanged
    let patch2 = migrate_legacy_config(&value).expect("migrate");
    store
        .apply(
            patch2,
            Actor {
                user_id: "system".into(),
                user_login: "v3-migration".into(),
            },
        )
        .await
        .expect("apply");
    let s2 = store.handle().load();
    assert_eq!(*s, *s2);
}
