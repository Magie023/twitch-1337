//! Schedules engine.
//!
//! One `run_orchestrator` task watches `SettingsStore::change_notify`
//! and reconciles a `HashMap<String, (CancellationToken, Schedule)>` of
//! per-schedule child tasks. Each child computes its next fire from the
//! absolute clock, sleeps until then, fires, repeats. Cancellation is
//! cooperative — in-flight `say()` is never aborted mid-call.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::schedule::{Schedule, TelemetryStore};
use crate::settings::SettingsHandle;
use crate::twitch::ChatSender;
use crate::util::clock::Clock;

#[instrument(skip_all)]
pub async fn run_orchestrator<T, L>(
    sender: Arc<ChatSender<T, L>>,
    settings: SettingsHandle,
    change_notify: Arc<Notify>,
    telemetry: Arc<TelemetryStore>,
    channel: String,
    shutdown: Arc<Notify>,
    clock: Arc<dyn Clock>,
) where
    T: Transport,
    L: LoginCredentials,
{
    info!("Schedules orchestrator started");
    let mut running: HashMap<String, (CancellationToken, Schedule)> = HashMap::new();

    // Enable the notification future BEFORE the initial reconcile so that any
    // `notify_waiters()` fired between reconcile returning and the first loop
    // iteration is not lost.
    let mut notified = Box::pin(change_notify.notified());
    notified.as_mut().enable();

    reconcile(
        &sender,
        &settings,
        &telemetry,
        &channel,
        &clock,
        &mut running,
    );

    loop {
        tokio::select! {
            () = &mut notified => {
                reconcile(&sender, &settings, &telemetry, &channel, &clock, &mut running);
                notified = Box::pin(change_notify.notified());
                notified.as_mut().enable();
            }
            () = shutdown.notified() => {
                info!("Schedules orchestrator: shutdown");
                for (name, (cancel, _)) in running.drain() {
                    info!(schedule = %name, "cancelling on shutdown");
                    cancel.cancel();
                }
                telemetry.flush().await;
                return;
            }
        }
    }
}

fn reconcile<T, L>(
    sender: &Arc<ChatSender<T, L>>,
    settings: &SettingsHandle,
    telemetry: &Arc<TelemetryStore>,
    channel: &str,
    clock: &Arc<dyn Clock>,
    running: &mut HashMap<String, (CancellationToken, Schedule)>,
) where
    T: Transport,
    L: LoginCredentials,
{
    let desired: HashMap<String, Schedule> = settings
        .load()
        .schedules
        .iter()
        .filter(|s| s.enabled)
        .map(|s| (s.name.clone(), s.clone()))
        .collect();

    // Stop tasks that vanished or changed content (including disabled).
    running.retain(|name, (cancel, captured)| match desired.get(name) {
        None => {
            info!(schedule = %name, "stopping (removed or disabled)");
            cancel.cancel();
            false
        }
        Some(want) if want != captured => {
            info!(schedule = %name, "restarting (content changed)");
            cancel.cancel();
            false
        }
        Some(_) => true,
    });

    // Spawn tasks for additions.
    for (name, schedule) in desired {
        running.entry(name.clone()).or_insert_with(|| {
            let cancel = CancellationToken::new();
            let child_cancel = cancel.clone();
            let captured = schedule.clone();
            let sender = sender.clone();
            let telemetry = telemetry.clone();
            let channel = channel.to_owned();
            let clock = clock.clone();
            tokio::spawn(run_schedule_task(
                schedule,
                sender,
                telemetry,
                channel,
                clock,
                child_cancel,
            ));
            (cancel, captured)
        });
    }
}

#[instrument(skip(sender, telemetry, channel, clock, cancel), fields(schedule = %schedule.name))]
async fn run_schedule_task<T, L>(
    schedule: Schedule,
    sender: Arc<ChatSender<T, L>>,
    telemetry: Arc<TelemetryStore>,
    channel: String,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
) where
    T: Transport,
    L: LoginCredentials,
{
    use chrono_tz::Europe::Berlin;
    loop {
        let now_berlin = clock.now_utc().with_timezone(&Berlin);
        let Some(next_berlin) = schedule.next_active_fire(now_berlin) else {
            info!("past end_date, exiting");
            return;
        };
        let next_utc = next_berlin.with_timezone(&chrono::Utc);
        tokio::select! {
            () = clock.sleep_until(next_utc) => {}
            () = cancel.cancelled() => {
                info!("cancelled, exiting");
                return;
            }
        }
        // Re-check cancellation after wakeup: tokio::select! may pick the
        // sleep branch even if cancel was signaled concurrently.
        if cancel.is_cancelled() {
            info!("cancelled after wakeup, exiting");
            return;
        }
        // Fire.
        sender.say(channel.clone(), schedule.message.clone()).await;
        telemetry.record_fire(&schedule.name, clock.now_utc()).await;
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    #[test]
    fn cancellation_token_clone_shares_state() {
        let parent = CancellationToken::new();
        let child = parent.clone();
        parent.cancel();
        assert!(child.is_cancelled());
    }
}
