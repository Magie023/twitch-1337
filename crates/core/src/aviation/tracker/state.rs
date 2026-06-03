use std::path::Path;

use tokio::fs;
use tracing::{info, warn};

use super::{FlightIdentifier, FlightTrackerState, HexSource, TargetConfirmation};

const FLIGHTS_FILENAME: &str = "flights.ron";

pub(crate) fn clear_pending_callsign_hexes(state: &mut FlightTrackerState) -> usize {
    let mut cleared = 0;
    for flight in &mut state.flights {
        if matches!(&flight.identifier, FlightIdentifier::Callsign(_))
            && flight.last_seen.is_none()
            && flight.hex.is_some()
            && flight.hex_source != Some(HexSource::AviationStack)
        {
            flight.hex = None;
            flight.hex_source = None;
            cleared += 1;
        }
    }
    cleared
}

fn migrate_target_confirmations(state: &mut FlightTrackerState) -> usize {
    let mut migrated = 0;
    for flight in &mut state.flights {
        if flight.target_confirmation == TargetConfirmation::Pending && flight.last_seen.is_some() {
            flight.target_confirmation = match flight.identifier {
                FlightIdentifier::Hex(_) => TargetConfirmation::InferredByAssignedHex,
                FlightIdentifier::Callsign(_) => TargetConfirmation::ConfirmedByCallsign,
            };
            migrated += 1;
        }
        if flight.target_confirmation == TargetConfirmation::AircraftVisible
            && flight.last_seen.is_some()
        {
            flight.last_seen = None;
            migrated += 1;
        }
    }
    migrated
}

pub(crate) async fn load_tracker_state(data_dir: &Path) -> FlightTrackerState {
    let path = data_dir.join(FLIGHTS_FILENAME);
    match fs::read_to_string(&path).await {
        Ok(contents) => match ron::from_str::<FlightTrackerState>(&contents) {
            Ok(mut state) => {
                let cleared_pending_hexes = clear_pending_callsign_hexes(&mut state);
                let migrated_target_confirmations = migrate_target_confirmations(&mut state);
                info!(
                    flights = state.flights.len(),
                    cleared_pending_hexes,
                    migrated_target_confirmations,
                    "Loaded flight tracker state from {}",
                    path.display()
                );
                state
            }
            Err(e) => {
                warn!(error = ?e, "Failed to parse flight tracker state, starting fresh");
                FlightTrackerState::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("No flight tracker state file found, starting fresh");
            FlightTrackerState::default()
        }
        Err(e) => {
            warn!(error = ?e, "Failed to read flight tracker state, starting fresh");
            FlightTrackerState::default()
        }
    }
}

pub(crate) async fn save_tracker_state(data_dir: &Path, state: &FlightTrackerState) {
    let path = data_dir.join(FLIGHTS_FILENAME);
    match crate::util::persist::atomic_save_ron_async(state, &path).await {
        Ok(()) => tracing::debug!(
            flights = state.flights.len(),
            "Saved flight tracker state to {}",
            path.display()
        ),
        Err(e) => tracing::error!(error = ?e, "Failed to save flight tracker state"),
    }
}
