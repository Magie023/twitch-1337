//! Shared test fixtures for the flight tracker's unit tests.
//!
//! Consolidates the `dt` timestamp helper and a full-field [`TrackedFlight`]
//! builder so the `mod.rs` and `state.rs` test modules don't each carry a copy.

use chrono::{DateTime, Utc};

use super::{FlightIdentifier, FlightPhase, HexSource, TargetConfirmation, TrackedFlight};

/// Parse an RFC 3339 timestamp into a UTC `DateTime`, panicking on bad input.
pub(crate) fn dt(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

/// A fully populated `AircraftVisible` flight fixture (callsign tracked by hex,
/// observed callsign mismatched) for state-level tests.
pub(crate) fn tracked_flight() -> TrackedFlight {
    TrackedFlight {
        identifier: FlightIdentifier::Callsign("DLH1929".to_string()),
        callsign: Some("DLH1929".to_string()),
        hex: Some("3C6497".to_string()),
        hex_source: Some(HexSource::Adsb),
        observed_callsign: Some("DLH9999".to_string()),
        target_confirmation: TargetConfirmation::AircraftVisible,
        phase: FlightPhase::Cruise,
        route: None,
        aircraft_type: None,
        altitude_ft: Some(12_000),
        vertical_rate_fpm: Some(-1_800),
        ground_speed_kts: Some(280.0),
        lat: Some(52.4),
        lon: Some(13.5),
        squawk: Some("1000".to_string()),
        tracked_by: "alice".to_string(),
        tracked_at: dt("2026-04-18T12:00:00Z"),
        last_seen: Some(dt("2026-04-18T12:01:00Z")),
        last_visible_at: Some(dt("2026-04-18T12:01:00Z")),
        last_phase_change: None,
        polls_since_change: 0,
        takeoff_at: None,
        aviationstack_checked: true,
        scheduled_departure_at: Some(dt("2026-04-18T12:00:00Z")),
        last_adsb_poll_at: Some(dt("2026-04-18T12:32:00Z")),
        divert_consecutive_polls: 0,
        dest_lat: None,
        dest_lon: None,
    }
}
