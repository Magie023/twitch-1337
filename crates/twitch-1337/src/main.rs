use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use async_trait::async_trait;
use chrono::Utc;
use color_eyre::eyre::Result;
use eyre::{WrapErr as _, eyre};
use secrecy::ExposeSecret as _;
use tokio::sync::oneshot;
use tracing::info;
use twitch_1337_core::{
    AuthenticatedLoginCredentials, PersonalBest, Services,
    ai::memory::store::MemoryStore,
    aviation, doener, ensure_data_dir, get_data_dir, install_crypto_provider, install_tracing,
    llm_factory, load_configuration, load_leaderboard,
    ping::{PingHandle, PingManager, ping_actor_channel_full},
    run_bot, setup_and_verify_twitch_client,
    twitch::whisper,
    util::clock::SystemClock,
};
use twitch_1337_web::helix::{AccessTokenProvider, HelixClient as _, ReqwestHelixClient};
use twitch_irc::login::LoginCredentials as _;

use twitch_1337_core as twitch_1337;

fn build_aviation_client(
    bootstrap: Option<&twitch_1337::config::AviationstackBootstrap>,
    settings: &twitch_1337::settings::AviationstackSettings,
) -> Option<aviation::AviationClient> {
    if settings.enabled && bootstrap.is_none() {
        tracing::warn!(
            "aviationstack.enabled=true in settings but no [aviationstack].api_key in \
             config.toml; metadata enrichment disabled. Set the api_key or disable in \
             /settings → Aviationstack."
        );
    }

    match aviation::AviationClient::new() {
        Ok(client) => {
            let client = if settings.enabled {
                client.with_aviationstack(
                    bootstrap.map(|b| b.api_key.clone()),
                    settings.base_url.clone(),
                    settings.timeout_secs,
                )
            } else {
                client
            };
            Some(client)
        }
        Err(e) => {
            tracing::error!(
                error = ?e,
                "Failed to initialize aviation client; aviation commands and flight tracker disabled"
            );
            None
        }
    }
}

#[tokio::main]
pub async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
        // reqwest uses rustls-no-provider; the ring default provider must be
        // installed before any TLS handshake or `Client::builder().build()`
        // panics. Skipping color_eyre + tracing on the healthcheck path is
        // intentional — they aren't needed for a one-shot HTTP probe.
        install_crypto_provider();
        return run_healthcheck().await;
    }

    color_eyre::install()?;
    install_tracing();
    install_crypto_provider();

    let (config, raw_toml) = load_configuration().await?;

    ensure_data_dir().await?;

    // Dashboard-managed runtime settings. Opened before IRC connect so the
    // settings-store values (admin_channel, ai_channel) can be passed to the
    // IRC setup which needs them to join channels at startup.
    // The same Arc-backed store is shared with both the IRC handlers (via
    // `Services.settings`) and `WebState`.  The audit log lives at
    // `$DATA_DIR/settings_audit.log`.
    let audit_log: Arc<dyn twitch_1337::settings::AuditLog> = Arc::new(
        twitch_1337::settings::FileAuditLog::new(get_data_dir().join("settings_audit.log")),
    );
    let (settings_store, settings_handle) = twitch_1337::settings::SettingsStore::open(
        &get_data_dir(),
        audit_log,
        &config.twitch.channel,
    )
    .wrap_err("Failed to open settings store")?;

    // One-shot migration: promote legacy [ai] keys from config.toml into
    // settings.ron on first v2 launch. A sentinel file prevents re-migration
    // on subsequent boots so later admin edits aren't clobbered.
    let migrated_marker = get_data_dir().join(".ai_migrated_v2");
    if !migrated_marker.exists() {
        let ai_patch = twitch_1337::settings::migrate::migrate_legacy_ai(&raw_toml)
            .wrap_err("ai migration")?;
        if ai_patch != twitch_1337::settings::overrides::AiOverrides::default() {
            let patch = twitch_1337::settings::overrides::SettingsOverrides {
                ai: ai_patch,
                ..twitch_1337::settings::overrides::SettingsOverrides::default()
            };
            let actor = twitch_1337::settings::Actor {
                user_id: "migrate".into(),
                user_login: "migrate".into(),
            };
            settings_store
                .apply(patch, actor)
                .await
                .wrap_err("ai migration apply")?;
            info!("migrated legacy [ai] keys into settings.ron");
        }
        std::fs::write(&migrated_marker, "").wrap_err("write .ai_migrated_v2 marker")?;
    }

    // One-shot v3 migration: promote remaining non-secret, non-bootstrap keys
    // from config.toml into settings.ron. Sentinel `.config_migrated_v3`
    // prevents re-migration so later dashboard edits aren't clobbered.
    // This MUST happen before the extra_channels capture so that
    // admin_channel/ai_channel are populated from config.toml on the first v3
    // boot and don't require an extra restart to take effect.
    let v3_marker = get_data_dir().join(".config_migrated_v3");
    let was_first_v3_boot = !v3_marker.exists();
    if was_first_v3_boot {
        let patch = twitch_1337::settings::migrate::migrate_legacy_config(&raw_toml)
            .wrap_err("v3 migration")?;
        let twitch_empty =
            patch.twitch == twitch_1337::settings::overrides::TwitchOverrides::default();
        let aviation_empty = patch.aviationstack
            == twitch_1337::settings::overrides::AviationstackOverrides::default();
        let suspend_empty =
            patch.suspend == twitch_1337::settings::overrides::SuspendOverrides::default();
        let web_empty =
            patch.web == twitch_1337::settings::overrides::WebRuntimeOverrides::default();
        if !(twitch_empty && aviation_empty && suspend_empty && web_empty) {
            let actor = twitch_1337::settings::Actor {
                user_id: "migrate".into(),
                user_login: "v3-migration".into(),
            };
            settings_store
                .apply(patch, actor)
                .await
                .wrap_err("v3 migration apply")?;
            info!("migrated legacy config.toml keys into settings.ron (v3)");
        }
        std::fs::write(&v3_marker, "").wrap_err("write .config_migrated_v3 marker")?;
    }

    // One-shot migration of `[[schedules]]`. Separate sentinel because PR 1's
    // `.config_migrated_v3` may already exist on shipped deployments; sharing
    // it would silently skip the schedules migration step.
    let schedules_marker = get_data_dir().join(".schedules_migrated_v3");
    let was_first_schedules_boot = !schedules_marker.exists();
    if was_first_schedules_boot {
        let patch = twitch_1337::settings::migrate::migrate_legacy_config(&raw_toml)
            .wrap_err("schedules v3 migration")?;
        if patch.schedules.is_some() {
            // Build a slim patch that only carries the schedules section so we
            // don't re-apply other migrated fields a second time.
            let slim = twitch_1337::settings::overrides::SettingsOverrides {
                schedules: patch.schedules,
                ..twitch_1337::settings::overrides::SettingsOverrides::default()
            };
            let actor = twitch_1337::settings::Actor {
                user_id: "migrate".into(),
                user_login: "schedules-v3-migration".into(),
            };
            // Best-effort: a legacy row that was valid under the old loose
            // validator can fail the new stricter Settings::validate (PR #227
            // review F2). Log + carry on so the bot boots; operators can
            // hand-edit config.toml or settings.ron.
            match settings_store.apply(slim, actor).await {
                Ok(_) => info!("migrated legacy [[schedules]] into settings.ron"),
                Err(e) => tracing::error!(
                    error = ?e,
                    "schedules v3 migration apply failed; legacy [[schedules]] \
                     skipped — bot will boot with no migrated schedules. \
                     Fix the offending row in config.toml or manage schedules via /schedules."
                ),
            }
        }
        // Write the sentinel regardless of apply outcome: if apply failed, the
        // operator must fix config.toml manually; re-running the migration on
        // every boot wouldn't help and risks overwriting dashboard edits made
        // in the meantime (PR #227 review F5).
        if let Err(e) = std::fs::write(&schedules_marker, "") {
            tracing::error!(
                error = ?e,
                marker = ?schedules_marker,
                "failed to write .schedules_migrated_v3 marker; next boot will re-run the migration"
            );
        }
    }

    let local = Utc::now().with_timezone(&chrono_tz::Europe::Berlin);
    let initial_schedule_count = settings_handle.load().schedules.len();
    info!(
        local_time = ?local,
        utc_time = ?Utc::now(),
        channel = %config.twitch.channel,
        username = %config.twitch.username,
        schedule_count = initial_schedule_count,
        "Starting twitch-1337 bot"
    );

    // Collect restart-required channel list from the now-post-migration snapshot.
    // Placed here (after v3 migration) so admin_channel/ai_channel set by the
    // migration are available on the first v3 boot, not just subsequent ones.
    let twitch_runtime = settings_handle.load().twitch.clone();
    let extra_channels: Vec<String> = [
        twitch_runtime.admin_channel.clone(),
        twitch_runtime.ai_channel.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();

    let (incoming, client, credentials, bot_user_id) =
        setup_and_verify_twitch_client(&config, &extra_channels).await?;
    let client = Arc::new(client);

    let doener_client = Arc::new(
        doener::DoeneratlasClient::new().wrap_err("Failed to initialize Döneratlas client")?,
    );
    let whisper_credentials = credentials.clone();
    let whisper = whisper::HelixWhisperSender::new(
        whisper_credentials,
        config.twitch.client_id.expose_secret().to_string(),
        bot_user_id,
        get_data_dir(),
    )
    .await
    .map(|sender| Arc::new(sender) as Arc<dyn whisper::WhisperSender>)?;

    let irc_connected = Arc::new(AtomicBool::new(false));

    let ping_manager =
        PingManager::load(&get_data_dir()).wrap_err("Failed to load ping manager")?;
    let (ping_actor_tx, ping_actor_rx, ping_names_tx, ping_names_rx) = ping_actor_channel_full();

    // Warn on subsequent boots if any migrated key still appears in config.toml
    // — it's silently ignored after the v3 marker has been written. Skipped on
    // the migration boot itself so we don't shout before the user can clean up.
    let legacy_v3_keys: &[(&str, &[&str])] = &[
        ("twitch.expected_latency", &["twitch", "expected_latency"]),
        ("twitch.hidden_admins", &["twitch", "hidden_admins"]),
        ("twitch.viewer_allowlist", &["twitch", "viewer_allowlist"]),
        ("twitch.admin_channel", &["twitch", "admin_channel"]),
        ("twitch.ai_channel", &["twitch", "ai_channel"]),
        (
            "suspend.default_duration_secs",
            &["suspend", "default_duration_secs"],
        ),
        ("aviationstack.enabled", &["aviationstack", "enabled"]),
        ("aviationstack.base_url", &["aviationstack", "base_url"]),
        (
            "aviationstack.timeout_secs",
            &["aviationstack", "timeout_secs"],
        ),
        ("web.session_ttl", &["web", "session_ttl"]),
        ("web.mod_check_refresh", &["web", "mod_check_refresh"]),
        ("schedules", &["schedules"]),
    ];
    let stale: Vec<&str> = legacy_v3_keys
        .iter()
        .filter_map(|(name, path)| {
            let mut node = &raw_toml;
            for seg in *path {
                node = node.get(seg)?;
            }
            Some(*name)
        })
        .collect();
    if !was_first_v3_boot && !was_first_schedules_boot && !stale.is_empty() {
        tracing::warn!(
            ?stale,
            "legacy config.toml keys are now ignored after v3 migration; remove them from config.toml"
        );
    }

    // Build aviation client from settings (restart-required fields).
    // `enabled`, `base_url`, and `timeout_secs` come from the settings store;
    // `api_key` remains in config.toml as a secret and is not dashboard-managed.
    let aviation_client = {
        let av_settings = settings_handle.load().aviationstack.clone();
        build_aviation_client(config.aviationstack.as_ref(), &av_settings)
    };

    let llm_client = llm_factory::build_llm_client(config.ai.as_ref(), &settings_handle.load())?;

    // Memory v2 store opens unconditionally so the dashboard editor has a
    // handle even when `[ai]` is disabled. The same `Arc`-backed store is
    // shared with the bot's IRC handlers / dreamer ritual via `Services`.
    let memory_store = MemoryStore::open(&get_data_dir(), settings_handle.clone())
        .await
        .wrap_err("open memory store")?;

    // Load the leaderboard before building the web spawner so the same Arc
    // can be shared with WebState (dashboard read) and the IRC tracker (writes).
    let leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>> = Arc::new(
        tokio::sync::RwLock::new(load_leaderboard(&get_data_dir()).await),
    );

    // Pre-create the flight-tracker mpsc channel when aviation is enabled so
    // the sender Arc can be wired into WebState before the handlers are spawned.
    let (aviation_tracker_tx, aviation_tracker_rx) = if aviation_client.is_some() {
        let (tx, rx) = tokio::sync::mpsc::channel::<aviation::TrackerCommand>(32);
        (Some(Arc::new(tx)), Some(rx))
    } else {
        (None, None)
    };

    let web_spawner = if config.web.enabled {
        let credentials_for_web = credentials.clone();
        Some(
            build_web_spawner(
                &config,
                credentials_for_web,
                irc_connected.clone(),
                PingHandle::new((*ping_actor_tx).clone()),
                memory_store.clone(),
                leaderboard.clone(),
                aviation_tracker_tx.clone(),
                settings_handle.clone(),
                settings_store.clone(),
            )
            .await?,
        )
    } else {
        None
    };

    let services = Services {
        clock: Arc::new(SystemClock),
        ai_bootstrap: config.ai.clone(),
        llm: llm_client,
        aviation: aviation_client,
        doener: doener_client,
        whisper: Some(whisper),
        data_dir: get_data_dir(),
        settings: settings_handle,
        settings_store,
        emote_glossary_override: None,
        irc_connected,
        web_spawner,
        ping_actor_tx,
        ping_actor_rx,
        ping_manager,
        ping_names_tx,
        ping_names_rx,
        memory_store,
        leaderboard,
        aviation_tracker_tx,
        aviation_tracker_rx,
        primary_history_tap: None,
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        let _ = shutdown_tx.send(());
    });

    run_bot(client, incoming, config, services, shutdown_rx).await
}

/// Build the web-task spawn closure. Resolves the broadcaster id, builds
/// the helix client + OAuth context, binds the listener loud (port-in-use
/// aborts startup), and returns a closure that — given the shared
/// shutdown `Notify` — spawns `run_web` on a tokio task.
// Top-level DI compositor for the web spawner; argument count tracks the
// dashboard's dependency surface and is intentionally explicit.
#[allow(clippy::too_many_arguments)]
async fn build_web_spawner(
    config: &twitch_1337::config::Configuration,
    credentials: AuthenticatedLoginCredentials,
    irc_connected: Arc<AtomicBool>,
    ping_actor: PingHandle,
    memory_store: MemoryStore,
    leaderboard: Arc<tokio::sync::RwLock<HashMap<String, PersonalBest>>>,
    tracker_tx: Option<Arc<tokio::sync::mpsc::Sender<aviation::TrackerCommand>>>,
    settings: twitch_1337::settings::SettingsHandle,
    settings_store: Arc<twitch_1337::settings::SettingsStore>,
) -> Result<twitch_1337::WebSpawner> {
    let bind_addr: std::net::SocketAddr = config
        .web
        .bind_addr
        .parse()
        .wrap_err("parse web.bind_addr")?;
    // Bind synchronously so a port-in-use failure aborts startup loudly.
    let listener = twitch_1337_web::bind(bind_addr).await?;

    let token_provider: Arc<dyn AccessTokenProvider> = Arc::new(CredsTokenProvider {
        creds: Arc::new(credentials),
    });
    let helix = Arc::new(ReqwestHelixClient::new(
        reqwest::Client::new(),
        config.twitch.client_id.clone(),
        token_provider,
    ));

    let broadcaster = helix
        .as_ref()
        .fetch_user_by_login(&config.twitch.channel)
        .await
        .wrap_err("resolve broadcaster id")?
        .ok_or_else(|| eyre!("channel `{}` not found on twitch", config.twitch.channel))?;

    let oauth = Arc::new(twitch_1337_web::auth::OAuthCtx::new(
        config.twitch.client_id.expose_secret(),
        &config.twitch.client_secret,
        &config.web.public_url,
    )?);

    let web_clock = Arc::new(twitch_1337_web::clock::SystemClock);
    let sessions = Arc::new(twitch_1337_web::auth::session::SessionTable::new(
        web_clock.clone(),
    ));

    let web_config = Arc::new(twitch_1337_web::config::WebConfig {
        bind_addr: config.web.bind_addr.clone(),
        public_url: config.web.public_url.clone(),
        session_secret: config.web.session_secret.clone(),
    });

    let signed_key = twitch_1337_web::state::derive_session_key(&config.web.session_secret)?;

    #[cfg(feature = "dev-login")]
    {
        tracing::warn!(
            target: "twitch_1337_web",
            "dev-login feature compiled in — /_dev/login mints mod sessions without OAuth (DO NOT SHIP)",
        );
    }

    let state = twitch_1337_web::WebState {
        sessions,
        helix: helix as Arc<dyn twitch_1337_web::helix::HelixClient>,
        irc_connected,
        config: web_config,
        clock: web_clock,
        channel: Arc::from(config.twitch.channel.as_str()),
        broadcaster_id: Arc::from(broadcaster.id.as_str()),
        client_id: config.twitch.client_id.clone(),
        oauth,
        ping_actor,
        memory_store,
        signed_key,
        leaderboard,
        tracker_tx,
        avatar_cache: Arc::new(twitch_1337_web::helix::AvatarCache::new(
            std::time::Duration::from_secs(3600),
        )),
        owner: Arc::new(arc_swap::ArcSwap::from_pointee(config.twitch.owner.clone())),
        settings,
        settings_store,
        ai_bootstrap: config.ai.clone().map(Arc::new),
        model_cache: Arc::new(twitch_1337_web::routes::ai_models::ModelListCache::default()),
        http: reqwest::Client::new(),
    };

    Ok(Box::new(move |shutdown| {
        let deps = twitch_1337_web::WebDeps { bind_addr, state };
        tokio::spawn(async move {
            if let Err(e) = twitch_1337_web::run_web(listener, deps, shutdown).await {
                tracing::error!(target: "twitch_1337_web", error = ?e, "Web task exited with error");
            }
        })
    }))
}

/// Bridges the bot's `RefreshingLoginCredentials` to the web crate's
/// helix `AccessTokenProvider`. Lets the helix client reuse the same
/// refreshed access token the bot already maintains in `token.ron`.
struct CredsTokenProvider {
    creds: Arc<AuthenticatedLoginCredentials>,
}

#[async_trait]
impl AccessTokenProvider for CredsTokenProvider {
    async fn current_access_token(&self) -> eyre::Result<String> {
        let creds = self
            .creds
            .as_ref()
            .get_credentials()
            .await
            .map_err(|e| eyre!("get_credentials: {e}"))?;
        Ok(creds.token.unwrap_or_default())
    }
}

/// Lightweight healthcheck for the Docker `HEALTHCHECK` directive. Reads the
/// config to find the web bind port and probes `/healthz`. When `[web]` is
/// disabled, exits 0 without touching the network so the container is still
/// considered healthy.
async fn run_healthcheck() -> Result<()> {
    let (config, _raw_toml) = load_configuration().await?;
    if !config.web.enabled {
        return Ok(());
    }
    let port = config.web.bind_addr.rsplit(':').next().unwrap_or("8080");
    let url = format!("http://127.0.0.1:{port}/healthz");
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?
        .get(&url)
        .send()
        .await?;
    if res.status().is_success() {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::build_aviation_client;
    use secrecy::SecretString;
    use twitch_1337_core::config::AviationstackBootstrap;
    use twitch_1337_core::settings::Settings;

    #[test]
    fn test_prefill_threshold_validation() {
        assert!((0.0..=1.0).contains(&0.0));
        assert!((0.0..=1.0).contains(&0.5));
        assert!((0.0..=1.0).contains(&1.0));
        assert!(!(0.0..=1.0).contains(&-0.1));
        assert!(!(0.0..=1.0).contains(&1.1));
    }

    #[test]
    fn disabled_aviationstack_still_builds_adsb_client() {
        twitch_1337_core::install_crypto_provider();
        let settings = Settings::compiled_defaults().aviationstack;

        let client = build_aviation_client(None, &settings).expect("aviation client");

        assert!(!client.aviationstack_enabled());
    }

    #[test]
    fn enabled_aviationstack_attaches_metadata_config() {
        twitch_1337_core::install_crypto_provider();
        let mut settings = Settings::compiled_defaults().aviationstack;
        settings.enabled = true;
        let bootstrap = AviationstackBootstrap {
            api_key: SecretString::new("test-key".to_owned().into()),
        };

        let client = build_aviation_client(Some(&bootstrap), &settings).expect("aviation client");

        assert!(client.aviationstack_enabled());
    }
}
