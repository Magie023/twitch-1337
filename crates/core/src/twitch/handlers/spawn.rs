//! Handler spawn factory.
//!
//! `run_bot` (in `src/lib.rs`) calls [`spawn_handlers`] once with everything
//! the long-running tasks need. The returned [`HandlerSet`] owns every
//! `JoinHandle` plus the shared `Arc<Notify>` post-spawn code uses to drain
//! the scheduled-message handler on shutdown.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32},
    },
};

use llm::LlmClient;
use tokio::{
    sync::{Notify, RwLock, broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
    time::{Duration, timeout},
};
use tracing::{error, info, warn};
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::ServerMessage, transport::Transport,
};

use crate::{
    ai,
    aviation::{self, AviationClient},
    config::Configuration,
    ping::{PingCommand, PingManager, run_ping_actor},
    suspend::SuspensionManager,
    twitch::{
        ChatSender,
        handlers::{
            commands::{CommandHandlerConfig, run_generic_command_handler},
            latency::run_latency_handler,
            router::run_message_router,
            schedule_runner::run_orchestrator,
            tracker_1337::{PersonalBest, run_1337_handler},
        },
        whisper::WhisperSender,
    },
    util::clock::Clock,
};

/// All `JoinHandle`s plus the shared state `run_bot` keeps after spawn.
pub(crate) struct HandlerSet {
    pub router: JoinHandle<()>,
    pub latency: JoinHandle<()>,
    pub tracker_1337: JoinHandle<()>,
    pub generic_commands: JoinHandle<()>,
    pub flight_tracker: JoinHandle<()>,
    pub ping_actor: JoinHandle<()>,
    pub scheduled_messages: JoinHandle<()>,
    /// Shared notify so consolidation/shutdown code can drain in-flight
    /// scheduled-message sends. Cloned, not consumed.
    pub shutdown_notify: Arc<Notify>,
}

/// Inputs for [`spawn_handlers`]. Grouped by handler.
pub(crate) struct SpawnDeps<T: Transport, L: LoginCredentials> {
    // Shared.
    pub client: Arc<TwitchIRCClient<T, L>>,
    pub incoming: tokio::sync::mpsc::UnboundedReceiver<ServerMessage>,
    pub config: Configuration,
    pub clock: Arc<dyn Clock>,
    pub data_dir: PathBuf,
    pub doener: Arc<crate::doener::DoeneratlasClient>,

    // 1337 tracker.
    pub leaderboard: Arc<RwLock<HashMap<String, PersonalBest>>>,

    // Generic commands (ping actor).
    pub ping_actor_tx: mpsc::Sender<PingCommand>,
    pub ping_actor_rx: mpsc::Receiver<PingCommand>,
    pub ping_manager: PingManager,
    pub ping_names_tx: watch::Sender<std::collections::HashSet<String>>,
    pub ping_names_rx: watch::Receiver<std::collections::HashSet<String>>,
    pub suspension_manager: Arc<SuspensionManager>,
    pub llm: Option<Arc<dyn LlmClient>>,
    pub ai_memory_v2: Option<ai::command::AiMemoryV2>,
    pub transcript: Option<crate::ai::memory::transcript::TranscriptWriter>,
    pub whisper: Option<Arc<dyn WhisperSender>>,

    // Flight tracker.
    pub aviation: Option<AviationClient>,
    pub aviation_for_commands: Option<AviationClient>,
    /// Pre-created sender half of the flight-tracker channel. Must be `Some`
    /// when `aviation` is `Some`, and `None` otherwise.
    pub aviation_tracker_tx: Option<mpsc::Sender<aviation::TrackerCommand>>,
    /// Pre-created receiver half of the flight-tracker channel. Consumed by
    /// `spawn_handlers` to start the flight-tracker task. Must be `Some` when
    /// `aviation` is `Some`, and `None` otherwise.
    pub aviation_tracker_rx: Option<mpsc::Receiver<aviation::TrackerCommand>>,

    // AI emote grounding.
    pub emote_provider: Option<Arc<crate::twitch::seventv::SevenTvEmoteProvider>>,

    // Shared IRC connectivity flag (latency monitor flips, web /healthz reads).
    pub irc_connected: Arc<AtomicBool>,

    // Dashboard-managed runtime settings (cooldowns, pings.cooldown, pings.public).
    pub settings: crate::settings::SettingsHandle,

    /// Settings store handle, used by the schedule sync task to subscribe to
    /// change notifications.
    pub settings_store: Arc<crate::settings::SettingsStore>,

    /// Optional test-only sink: when set, the commands handler stores the
    /// freshly built primary `ChatHistory` here so integration tests can
    /// peek at chat-history entries. Production wires `None`.
    pub primary_history_tap:
        Option<Arc<tokio::sync::Mutex<Option<crate::ai::chat_history::ChatHistory>>>>,

    /// Per-schedule runtime telemetry. Shared with the dashboard so card
    /// chips ("12 today", "12m ago") read live data.
    pub telemetry: Arc<crate::schedule::TelemetryStore>,
    /// Process start instant, captured in `run_bot`; forwarded to `!v`.
    pub started_at: std::time::Instant,
}

/// Spawn every long-running handler task in the order they currently
/// appear in `run_bot`. Returns a [`HandlerSet`] owning all handles plus
/// shared `Notify` / `tracker_tx`.
pub(crate) fn spawn_handlers<T, L>(deps: SpawnDeps<T, L>) -> HandlerSet
where
    T: Transport + Send + Sync + 'static,
    L: LoginCredentials + Send + Sync + 'static,
{
    let SpawnDeps {
        client,
        incoming,
        config,
        clock,
        data_dir,
        doener,
        leaderboard,
        ping_actor_tx,
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
        aviation_tracker_tx,
        aviation_tracker_rx,
        emote_provider,
        irc_connected,
        settings,
        settings_store,
        primary_history_tap,
        telemetry,
        started_at,
    } = deps;

    let ping_actor = tokio::spawn(run_ping_actor(ping_actor_rx, ping_manager, ping_names_tx));

    // One sanitizing chat sender shared by every PRIVMSG-emitting handler.
    // Tasks that need raw IRC framing (latency PING/PONG, JOINs) keep `client`.
    let chat_sender = ChatSender::new(client.clone());

    // Flight tracker: spawned first so `tracker_tx` exists for the command handler.
    // The channel is pre-created by the caller (production: main.rs; tests: TestBotBuilder)
    // so the sender Arc can be shared with WebState before handlers are spawned.
    let (tracker_tx, flight_tracker) = match (aviation, aviation_tracker_rx) {
        (Some(av), Some(rx)) => {
            let tx = aviation_tracker_tx.expect("tracker_tx must be Some when aviation is Some");
            let handle = tokio::spawn({
                let sender = chat_sender.clone();
                let channel = config.twitch.channel.clone();
                let dir = data_dir.clone();
                let clk = clock.clone();
                async move {
                    aviation::run_flight_tracker(rx, sender, channel, av, dir, clk).await;
                }
            });
            (Some(tx), handle)
        }
        _ => (None, tokio::spawn(std::future::pending::<()>())),
    };

    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(100);

    let router = tokio::spawn(run_message_router(incoming, broadcast_tx.clone()));

    // Notify lets the scheduled-message handler drain in-flight sends before exiting.
    let shutdown_notify = Arc::new(Notify::new());

    // Schedules orchestrator: subscribes to SettingsStore::change_notify
    // and reconciles per-schedule tasks. No cache, no 30s poll.
    // `telemetry` comes from the caller (constructed in run_bot so WebState
    // and the orchestrator share the same Arc).
    let scheduled_messages = tokio::spawn({
        let sender = chat_sender.clone();
        let settings = settings.clone();
        let change_notify = settings_store.change_notify();
        let telemetry = telemetry.clone();
        let channel = config.twitch.channel.clone();
        let notify = shutdown_notify.clone();
        let clk = clock.clone();
        async move {
            run_orchestrator(
                sender,
                settings,
                change_notify,
                telemetry,
                channel,
                notify,
                clk,
            )
            .await;
        }
    });

    let latency_value = Arc::new(AtomicU32::new(settings.load().twitch.expected_latency));

    let latency = tokio::spawn({
        let client = client.clone();
        let btx = broadcast_tx.clone();
        let lat = latency_value.clone();
        let conn = irc_connected.clone();
        async move {
            run_latency_handler(client, btx, lat, conn).await;
        }
    });

    let _transcript_tap = if let Some(t) = transcript {
        let rx = broadcast_tx.subscribe();
        let w = Arc::new(t);
        let ch = config.twitch.channel.clone();
        Some(tokio::spawn(async move {
            crate::twitch::handlers::transcript::run_transcript_tap(rx, w, ch).await;
        }))
    } else {
        None
    };

    let tracker_1337 = tokio::spawn({
        let btx = broadcast_tx.clone();
        let sender = chat_sender.clone();
        let channel = config.twitch.channel.clone();
        let lat = latency_value.clone();
        let lb = leaderboard.clone();
        let clk = clock.clone();
        let dd = data_dir.clone();
        async move {
            run_1337_handler(btx, sender, channel, lat, lb, clk, dd).await;
        }
    });

    let generic_commands = tokio::spawn({
        let btx = broadcast_tx.clone();
        let client = client.clone();
        async move {
            let (admin_channel, ai_channel) = {
                let s = settings.load();
                (s.twitch.admin_channel.clone(), s.twitch.ai_channel.clone())
            };
            run_generic_command_handler(CommandHandlerConfig {
                broadcast_tx: btx,
                client,
                ai_config: config.ai.clone(),
                llm,
                ai_memory_v2,
                leaderboard,
                ping_actor_tx,
                ping_names_rx,
                settings: settings.clone(),
                tracker_tx,
                aviation_client: aviation_for_commands,
                whisper,
                admin_channel,
                ai_channel,
                bot_username: config.twitch.username.clone(),
                channel: config.twitch.channel.clone(),
                data_dir: data_dir.clone(),
                doener: doener.clone(),
                suspension_manager: suspension_manager.clone(),
                emote_provider,
                primary_history_tap,
                started_at,
            })
            .await;
        }
    });

    HandlerSet {
        router,
        latency,
        tracker_1337,
        generic_commands,
        flight_tracker,
        ping_actor,
        scheduled_messages,
        shutdown_notify,
    }
}

/// Why [`await_shutdown`] returned. Lets the caller distinguish an intentional
/// stop from a fault so the admin-channel announce matches reality.
pub(crate) enum ShutdownOutcome {
    /// The shutdown signal (Ctrl+C / SIGTERM) fired — intentional, graceful exit.
    Graceful,
    /// A handler task exited unexpectedly — the bot is faulting, not a clean stop.
    HandlerExited,
}

/// Awaits whichever happens first: shutdown signal, or any handler exiting.
///
/// On shutdown, notifies `shutdown_notify` (so the scheduled-message handler
/// drains in-flight `say()` calls) and waits up to 5s for the scheduled
/// handler before returning. Returns the ping-actor handle plus a
/// [`ShutdownOutcome`] telling the caller whether the exit was graceful.
///
/// The scheduled-message handler subscribes directly to
/// `SettingsStore::change_notify` and reconciles per-schedule tasks
/// internally — there is no separate settings-sync task.
pub(crate) async fn await_shutdown(
    handlers: HandlerSet,
    shutdown: oneshot::Receiver<()>,
) -> (tokio::task::JoinHandle<()>, ShutdownOutcome) {
    let HandlerSet {
        router,
        latency,
        tracker_1337,
        generic_commands,
        flight_tracker,
        mut ping_actor,
        scheduled_messages: mut sched,
        shutdown_notify,
    } = handlers;

    let outcome = tokio::select! {
        _ = shutdown => {
            info!("Shutdown signal received, exiting gracefully");
            // Always wake any waiters (scheduled messages, web dashboard).
            shutdown_notify.notify_waiters();
            if let Err(e) = timeout(Duration::from_secs(5), &mut sched).await {
                warn!(?e, "Scheduled message handler did not shut down within 5s");
            }
            ShutdownOutcome::Graceful
        }
        result = router => { error!("Message router exited unexpectedly: {result:?}"); ShutdownOutcome::HandlerExited }
        result = tracker_1337 => { error!("1337 handler exited unexpectedly: {result:?}"); ShutdownOutcome::HandlerExited }
        result = generic_commands => { error!("Generic Command Handler exited unexpectedly: {result:?}"); ShutdownOutcome::HandlerExited }
        result = latency => { error!("Latency handler exited unexpectedly: {result:?}"); ShutdownOutcome::HandlerExited }
        result = flight_tracker => { error!("Flight tracker exited unexpectedly: {result:?}"); ShutdownOutcome::HandlerExited }
        result = &mut ping_actor => { error!("Ping actor exited unexpectedly: {result:?}"); ShutdownOutcome::HandlerExited }
        result = &mut sched => { error!("Scheduled message handler exited unexpectedly: {result:?}"); ShutdownOutcome::HandlerExited }
    };

    (ping_actor, outcome)
}
