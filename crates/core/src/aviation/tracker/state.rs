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
    }
    migrated
}

/// Seed `last_visible_at` (the tracking-lost removal anchor) from `last_seen`
/// for flights persisted before the field existed. Without this, a restored
/// `AircraftVisible` flight that then goes empty would never be removed (anchor
/// stays `None`), while older state would otherwise key removal off the stale
/// `last_seen`. Returns how many flights were backfilled.
fn backfill_visible_anchor(state: &mut FlightTrackerState) -> usize {
    let mut backfilled = 0;
    for flight in &mut state.flights {
        if flight.last_visible_at.is_none() && flight.last_seen.is_some() {
            flight.last_visible_at = flight.last_seen;
            backfilled += 1;
        }
    }
    backfilled
}

pub(crate) async fn load_tracker_state(data_dir: &Path) -> FlightTrackerState {
    let path = data_dir.join(FLIGHTS_FILENAME);
    match fs::read_to_string(&path).await {
        Ok(contents) => match ron::from_str::<FlightTrackerState>(&contents) {
            Ok(mut state) => {
                let cleared_pending_hexes = clear_pending_callsign_hexes(&mut state);
                let migrated_target_confirmations = migrate_target_confirmations(&mut state);
                let backfilled_visible_anchors = backfill_visible_anchor(&mut state);
                info!(
                    flights = state.flights.len(),
                    cleared_pending_hexes,
                    migrated_target_confirmations,
                    backfilled_visible_anchors,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aviation::tracker::test_support::{dt, tracked_flight};

    #[test]
    fn migrate_target_confirmations_preserves_visible_last_seen_anchor() {
        let last_seen = dt("2026-04-18T12:01:00Z");
        let mut state = FlightTrackerState {
            flights: vec![tracked_flight()],
        };

        assert_eq!(migrate_target_confirmations(&mut state), 0);
        assert_eq!(state.flights[0].last_seen, Some(last_seen));
        assert_eq!(
            state.flights[0].target_confirmation,
            TargetConfirmation::AircraftVisible
        );
    }

    #[test]
    fn backfill_visible_anchor_seeds_from_last_seen_for_visible_flight() {
        let last_seen = dt("2026-04-18T12:01:00Z");
        let mut flight = tracked_flight();
        // Simulate state persisted before `last_visible_at` existed.
        flight.last_visible_at = None;
        assert_eq!(
            flight.target_confirmation,
            TargetConfirmation::AircraftVisible
        );
        let mut state = FlightTrackerState {
            flights: vec![flight],
        };

        assert_eq!(backfill_visible_anchor(&mut state), 1);
        assert_eq!(state.flights[0].last_visible_at, Some(last_seen));
        // Idempotent: a second pass touches nothing.
        assert_eq!(backfill_visible_anchor(&mut state), 0);
    }

    #[test]
    fn backfill_visible_anchor_skips_pending_flight_without_last_seen() {
        let mut flight = tracked_flight();
        flight.last_seen = None;
        flight.last_visible_at = None;
        let mut state = FlightTrackerState {
            flights: vec![flight],
        };

        assert_eq!(backfill_visible_anchor(&mut state), 0);
        assert_eq!(state.flights[0].last_visible_at, None);
    }
}
