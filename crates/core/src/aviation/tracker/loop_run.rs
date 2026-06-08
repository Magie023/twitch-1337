use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::info;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::aviation::AviationClient;
use crate::twitch::ChatSender;
use crate::util::clock::Clock;

use super::{
    TrackerCommand,
    commands::{poll_all_flights_with_commands, process_command},
    debug_journal::{DEBUG_JOURNAL_KEEP_FILES, prune_debug_journals},
    schedule::next_poll_at,
    state::load_tracker_state,
};

pub async fn run_flight_tracker<T, L>(
    mut cmd_rx: mpsc::Receiver<TrackerCommand>,
    sender: Arc<ChatSender<T, L>>,
    channel: String,
    aviation_client: AviationClient,
    data_dir: PathBuf,
    clock: Arc<dyn Clock>,
) where
    T: Transport,
    L: LoginCredentials,
{
    let mut state = load_tracker_state(&data_dir).await;
    info!(flights = state.flights.len(), "Flight tracker started");

    prune_debug_journals(&data_dir, DEBUG_JOURNAL_KEEP_FILES).await;
    let mut last_journal_date = clock.now_utc().date_naive();

    loop {
        if state.flights.is_empty() {
            let Some(cmd) = cmd_rx.recv().await else {
                info!("Flight tracker command channel closed, shutting down");
                return;
            };
            process_command(
                cmd,
                &mut state,
                &sender,
                &aviation_client,
                &data_dir,
                &*clock,
            )
            .await;
        } else {
            while let Ok(cmd) = cmd_rx.try_recv() {
                process_command(
                    cmd,
                    &mut state,
                    &sender,
                    &aviation_client,
                    &data_dir,
                    &*clock,
                )
                .await;
            }

            let deferred_commands = poll_all_flights_with_commands(
                &mut state,
                &sender,
                &channel,
                &aviation_client,
                &data_dir,
                &*clock,
                Some(&mut cmd_rx),
            )
            .await;
            for cmd in deferred_commands {
                process_command(
                    cmd,
                    &mut state,
                    &sender,
                    &aviation_client,
                    &data_dir,
                    &*clock,
                )
                .await;
            }

            let now = clock.now_utc();
            // Roll the journal forward: once the date changes, drop stale files.
            let today = now.date_naive();
            if today != last_journal_date {
                prune_debug_journals(&data_dir, DEBUG_JOURNAL_KEEP_FILES).await;
                last_journal_date = today;
            }
            let next_at = next_poll_at(&state.flights, now).unwrap_or(now);
            tracing::debug!(
                next_poll_at = %next_at,
                flights = state.flights.len(),
                "Sleeping until next flight poll"
            );

            tokio::select! {
                _ = clock.sleep_until(next_at) => {}
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            process_command(
                                cmd,
                                &mut state,
                                &sender,
                                &aviation_client,
                                &data_dir,
                                &*clock,
                            )
                            .await;
                        }
                        None => {
                            info!("Flight tracker command channel closed, shutting down");
                            return;
                        }
                    }
                }
            }
        }
    }
}
