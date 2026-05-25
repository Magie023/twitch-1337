use std::sync::Arc;

use tokio::sync::{Notify, RwLock};
use tracing::{debug, info, instrument, warn};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::database;
use crate::settings::{SettingsHandle, SettingsStore};
use crate::twitch::ChatSender;
use crate::util::clock::Clock;

/// Subscribe to `SettingsStore`'s change `Notify` and regenerate the
/// `ScheduleCache` whenever the schedules vec resolves to a different list
/// than the cache currently holds. Initial population also happens on the
/// first iteration: the task primes the cache once at start before waiting
/// to guarantee we don't miss an apply that races our subscription.
#[instrument(skip(settings, store, cache, shutdown))]
pub async fn run_schedule_settings_sync(
    settings: SettingsHandle,
    store: Arc<SettingsStore>,
    cache: Arc<RwLock<database::ScheduleCache>>,
    shutdown: Arc<Notify>,
) {
    info!("Schedule settings sync task started");
    let change = store.change_notify();
    // Prime the cache once before waiting so the initial state matches
    // whatever was loaded at startup.
    regenerate(&settings, &cache).await;
    loop {
        // Pre-register the Notified future BEFORE entering regenerate() so a
        // notify_waiters() fired during regenerate() is captured rather than
        // dropped. tokio::sync::Notify only wakes futures that are already
        // polled or explicitly enabled — without enable(), a wakeup that
        // races our subscription is silently lost (PR #227 review F1).
        let mut notified = Box::pin(change.notified());
        notified.as_mut().enable();
        tokio::select! {
            () = &mut notified => {
                regenerate(&settings, &cache).await;
            }
            () = shutdown.notified() => {
                info!("Schedule settings sync: shutdown received");
                return;
            }
        }
    }
}

async fn regenerate(settings: &SettingsHandle, cache: &Arc<RwLock<database::ScheduleCache>>) {
    let new_list = crate::settings::schedules::build_schedules(&settings.load().schedules);
    let mut g = cache.write().await;
    if g.schedules != new_list {
        let old_count = g.schedules.len();
        g.update(new_list);
        info!(
            old_count,
            new_count = g.schedules.len(),
            version = g.version,
            "Schedules updated from settings"
        );
    }
}

/// Run a single schedule task.
/// This task will run the schedule at its configured interval,
/// checking if it's still active before each post.
#[instrument(skip(sender, cache, channel, clock), fields(schedule = %schedule.name))]
pub(crate) async fn run_schedule_task<T, L>(
    schedule: database::Schedule,
    sender: Arc<ChatSender<T, L>>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
    channel: String,
    shutdown: Arc<tokio::sync::Notify>,
    clock: Arc<dyn Clock>,
) where
    T: Transport,
    L: LoginCredentials,
{
    use tokio::time::{Duration, sleep};

    let interval_duration = Duration::from_secs(schedule.interval.num_seconds() as u64);

    info!(
        schedule = %schedule.name,
        interval_seconds = schedule.interval.num_seconds(),
        "Schedule task started"
    );

    loop {
        // Bail on shutdown so in-flight sends below aren't torn apart when the runtime stops.
        tokio::select! {
            () = sleep(interval_duration) => {}
            () = shutdown.notified() => {
                info!(schedule = %schedule.name, "Shutdown received, stopping task");
                break;
            }
        }

        // Check if schedule still exists in cache
        let still_exists = {
            let cache_guard = cache.read().await;
            cache_guard
                .schedules
                .iter()
                .any(|s| s.name == schedule.name)
        };

        if !still_exists {
            info!(
                schedule = %schedule.name,
                "Schedule no longer in cache, stopping task"
            );
            break;
        }

        // Check if schedule is currently active (respects date range and time window)
        let now = clock.now_utc().with_timezone(&chrono_tz::Europe::Berlin);

        if !schedule.is_active(now) {
            debug!(
                schedule = %schedule.name,
                "Schedule not active at current time, skipping post"
            );
            continue;
        }

        // Post the message
        info!(
            schedule = %schedule.name,
            message = %schedule.message,
            "Posting scheduled message"
        );

        sender.say(channel.clone(), schedule.message.clone()).await;
        debug!(schedule = %schedule.name, "Scheduled message posted");
    }

    info!(schedule = %schedule.name, "Schedule task exiting");
}

/// Dynamic scheduled message handler that monitors cache for changes.
/// Spawns and stops tasks dynamically based on cache updates.
#[instrument(skip(sender, cache, channel, clock))]
pub async fn run_scheduled_message_handler<T, L>(
    sender: Arc<ChatSender<T, L>>,
    cache: Arc<tokio::sync::RwLock<database::ScheduleCache>>,
    channel: String,
    shutdown: Arc<tokio::sync::Notify>,
    clock: Arc<dyn Clock>,
) where
    T: Transport,
    L: LoginCredentials,
{
    use std::collections::HashMap;
    use tokio::task::JoinHandle;
    use tokio::time::{Duration, interval};

    info!("Dynamic scheduled message handler started");

    // Track running tasks by schedule name. We keep a cloned `Schedule`
    // alongside the handle so a hot-reload that edits an existing schedule's
    // content (without renaming) still aborts the stale task and respawns.
    // Without this, `retain` keyed only on name silently dropped content
    // changes — see issue #39.
    let mut running_tasks: HashMap<String, (JoinHandle<()>, database::Schedule)> = HashMap::new();
    let mut current_version = 0u64;

    // Monitor cache for changes every 30 seconds
    let mut check_interval = interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            _ = check_interval.tick() => {}
            () = shutdown.notified() => {
                info!("Scheduled message handler: shutdown received, awaiting children");
                for (name, (handle, _)) in running_tasks.drain() {
                    handle.abort();
                    if let Err(e) = handle.await
                        && !e.is_cancelled()
                    {
                        warn!(schedule = %name, error = ?e, "Schedule task join error");
                    }
                }
                return;
            }
        }

        let (schedules, version) = {
            let cache_guard = cache.read().await;
            (cache_guard.schedules.clone(), cache_guard.version)
        };

        // Check if cache version has changed
        if version != current_version {
            info!(
                old_version = current_version,
                new_version = version,
                schedule_count = schedules.len(),
                "Cache version changed, updating tasks"
            );

            current_version = version;

            // Build set of schedule names that should be running
            let desired_schedules: HashMap<String, database::Schedule> =
                schedules.into_iter().map(|s| (s.name.clone(), s)).collect();

            // Stop tasks for schedules that no longer exist OR whose content
            // changed. Comparing the captured `Schedule` against the desired
            // one catches in-place edits to message/interval/active window
            // that share a name — `or_insert_with` below would otherwise be a
            // no-op for the changed entry and the stale task keeps firing.
            running_tasks.retain(
                |name, (handle, captured)| match desired_schedules.get(name) {
                    None => {
                        info!(schedule = %name, "Stopping task for removed schedule");
                        handle.abort();
                        false
                    }
                    Some(desired) if desired != captured => {
                        info!(schedule = %name, "Restarting task for changed schedule");
                        handle.abort();
                        false
                    }
                    Some(_) => true,
                },
            );

            // Start tasks for new (or just-aborted) schedules.
            for (name, schedule) in desired_schedules {
                let channel = channel.clone();
                let shutdown = shutdown.clone();
                let clock = clock.clone();
                running_tasks.entry(name.clone()).or_insert_with(|| {
                    info!(schedule = %name, "Starting task for new schedule");
                    let captured = schedule.clone();
                    let handle = tokio::spawn(run_schedule_task(
                        schedule,
                        sender.clone(),
                        cache.clone(),
                        channel,
                        shutdown,
                        clock,
                    ));
                    (handle, captured)
                });
            }

            info!(active_tasks = running_tasks.len(), "Task update complete");
        }
    }
}

#[cfg(test)]
mod sync_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::{Notify, RwLock};

    use crate::database;
    use crate::settings::{
        Actor, FileAuditLog, ScheduleSettings, SettingsStore, audit::MemoryAuditLog,
        overrides::SettingsOverrides,
    };

    #[tokio::test]
    async fn settings_apply_bumps_schedule_cache() {
        let dir = tempfile::tempdir().expect("tmp");
        let audit = Arc::new(MemoryAuditLog::default());
        let (store, settings_handle) =
            SettingsStore::open(dir.path(), audit, "main").expect("open");
        let cache = Arc::new(RwLock::new(database::ScheduleCache::new()));
        let shutdown = Arc::new(Notify::new());

        let cache_for_task = cache.clone();
        let store_for_task = store.clone();
        let shutdown_for_task = shutdown.clone();
        let handle_for_task = settings_handle.clone();
        let task = tokio::spawn(async move {
            super::run_schedule_settings_sync(
                handle_for_task,
                store_for_task,
                cache_for_task,
                shutdown_for_task,
            )
            .await;
        });

        // Yield so the sync task subscribes before we apply.
        tokio::time::sleep(Duration::from_millis(50)).await;

        store
            .apply(
                SettingsOverrides {
                    schedules: Some(vec![ScheduleSettings {
                        name: "noon".into(),
                        message: "hi".into(),
                        interval: "01:00".into(),
                        enabled: true,
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                Actor {
                    user_id: "1".into(),
                    user_login: "tester".into(),
                },
            )
            .await
            .expect("apply");

        // Poll the cache up to 1s for version > 0 + 1 schedule. (The sync
        // task wakes via Notify, processes synchronously, then loops; this
        // should be well under 50ms in practice.)
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            {
                let g = cache.read().await;
                if g.version > 0 && g.schedules.len() == 1 {
                    assert_eq!(g.schedules[0].name, "noon");
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                let g = cache.read().await;
                panic!(
                    "cache did not update within 1s; version={}, len={}",
                    g.version,
                    g.schedules.len()
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
        let _ = FileAuditLog::new(dir.path().join("settings_audit.log")); // unused, silences import lint
    }

    /// Race regression: fires `notify_waiters()` while the sync task's
    /// regenerate() is mid-flight. Pre-fix, the second wakeup was dropped
    /// because the next `change.notified()` future hadn't been polled yet.
    #[tokio::test]
    async fn settings_apply_during_regenerate_is_not_lost() {
        let dir = tempfile::tempdir().expect("tmp");
        let audit = Arc::new(MemoryAuditLog::default());
        let (store, settings_handle) =
            SettingsStore::open(dir.path(), audit, "main").expect("open");
        let cache = Arc::new(RwLock::new(database::ScheduleCache::new()));
        let shutdown = Arc::new(Notify::new());

        let cache_for_task = cache.clone();
        let store_for_task = store.clone();
        let shutdown_for_task = shutdown.clone();
        let handle_for_task = settings_handle.clone();
        let task = tokio::spawn(async move {
            super::run_schedule_settings_sync(
                handle_for_task,
                store_for_task,
                cache_for_task,
                shutdown_for_task,
            )
            .await;
        });

        // Give the sync task time to register its first notified() waiter.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Burst two applies back-to-back. The second fires while the task
        // is regenerating from the first; the wakeup must not be lost.
        for (i, name) in [(1usize, "one"), (2, "two")] {
            store
                .apply(
                    SettingsOverrides {
                        schedules: Some(vec![ScheduleSettings {
                            name: name.into(),
                            message: "hi".into(),
                            interval: "01:00".into(),
                            enabled: true,
                            ..Default::default()
                        }]),
                        ..Default::default()
                    },
                    Actor {
                        user_id: format!("{i}"),
                        user_login: "tester".into(),
                    },
                )
                .await
                .expect("apply");
        }

        // Cache must converge to the *second* apply's content within 1s.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            {
                let g = cache.read().await;
                if g.schedules.len() == 1 && g.schedules[0].name == "two" {
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                let g = cache.read().await;
                panic!(
                    "cache did not converge to second apply within 1s; \
                     version={}, schedules={:?}",
                    g.version,
                    g.schedules.iter().map(|s| &s.name).collect::<Vec<_>>()
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
    }
}
