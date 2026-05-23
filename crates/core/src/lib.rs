//! Twitch IRC bot library crate.
//!
//! The binary (`src/main.rs`) reads config, builds a production
//! `TwitchIRCClient`, and runs the bot. Integration tests (`tests/`) use a
//! fake transport, fake clock, and fake LLM against the same handlers.

pub mod ai;
pub mod aviation;
pub mod commands;
pub mod config;
pub mod cooldown;
pub mod database;
pub mod doener;
pub mod llm_factory;
pub mod ping;
pub mod settings;
pub mod suspend;
pub mod twitch;
pub mod util;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use eyre::{Result, WrapErr as _};
use llm::LlmClient;
use tokio::sync::{mpsc::UnboundedReceiver, oneshot};
use tracing::info;
use twitch_irc::{
    TwitchIRCClient,
    login::{LoginCredentials, RefreshingLoginCredentials},
    message::ServerMessage,
    transport::Transport,
};

use crate::{
    aviation::AviationClient,
    config::Configuration,
    twitch::handlers::{
        spawn::{SpawnDeps, spawn_handlers},
        tracker_1337::{TARGET_HOUR, TARGET_MINUTE},
    },
    twitch::whisper::WhisperSender,
    util::clock::Clock,
};

pub type AuthenticatedLoginCredentials =
    RefreshingLoginCredentials<crate::twitch::token_storage::FileBasedTokenStorage>;

/// Generic alias for any authenticated Twitch IRC client. The production
/// default is `SecureTCPTransport` + file-backed refreshing credentials.
pub type AuthenticatedTwitchClient<
    T = twitch_irc::SecureTCPTransport,
    L = AuthenticatedLoginCredentials,
> = TwitchIRCClient<T, L>;

pub use ai::chat_history::{
    ChatHistory, ChatHistoryBuffer, ChatHistoryEntry, ChatHistoryPage, ChatHistoryQuery,
    ChatHistorySource, DEFAULT_HISTORY_LENGTH, MAX_HISTORY_LENGTH, MAX_TOOL_RESULT_MESSAGES,
};
pub use config::{load_configuration, validate_config};
pub use twitch::{
    handlers::tracker_1337::{PersonalBest, load_leaderboard},
    setup::{setup_and_verify_twitch_client, setup_twitch_client},
    token_storage::FileBasedTokenStorage,
};
pub use util::{
    APP_USER_AGENT, ensure_data_dir, get_config_path, get_data_dir, install_crypto_provider,
    parse_flight_duration, resolve_berlin_time, telemetry::install_tracing, truncate_response,
};

/// Test-overridable services injected into [`run_bot`].
///
/// Production wires real implementations; integration tests wire fakes.
pub struct Services {
    pub clock: Arc<dyn Clock>,
    /// Bootstrap-only AI credentials from `config.toml`. Presence gates the
    /// AI feature; all knobs come from the settings store.
    pub ai_bootstrap: Option<crate::config::AiBootstrap>,
    pub llm: Option<Arc<dyn LlmClient>>,
    pub aviation: Option<AviationClient>,
    pub doener: Arc<crate::doener::DoeneratlasClient>,
    pub whisper: Option<Arc<dyn WhisperSender>>,
    pub data_dir: PathBuf,
    /// Shared dashboard-managed runtime settings. Constructed by the bin so
    /// the same `Arc` is handed to the IRC command handlers (via `SpawnDeps`)
    /// and to `WebState` (for the dashboard settings page).
    pub settings: crate::settings::SettingsHandle,
    /// Owner of `settings.ron`. The bin keeps an `Arc` for `WebState` (the
    /// POST handler calls `.apply()` / `.reset()`); the bot itself only
    /// reads via the handle.
    pub settings_store: Arc<crate::settings::SettingsStore>,
    /// Optional override for the 7TV emote glossary TOML. Production leaves
    /// this `None` so the baked glossary is used; integration tests inject
    /// custom fixtures.
    pub emote_glossary_override: Option<String>,
    /// Shared connectivity flag flipped by the latency monitor. Read by the
    /// web dashboard's `/healthz` endpoint.
    pub irc_connected: Arc<std::sync::atomic::AtomicBool>,
    /// Optional callback that spawns the embedded web dashboard task.
    ///
    /// `core` no longer depends on the `web` crate (the cycle would break
    /// builds), so the binary is the only place that can construct
    /// `WebState` + spawn `run_web`. Tests leave this `None` and the bot
    /// runs without a dashboard.
    ///
    /// The closure receives the shared shutdown `Notify` (so axum's
    /// graceful-shutdown future fires when handlers wind down) and is
    /// expected to return a `JoinHandle` that resolves when the web task
    /// exits.
    pub web_spawner: Option<WebSpawner>,
    /// Sender half of the ping actor channel, wrapped in `Arc` so it can be
    /// cheaply cloned into `WebState`. The matching receiver and watch pair
    /// are consumed by `spawn_handlers` to start the ping-actor task.
    pub ping_actor_tx: Arc<tokio::sync::mpsc::Sender<crate::ping::PingCommand>>,
    /// Receiver half of the ping actor channel. Consumed exactly once by
    /// `spawn_handlers`.
    pub ping_actor_rx: tokio::sync::mpsc::Receiver<crate::ping::PingCommand>,
    /// Owned `PingManager` loaded from disk. Transferred into the ping actor.
    pub ping_manager: crate::ping::PingManager,
    /// Watch sender for the live ping name set.
    pub ping_names_tx: tokio::sync::watch::Sender<HashSet<String>>,
    /// Watch receiver for the live ping name set.
    pub ping_names_rx: tokio::sync::watch::Receiver<HashSet<String>>,
    /// Shared v2 memory store. Constructed by the bin so the bot's `!ai`
    /// turn / dreamer ritual and the dashboard memory editor write through
    /// the *same* per-path mutex map. Two independent stores against the
    /// same on-disk tree would silently race past each other's locks.
    pub memory_store: crate::ai::memory::store::MemoryStore,
    /// Shared 1337 leaderboard. Created by the bin before the web spawner so
    /// the same `Arc` is handed to both `WebState` and the IRC tracker handler
    /// (via `SpawnDeps`). `run_bot` moves this into `SpawnDeps`; tests pass an
    /// empty map.
    pub leaderboard: Arc<
        tokio::sync::RwLock<
            std::collections::HashMap<String, crate::twitch::handlers::tracker_1337::PersonalBest>,
        >,
    >,
    /// Sender half of the flight-tracker mpsc channel, wrapped in `Arc` so it
    /// can be cheaply cloned into `WebState`. `None` when aviation is disabled.
    /// The matching receiver lives in `aviation_tracker_rx` and is consumed by
    /// `spawn_handlers` to start the flight-tracker task.
    pub aviation_tracker_tx:
        Option<Arc<tokio::sync::mpsc::Sender<crate::aviation::TrackerCommand>>>,
    /// Receiver half of the flight-tracker mpsc channel. `None` when aviation
    /// is disabled. Consumed exactly once by `spawn_handlers`.
    pub aviation_tracker_rx: Option<tokio::sync::mpsc::Receiver<crate::aviation::TrackerCommand>>,
    /// Optional test-only sink: when set, the commands handler stores the
    /// freshly built `primary_history` `Arc` here once the AI command is
    /// wired up. Production passes `None`; integration tests use this to
    /// peek at chat-history entries (display_name / user_id) for assertions.
    pub primary_history_tap:
        Option<Arc<tokio::sync::Mutex<Option<crate::ai::chat_history::ChatHistory>>>>,
}

pub type WebSpawner =
    Box<dyn FnOnce(Arc<tokio::sync::Notify>) -> tokio::task::JoinHandle<()> + Send + 'static>;

/// Run the bot until `shutdown` fires or a handler exits.
///
/// Shared entry point for `main.rs` (production) and integration tests.
/// Generic over `Transport` and `LoginCredentials` so tests can substitute a
/// `FakeTransport` without touching production code paths.
pub async fn run_bot<T, L>(
    client: Arc<TwitchIRCClient<T, L>>,
    incoming: UnboundedReceiver<ServerMessage>,
    config: Configuration,
    services: Services,
    shutdown: oneshot::Receiver<()>,
) -> Result<()>
where
    T: Transport + Send + Sync + 'static,
    L: LoginCredentials + Send + Sync + 'static,
{
    let Services {
        clock,
        ai_bootstrap: _,
        llm,
        aviation,
        doener,
        whisper,
        data_dir,
        settings,
        settings_store: _,
        emote_glossary_override,
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
        primary_history_tap,
    } = services;

    // Clone the sender out of the Arc for SpawnDeps.
    let ping_actor_tx_inner = (*ping_actor_tx).clone();
    let aviation_tracker_tx_inner = aviation_tracker_tx.as_ref().map(|a| (**a).clone());

    let schedules_enabled = !config.schedules.is_empty();

    let suspension_manager = Arc::new(suspend::SuspensionManager::new());

    // Aviation is consumed by the flight tracker; clone first so commands (!up/!fl) also get it.
    let aviation_for_commands = aviation.clone();

    let ai_memory_v2 = crate::ai::command::build_ai_memory_v2(
        config.ai.is_some(),
        &settings.load_full(),
        memory_store,
    )
    .await?;
    let transcript = ai_memory_v2.as_ref().map(|m| m.transcript.clone());

    // 7TV emote provider: built once at startup so malformed glossary TOML
    // (baked or test-injected) fails fast instead of silently disabling emotes.
    // Presence of [ai.emotes] is checked here; all live knobs are re-read per
    // call via SettingsHandle inside the provider.
    let emote_provider = match (llm.as_ref(), config.ai.as_ref()) {
        (Some(_), Some(_)) => {
            if settings.load().ai.emotes.is_some() {
                let glossary_toml = emote_glossary_override
                    .as_deref()
                    .unwrap_or(crate::twitch::seventv::BAKED_GLOSSARY_TOML);
                let provider = crate::twitch::seventv::SevenTvEmoteProvider::new(
                    settings.clone(),
                    glossary_toml,
                )
                .wrap_err("Failed to initialize 7TV emote provider")?;
                tracing::info!("7TV emote glossary prompt grounding enabled");
                Some(Arc::new(provider))
            } else {
                None
            }
        }
        _ => None,
    };

    // Clone before moving into SpawnDeps so the dreamer ritual can also use them.
    let ai_memory_v2_for_ritual = ai_memory_v2.clone();
    let llm_for_ritual = llm.clone();
    let ai_present_for_ritual = config.ai.is_some();
    let settings_for_ritual = settings.clone();
    let channel_for_ritual = config.twitch.channel.clone();

    let handlers = spawn_handlers(SpawnDeps {
        client,
        incoming,
        config,
        clock,
        data_dir,
        doener,
        leaderboard,
        ping_actor_tx: ping_actor_tx_inner,
        ping_actor_rx,
        ping_manager,
        ping_names_tx,
        ping_names_rx,
        suspension_manager,
        llm,
        ai_memory_v2,
        transcript,
        whisper,
        aviation,
        aviation_for_commands,
        aviation_tracker_tx: aviation_tracker_tx_inner,
        aviation_tracker_rx,
        emote_provider,
        irc_connected: irc_connected.clone(),
        settings: settings.clone(),
        primary_history_tap,
    });

    let shutdown_notify = handlers.shutdown_notify.clone();

    // Optional embedded web dashboard. The bin builds and supplies the
    // spawn closure so this crate stays independent of `twitch_1337_web`.
    let web_handle = web_spawner.map(|spawner| spawner(shutdown_notify.clone()));

    // Daily dreamer ritual. Re-reads settings each loop iteration so dashboard
    // edits to ai.dreamer.* apply on the next scheduled run without a restart.
    if let (Some(llm), Some(mem)) = (llm_for_ritual.as_ref(), &ai_memory_v2_for_ritual)
        && ai_present_for_ritual
    {
        crate::ai::memory::ritual::spawn_ritual(
            llm.clone(),
            mem.store.clone(),
            mem.transcript.clone(),
            settings_for_ritual.clone(),
            channel_for_ritual,
            shutdown_notify.clone(),
        );
        tracing::info!("Daily AI memory dreamer ritual spawned (live settings)");
    }

    if schedules_enabled {
        info!(
            "Bot running with continuous connection. Handlers: Config watcher, 1337 tracker, Generic commands, Scheduled messages, Latency monitor, Flight tracker"
        );
        info!("Scheduled messages: Loaded from config.toml, reloads on file change");
    } else {
        info!(
            "Bot running with continuous connection. Handlers: 1337 tracker, Generic commands, Latency monitor, Flight tracker"
        );
    }
    info!(
        "1337 tracker scheduled to run daily at {}:{:02} (Europe/Berlin)",
        TARGET_HOUR,
        TARGET_MINUTE - 1
    );
    let ping_actor_handle =
        crate::twitch::handlers::spawn::await_shutdown(handlers, shutdown).await;

    // Drop the Arc<Sender> clone held in Services so detached tasks are the
    // only remaining senders. The ping actor drains once all senders drop.
    drop(ping_actor_tx);

    // After handler shutdown, drain the web task. The graceful shutdown future
    // is wired to the same `shutdown_notify` notified by `await_shutdown`, so
    // axum::serve has already begun winding down.
    if let Some(handle) = web_handle
        && let Err(e) = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await
    {
        tracing::warn!(target: "twitch_1337_web", ?e, "Web task did not shut down within 5s");
    }

    // Best-effort drain: actor exits once all Arc<Sender> clones drop. A short
    // timeout avoids blocking shutdown when a stray clone lingers; the runtime
    // will clean up on exit.
    if let Err(e) =
        tokio::time::timeout(std::time::Duration::from_millis(500), ping_actor_handle).await
    {
        tracing::debug!(
            ?e,
            "Ping actor did not drain within 500ms; runtime will clean up"
        );
    }

    info!("Bot shutdown complete");
    Ok(())
}
