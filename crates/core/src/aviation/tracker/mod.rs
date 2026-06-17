pub(crate) mod commands;
pub(crate) mod debug_journal;
pub(crate) mod format;
pub(crate) mod loop_run;
pub(crate) mod metadata;
pub(crate) mod phase;
pub(crate) mod schedule;
pub(crate) mod state;
#[cfg(test)]
pub(crate) mod test_support;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::Duration;
use twitch_irc::message::PrivmsgMessage;

use crate::aviation::AviationstackFlightMetadata;

pub use loop_run::run_flight_tracker;

/// Maximum number of simultaneously tracked flights.
pub const MAX_TRACKED_FLIGHTS: usize = 12;

/// Maximum number of flights a single user can track.
pub const MAX_FLIGHTS_PER_USER: usize = 3;

/// No data threshold before declaring tracking lost.
pub const TRACKING_LOST_THRESHOLD: Duration = Duration::from_secs(300);
/// Time after tracking lost before auto-removing.
pub const TRACKING_LOST_REMOVAL: Duration = Duration::from_secs(1800);

pub const POLL_FAST: Duration = Duration::from_secs(30);
pub const POLL_NORMAL: Duration = Duration::from_secs(60);
pub const POLL_SLOW: Duration = Duration::from_secs(120);
/// Timeout for a single live ADS-B lookup.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for adsbdb route fetch.
pub(crate) const ROUTE_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) const DIVERT_BEARING_THRESHOLD: f64 = 90.0;
pub(crate) const DIVERT_CONSECUTIVE_POLLS: u32 = 3;

/// Identifies a flight either by callsign or ICAO24 hex code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FlightIdentifier {
    Callsign(String),
    Hex(String),
}

impl FlightIdentifier {
    /// Parse user input into a FlightIdentifier.
    ///
    /// IATA flight numbers (`LL####`, e.g. `DE1513`, `AF1234`) take precedence
    /// over the six-character ICAO24 hex rule when both apply — the user means
    /// a flight, and `!track` resolves it to the operating ICAO callsign. Such
    /// inputs are stored as callsigns; ADS-B may fall back to hex lookup when a
    /// callsign poll misses and the string is also valid hex.
    ///
    /// Remaining 6-character all-hex-digit strings are ICAO24 hex codes.
    /// Everything else is treated as a callsign and must be ASCII alphanumeric
    /// with length 1..=8 (ICAO callsigns are at most 8 chars). Rejects path
    /// separators and other characters that would let user input forge URL
    /// segments when interpolated into ADS-B aggregator endpoints.
    pub fn parse(input: &str) -> eyre::Result<Self> {
        let input = input.trim().to_uppercase();
        if input.is_empty() {
            eyre::bail!("Identifier darf nicht leer sein");
        }
        if crate::aviation::is_iata_flight_number(&input) {
            return Ok(FlightIdentifier::Callsign(input));
        }
        if Self::is_valid_icao24_hex(&input) {
            return Ok(FlightIdentifier::Hex(input));
        }
        if input.len() > 8 || !input.chars().all(|c| c.is_ascii_alphanumeric()) {
            eyre::bail!("Ungültiges callsign/hex");
        }
        Ok(FlightIdentifier::Callsign(input))
    }

    pub(crate) fn is_valid_icao24_hex(value: &str) -> bool {
        value.len() == 6 && value.chars().all(|c| c.is_ascii_hexdigit())
    }

    /// Returns the display string (the callsign or hex value).
    pub fn as_str(&self) -> &str {
        match self {
            FlightIdentifier::Callsign(s) | FlightIdentifier::Hex(s) => s,
        }
    }

    /// Check if this identifier matches a given callsign or hex.
    pub fn matches(&self, callsign: Option<&str>, hex: Option<&str>) -> bool {
        match self {
            FlightIdentifier::Callsign(s) => callsign.is_some_and(|cs| cs.eq_ignore_ascii_case(s)),
            FlightIdentifier::Hex(s) => hex.is_some_and(|h| h.eq_ignore_ascii_case(s)),
        }
    }
}

impl std::fmt::Display for FlightIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Detected flight phase.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum FlightPhase {
    Unknown,
    Ground,
    Takeoff,
    Climb,
    Cruise,
    Descent,
    Approach,
    Landing,
}

/// Where the currently stored ICAO24 hex came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HexSource {
    UserInput,
    AviationStack,
    Adsb,
}

/// How confidently the observed ADS-B aircraft matches the requested target flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TargetConfirmation {
    #[default]
    Pending,
    AircraftVisible,
    ConfirmedByCallsign,
    InferredByAssignedHex,
}

impl TargetConfirmation {
    pub(crate) fn is_target_confirmed(self) -> bool {
        matches!(
            self,
            TargetConfirmation::ConfirmedByCallsign | TargetConfirmation::InferredByAssignedHex
        )
    }
}

impl std::fmt::Display for FlightPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlightPhase::Unknown => write!(f, "Unknown"),
            FlightPhase::Ground => write!(f, "Ground"),
            FlightPhase::Takeoff => write!(f, "Takeoff"),
            FlightPhase::Climb => write!(f, "Climb"),
            FlightPhase::Cruise => write!(f, "Cruise"),
            FlightPhase::Descent => write!(f, "Descent"),
            FlightPhase::Approach => write!(f, "Approach"),
            FlightPhase::Landing => write!(f, "Landing"),
        }
    }
}

/// State of a single tracked flight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedFlight {
    pub identifier: FlightIdentifier,
    pub callsign: Option<String>,
    #[serde(default)]
    pub alias_callsigns: Vec<String>,
    pub hex: Option<String>,
    #[serde(default)]
    pub hex_source: Option<HexSource>,
    #[serde(default)]
    pub observed_callsign: Option<String>,
    #[serde(default)]
    pub target_confirmation: TargetConfirmation,
    pub phase: FlightPhase,
    pub route: Option<(String, String)>,
    pub aircraft_type: Option<String>,

    pub altitude_ft: Option<i64>,
    pub vertical_rate_fpm: Option<i64>,
    pub ground_speed_kts: Option<f64>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub squawk: Option<String>,

    pub tracked_by: String,
    pub tracked_at: DateTime<Utc>,
    pub last_seen: Option<DateTime<Utc>>,
    /// Last poll in which the assigned aircraft was visible on ADS-B; drives the
    /// tracking-lost removal timer. Distinct from `last_seen`, which is the last
    /// *target-confirmed* sighting and drives confirmation decay.
    #[serde(default)]
    pub last_visible_at: Option<DateTime<Utc>>,
    pub last_phase_change: Option<DateTime<Utc>>,
    pub polls_since_change: u32,
    #[serde(default)]
    pub takeoff_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub aviationstack_checked: bool,
    #[serde(default)]
    pub scheduled_departure_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_adsb_poll_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub divert_consecutive_polls: u32,

    #[serde(default)]
    pub dest_lat: Option<f64>,
    #[serde(default)]
    pub dest_lon: Option<f64>,
}

/// Persisted AviationStack lookup result shared by `!track` and `!info`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFlightInfo {
    pub aliases: Vec<String>,
    pub flight_date: Option<String>,
    pub metadata: Option<AviationstackFlightMetadata>,
    pub cached_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl CachedFlightInfo {
    pub(crate) fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }

    pub(crate) fn matches_any_alias(&self, aliases: &[String]) -> bool {
        aliases.iter().any(|alias| {
            self.aliases
                .iter()
                .any(|cached| cached.eq_ignore_ascii_case(alias))
        })
    }
}

/// Persisted state of all tracked flights.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FlightTrackerState {
    pub flights: Vec<TrackedFlight>,
    #[serde(default)]
    pub flight_info_cache: Vec<CachedFlightInfo>,
}

/// Read projection of [`TrackedFlight`] for the web dashboard.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TrackedFlightView {
    pub identifier: String,
    pub callsign: Option<String>,
    pub owner_login: String,
    pub phase: String,
    pub altitude_ft: Option<i64>,
    pub ground_speed_kts: Option<f64>,
    pub last_seen_secs_ago: Option<u64>,
}

/// Converts the tracked flights in `state` into a snapshot of [`TrackedFlightView`]s.
pub fn build_flight_view(state: &FlightTrackerState, now: DateTime<Utc>) -> Vec<TrackedFlightView> {
    state
        .flights
        .iter()
        .map(|f| TrackedFlightView {
            identifier: f
                .callsign
                .clone()
                .or_else(|| f.hex.clone())
                .unwrap_or_else(|| format!("{}", f.identifier)),
            callsign: f.callsign.clone(),
            owner_login: f.tracked_by.clone(),
            phase: if f.target_confirmation == TargetConfirmation::AircraftVisible {
                "AircraftVisible".to_string()
            } else {
                format!("{}", f.phase)
            },
            altitude_ft: f.altitude_ft,
            ground_speed_kts: f.ground_speed_kts,
            last_seen_secs_ago: f
                .last_seen
                .map(|seen| (now - seen).num_seconds().max(0) as u64),
        })
        .collect()
}

/// Commands sent from chat command handlers to the flight tracker task.
pub enum TrackerCommand {
    Track {
        identifier: FlightIdentifier,
        requested_by: String,
        reply_to: PrivmsgMessage,
    },
    Untrack {
        identifier: String,
        requested_by: String,
        is_mod: bool,
        reply_to: PrivmsgMessage,
    },
    Status {
        identifier: Option<String>,
        reply_to: PrivmsgMessage,
    },
    Info {
        identifier: FlightIdentifier,
        reply_to: PrivmsgMessage,
    },
    Snapshot {
        reply: tokio::sync::oneshot::Sender<Vec<TrackedFlightView>>,
    },
    DeleteFromWeb {
        identifier: String,
        reply: tokio::sync::oneshot::Sender<Option<String>>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aviation::tracker::{
        commands::aircraft_matches_tracked_callsign,
        debug_journal::{
            DebugHttpOutcome, DiversionDebugInput, FlightTrackerDebugEvent, append_debug_event,
            debug_journal_path,
        },
        format::msg_landing,
        metadata::apply_aviationstack_metadata,
        phase::{CRUISE_STABLE_POLLS, detect_phase},
        schedule::{PendingPollSchedule, next_poll_at, pending_poll_schedule},
        state::clear_pending_callsign_hexes,
        test_support::dt,
    };
    use crate::aviation::{AltBaro, AviationstackFlightMetadata, NearbyAircraft};

    fn tracked_flight() -> TrackedFlight {
        TrackedFlight {
            identifier: FlightIdentifier::Callsign("DLH1234".to_string()),
            callsign: Some("DLH1234".to_string()),
            alias_callsigns: vec!["DLH1234".to_string()],
            hex: None,
            hex_source: None,
            observed_callsign: None,
            target_confirmation: TargetConfirmation::Pending,
            phase: FlightPhase::Landing,
            route: None,
            aircraft_type: None,
            altitude_ft: None,
            vertical_rate_fpm: None,
            ground_speed_kts: None,
            lat: None,
            lon: None,
            squawk: None,
            tracked_by: "alice".to_string(),
            tracked_at: dt("2026-04-18T10:00:00Z"),
            last_seen: None,
            last_visible_at: None,
            last_phase_change: None,
            polls_since_change: 0,
            takeoff_at: None,
            aviationstack_checked: false,
            scheduled_departure_at: None,
            last_adsb_poll_at: None,
            divert_consecutive_polls: 0,
            dest_lat: None,
            dest_lon: None,
        }
    }

    fn tracked_flight_with(phase: FlightPhase, polls_since_change: u32) -> TrackedFlight {
        TrackedFlight {
            phase,
            polls_since_change,
            ..tracked_flight()
        }
    }

    #[test]
    fn clear_pending_callsign_hexes_drops_untrusted_hex_only_before_adsb_seen() {
        let mut pending_callsign = tracked_flight();
        pending_callsign.hex = Some("48C2A2".to_string());

        let mut aviationstack_pending = tracked_flight();
        aviationstack_pending.hex = Some("4CA87D".to_string());
        aviationstack_pending.hex_source = Some(HexSource::AviationStack);

        let mut live_callsign = tracked_flight();
        live_callsign.hex = Some("3C6589".to_string());
        live_callsign.last_seen = Some(dt("2026-04-18T11:00:00Z"));

        let mut pending_hex_identifier = tracked_flight();
        pending_hex_identifier.identifier = FlightIdentifier::Hex("48C2A2".to_string());
        pending_hex_identifier.hex = Some("48C2A2".to_string());

        let mut state = FlightTrackerState {
            flights: vec![
                pending_callsign,
                aviationstack_pending,
                live_callsign,
                pending_hex_identifier,
            ],
            flight_info_cache: Vec::new(),
        };

        assert_eq!(clear_pending_callsign_hexes(&mut state), 1);
        assert_eq!(state.flights[0].hex, None);
        assert_eq!(state.flights[0].hex_source, None);
        assert_eq!(state.flights[1].hex.as_deref(), Some("4CA87D"));
        assert_eq!(state.flights[2].hex.as_deref(), Some("3C6589"));
        assert_eq!(state.flights[3].hex.as_deref(), Some("48C2A2"));
    }

    #[test]
    fn aircraft_matches_tracked_callsign_rejects_reused_airframe() {
        let mut flight = tracked_flight();
        flight.identifier = FlightIdentifier::Callsign("FR196".to_string());
        flight.callsign = Some("RYR196".to_string());
        let matching = NearbyAircraft {
            flight: Some(" RYR196 ".to_string()),
            ..aircraft_at(34_000, 0)
        };
        let reused_airframe = NearbyAircraft {
            flight: Some("RYR58JX ".to_string()),
            ..aircraft_at(34_000, 0)
        };
        let missing_callsign = NearbyAircraft {
            flight: None,
            ..aircraft_at(34_000, 0)
        };

        assert!(aircraft_matches_tracked_callsign(&matching, &flight));
        assert!(!aircraft_matches_tracked_callsign(
            &reused_airframe,
            &flight
        ));
        assert!(!aircraft_matches_tracked_callsign(
            &missing_callsign,
            &flight
        ));
    }

    fn aircraft_at(altitude_ft: i64, baro_rate: i64) -> NearbyAircraft {
        NearbyAircraft {
            hex: Some("4952c3".to_string()),
            flight: Some("TAP247".to_string()),
            r: None,
            t: Some("A339".to_string()),
            alt_baro: Some(AltBaro::Feet(altitude_ft)),
            lat: Some(38.0),
            lon: Some(-30.0),
            gs: Some(450.0),
            baro_rate: Some(baro_rate),
            geom_rate: None,
            squawk: Some("1000".to_string()),
            nav_modes: None,
            rssi: None,
            seen_pos: None,
        }
    }

    fn metadata() -> AviationstackFlightMetadata {
        AviationstackFlightMetadata::default()
    }

    fn pending_interval(flight: &TrackedFlight, now: DateTime<Utc>) -> tokio::time::Duration {
        match pending_poll_schedule(flight, now) {
            PendingPollSchedule::Active { interval, .. } => interval,
            PendingPollSchedule::Expired => panic!("expected active pending schedule"),
        }
    }

    fn assert_pending_expired(flight: &TrackedFlight, now: DateTime<Utc>) {
        assert_eq!(
            pending_poll_schedule(flight, now),
            PendingPollSchedule::Expired
        );
    }

    // These constants mirror private consts in schedule.rs; keep in sync.
    const PENDING_POLL_60_SEC: tokio::time::Duration = tokio::time::Duration::from_secs(60);
    const PENDING_POLL_2_MIN: tokio::time::Duration = tokio::time::Duration::from_secs(120);
    const PENDING_POLL_5_MIN: tokio::time::Duration = tokio::time::Duration::from_secs(300);
    const PENDING_POLL_10_MIN: tokio::time::Duration = tokio::time::Duration::from_secs(600);
    const PENDING_POLL_15_MIN: tokio::time::Duration = tokio::time::Duration::from_secs(900);
    const PENDING_POLL_30_MIN: tokio::time::Duration = tokio::time::Duration::from_secs(1800);

    #[test]
    fn landing_uses_takeoff_time_not_tracking_time() {
        let mut flight = tracked_flight();
        flight.takeoff_at = Some(dt("2026-04-18T10:30:00Z"));

        let msg = msg_landing(&flight, dt("2026-04-18T12:00:00Z"));

        assert!(msg.contains("Flugzeit: 1h30m"), "got: {msg}");
        assert!(!msg.contains("2h00m"), "got: {msg}");
    }

    #[test]
    fn landing_reports_unknown_duration_without_takeoff_time() {
        let flight = tracked_flight();

        let msg = msg_landing(&flight, dt("2026-04-18T12:00:00Z"));

        assert!(
            msg.contains("Flugzeit: unbekannt (Takeoff nicht beobachtet)"),
            "got: {msg}"
        );
    }

    #[test]
    fn aviationstack_metadata_sets_route_and_actual_runway_takeoff() {
        let mut flight = tracked_flight();
        let mut m = metadata();
        m.departure_iata = Some("fra".to_string());
        m.arrival_iata = Some("muc".to_string());
        m.departure_scheduled = Some(dt("2026-04-18T09:45:00Z"));
        m.departure_actual = Some(dt("2026-04-18T10:00:00Z"));
        m.departure_actual_runway = Some(dt("2026-04-18T10:05:00Z"));
        m.aircraft_icao24 = Some("3c6589".to_string());
        m.aircraft_icao = Some("a320".to_string());

        apply_aviationstack_metadata(&mut flight, m);

        assert_eq!(flight.route, Some(("FRA".to_string(), "MUC".to_string())));
        assert_eq!(
            flight.scheduled_departure_at,
            Some(dt("2026-04-18T09:45:00Z"))
        );
        assert_eq!(flight.takeoff_at, Some(dt("2026-04-18T10:05:00Z")));
        assert_eq!(flight.hex.as_deref(), Some("3C6589"));
        assert_eq!(flight.hex_source, Some(HexSource::AviationStack));
        assert_eq!(flight.aircraft_type.as_deref(), Some("A320"));
    }

    #[test]
    fn aviationstack_metadata_falls_back_to_actual_departure() {
        let mut flight = tracked_flight();
        let mut m = metadata();
        m.departure_actual = Some(dt("2026-04-18T10:00:00Z"));

        apply_aviationstack_metadata(&mut flight, m);

        assert_eq!(flight.takeoff_at, Some(dt("2026-04-18T10:00:00Z")));
    }

    #[test]
    fn aviationstack_metadata_replaces_existing_resolved_callsign() {
        let mut flight = tracked_flight();
        flight.callsign = Some("EZY123".to_string());
        flight.alias_callsigns = vec!["U2123".to_string(), "EZY123".to_string()];
        let mut m = metadata();
        m.flight_iata = Some("U2123".to_string());
        m.flight_icao = Some("EJU123".to_string());

        apply_aviationstack_metadata(&mut flight, m);

        assert_eq!(flight.callsign.as_deref(), Some("EJU123"));
        assert!(flight.alias_callsigns.iter().any(|alias| alias == "EZY123"));
        assert!(flight.alias_callsigns.iter().any(|alias| alias == "EJU123"));
    }

    #[test]
    fn aviationstack_metadata_preserves_static_callsign_as_alias() {
        let mut flight = tracked_flight();
        flight.identifier = FlightIdentifier::Callsign("AF1234".to_string());
        flight.callsign = Some("AFR1234".to_string());
        flight.alias_callsigns = vec!["AF1234".to_string(), "AFR1234".to_string()];
        let mut m = metadata();
        m.flight_iata = Some("AF1234".to_string());
        m.flight_icao = Some("KLM1234".to_string());

        apply_aviationstack_metadata(&mut flight, m);

        assert_eq!(flight.callsign.as_deref(), Some("KLM1234"));
        assert_eq!(
            flight.alias_callsigns,
            vec![
                "AF1234".to_string(),
                "AFR1234".to_string(),
                "KLM1234".to_string()
            ]
        );
    }

    #[test]
    fn aviationstack_metadata_can_upgrade_iata_callsign_to_icao() {
        let mut flight = tracked_flight();
        flight.callsign = Some("LH1929".to_string());
        let mut m = metadata();
        m.flight_iata = Some("LH1929".to_string());
        m.flight_icao = Some("DLH1929".to_string());

        apply_aviationstack_metadata(&mut flight, m);

        assert_eq!(flight.callsign.as_deref(), Some("DLH1929"));
    }

    #[test]
    fn pending_poll_schedule_uses_sparse_interval_far_before_departure() {
        let mut flight = tracked_flight();
        flight.scheduled_departure_at = Some(dt("2026-04-18T12:00:00Z"));

        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T05:59:00Z")),
            PENDING_POLL_15_MIN
        );
    }

    #[test]
    fn pending_poll_schedule_uses_issue_259_pre_departure_ramp() {
        let mut flight = tracked_flight();
        flight.scheduled_departure_at = Some(dt("2026-04-18T12:00:00Z"));

        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T09:01:00Z")),
            PENDING_POLL_15_MIN
        );
        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T11:01:00Z")),
            PENDING_POLL_5_MIN
        );
        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T11:31:00Z")),
            PENDING_POLL_60_SEC
        );
    }

    #[test]
    fn pending_poll_schedule_slows_after_departure_window() {
        let mut flight = tracked_flight();
        flight.scheduled_departure_at = Some(dt("2026-04-18T12:00:00Z"));

        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T13:00:00Z")),
            PENDING_POLL_5_MIN
        );
        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T15:01:00Z")),
            PENDING_POLL_15_MIN
        );
    }

    #[test]
    fn pending_poll_schedule_expires_scheduled_flights() {
        let mut flight = tracked_flight();
        flight.scheduled_departure_at = Some(dt("2026-04-18T12:00:00Z"));

        assert_pending_expired(&flight, dt("2026-04-19T00:00:00Z"));
    }

    #[test]
    fn next_poll_at_strict_chills_until_three_hours_before_departure() {
        let mut flight = tracked_flight();
        let now = dt("2026-04-18T10:00:00Z");
        flight.scheduled_departure_at = Some(dt("2026-04-18T15:00:00Z"));
        flight.last_adsb_poll_at = None;

        assert_eq!(
            next_poll_at(&[flight], now),
            Some(dt("2026-04-18T12:00:00Z"))
        );
    }

    #[test]
    fn next_poll_at_wakes_at_scheduled_cadence_boundaries() {
        let mut flight = tracked_flight();
        let now = dt("2026-04-18T10:59:00Z");
        flight.scheduled_departure_at = Some(dt("2026-04-18T12:00:00Z"));
        flight.last_adsb_poll_at = Some(dt("2026-04-18T10:50:00Z"));

        assert_eq!(
            next_poll_at(&[flight], now),
            Some(dt("2026-04-18T11:00:00Z"))
        );
    }

    #[test]
    fn pending_poll_schedule_ramps_unknown_departure_and_expires() {
        let flight = tracked_flight();

        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T10:05:00Z")),
            PENDING_POLL_2_MIN
        );
        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T10:10:00Z")),
            PENDING_POLL_10_MIN
        );
        assert_eq!(
            pending_interval(&flight, dt("2026-04-18T16:00:00Z")),
            PENDING_POLL_30_MIN
        );
        assert_pending_expired(&flight, dt("2026-04-19T10:00:00Z"));
    }

    #[test]
    fn next_poll_at_respects_pending_due_time() {
        let mut flight = tracked_flight();
        let now = dt("2026-04-18T10:00:00Z");
        flight.scheduled_departure_at = Some(dt("2026-04-18T10:30:00Z"));
        flight.last_adsb_poll_at = Some(now);

        assert_eq!(
            next_poll_at(&[flight], now),
            Some(dt("2026-04-18T10:01:00Z"))
        );
    }

    #[test]
    fn next_poll_at_keeps_live_fast_interval_for_recent_changes() {
        let mut flight = tracked_flight_with(FlightPhase::Unknown, 0);
        let now = dt("2026-04-18T10:00:00Z");
        flight.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        flight.last_seen = Some(now);
        flight.last_adsb_poll_at = Some(now);

        assert_eq!(
            next_poll_at(&[flight], now),
            Some(dt("2026-04-18T10:00:30Z"))
        );
    }

    #[test]
    fn divert_alert_repeats_after_consecutive_poll_threshold() {
        use crate::aviation::tracker::phase::update_divert_counter;
        let mut divert_consecutive_polls = 0;

        assert!(!update_divert_counter(&mut divert_consecutive_polls, true));
        assert_eq!(divert_consecutive_polls, 1);
        assert!(!update_divert_counter(&mut divert_consecutive_polls, true));
        assert_eq!(divert_consecutive_polls, 2);
        assert!(update_divert_counter(&mut divert_consecutive_polls, true));
        assert_eq!(divert_consecutive_polls, 3);
        assert!(update_divert_counter(&mut divert_consecutive_polls, true));
        assert_eq!(divert_consecutive_polls, 4);
        assert!(update_divert_counter(&mut divert_consecutive_polls, true));
        assert_eq!(divert_consecutive_polls, 5);

        assert!(!update_divert_counter(&mut divert_consecutive_polls, false));
        assert_eq!(divert_consecutive_polls, 0);

        assert!(!update_divert_counter(&mut divert_consecutive_polls, true));
        assert_eq!(divert_consecutive_polls, 1);
        assert!(!update_divert_counter(&mut divert_consecutive_polls, true));
        assert_eq!(divert_consecutive_polls, 2);
        assert!(update_divert_counter(&mut divert_consecutive_polls, true));
        assert_eq!(divert_consecutive_polls, 3);
    }

    #[test]
    fn detect_phase_keeps_descent_during_high_altitude_level_off() {
        let flight = tracked_flight_with(FlightPhase::Descent, CRUISE_STABLE_POLLS);
        let ac = aircraft_at(34_000, 0);

        assert_eq!(detect_phase(&flight, &ac), FlightPhase::Descent);
    }

    #[test]
    fn detect_phase_keeps_approach_during_high_altitude_level_off() {
        let flight = tracked_flight_with(FlightPhase::Approach, CRUISE_STABLE_POLLS);
        let ac = aircraft_at(12_000, 0);

        assert_eq!(detect_phase(&flight, &ac), FlightPhase::Approach);
    }

    #[test]
    fn detect_phase_still_detects_cruise_from_non_descent_phase() {
        let flight = tracked_flight_with(FlightPhase::Climb, CRUISE_STABLE_POLLS);
        let ac = aircraft_at(34_000, 0);

        assert_eq!(detect_phase(&flight, &ac), FlightPhase::Cruise);
    }

    #[test]
    fn parse_accepts_six_char_hex() {
        let id = FlightIdentifier::parse("4ca87d").unwrap();
        assert_eq!(id, FlightIdentifier::Hex("4CA87D".to_string()));
    }

    #[test]
    fn parse_treats_six_char_iata_flight_numbers_as_callsigns() {
        // DE1513 (Condor) is all-hex-digit but must resolve as a flight, not a
        // raw ICAO24 address — the regression that motivated the IATA-precedence rule.
        let id = FlightIdentifier::parse("DE1513").unwrap();
        assert_eq!(id, FlightIdentifier::Callsign("DE1513".to_string()));

        let id = FlightIdentifier::parse("AF1234").unwrap();
        assert_eq!(id, FlightIdentifier::Callsign("AF1234".to_string()));

        let id = FlightIdentifier::parse("BA1234").unwrap();
        assert_eq!(id, FlightIdentifier::Callsign("BA1234".to_string()));
    }

    #[test]
    fn parse_prefers_iata_over_valid_icao24_hex() {
        let id = FlightIdentifier::parse("AB1234").unwrap();
        assert_eq!(id, FlightIdentifier::Callsign("AB1234".to_string()));
        assert!(FlightIdentifier::is_valid_icao24_hex("AB1234"));
    }

    #[test]
    fn aircraft_matches_tracked_callsign_accepts_alias_callsigns() {
        let mut flight = tracked_flight();
        flight.identifier = FlightIdentifier::Callsign("AF1234".to_string());
        flight.callsign = Some("KLM1234".to_string());
        flight.alias_callsigns = vec![
            "AF1234".to_string(),
            "AFR1234".to_string(),
            "KLM1234".to_string(),
        ];
        let operating_carrier = NearbyAircraft {
            flight: Some(" AFR1234 ".to_string()),
            ..aircraft_at(34_000, 0)
        };

        assert!(aircraft_matches_tracked_callsign(
            &operating_carrier,
            &flight
        ));
    }

    #[test]
    fn find_flight_index_matches_alias_callsigns() {
        let mut flight = tracked_flight();
        flight.identifier = FlightIdentifier::Callsign("AF1234".to_string());
        flight.callsign = Some("KLM1234".to_string());
        flight.alias_callsigns = vec!["AF1234".to_string(), "AFR1234".to_string()];
        let state = FlightTrackerState {
            flights: vec![flight],
            flight_info_cache: Vec::new(),
        };

        assert_eq!(
            crate::aviation::tracker::commands::find_flight_index(&state.flights, "afr1234"),
            Some(0)
        );
    }

    #[test]
    fn parse_accepts_alphanumeric_callsign() {
        let id = FlightIdentifier::parse("DLH1234").unwrap();
        assert_eq!(id, FlightIdentifier::Callsign("DLH1234".to_string()));
    }

    #[test]
    fn parse_accepts_eight_char_callsign() {
        let id = FlightIdentifier::parse("RYR1234A").unwrap();
        assert_eq!(id, FlightIdentifier::Callsign("RYR1234A".to_string()));
    }

    #[test]
    fn parse_rejects_path_traversal() {
        assert!(FlightIdentifier::parse("../foo").is_err());
        assert!(FlightIdentifier::parse("a/b").is_err());
    }

    #[test]
    fn parse_rejects_too_long_callsign() {
        assert!(FlightIdentifier::parse("ABCDEFGHI").is_err());
    }

    #[test]
    fn parse_rejects_non_ascii() {
        assert!(FlightIdentifier::parse("DLH1ä34").is_err());
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(FlightIdentifier::parse("").is_err());
        assert!(FlightIdentifier::parse("   ").is_err());
    }

    #[test]
    fn build_flight_view_returns_one_entry_per_flight() {
        let flight_a = TrackedFlight {
            identifier: FlightIdentifier::Callsign("EZY100".to_string()),
            callsign: Some("EZY100".to_string()),
            phase: FlightPhase::Cruise,
            altitude_ft: Some(35_000),
            ground_speed_kts: Some(480.0),
            tracked_by: "alice".to_string(),
            last_seen: None,
            ..tracked_flight()
        };
        let flight_b = TrackedFlight {
            identifier: FlightIdentifier::Hex("4CA87D".to_string()),
            callsign: None,
            hex: Some("4CA87D".to_string()),
            phase: FlightPhase::Ground,
            altitude_ft: Some(0),
            ground_speed_kts: Some(0.0),
            tracked_by: "bob".to_string(),
            last_seen: None,
            ..tracked_flight()
        };

        let state = FlightTrackerState {
            flights: vec![flight_a, flight_b],
            flight_info_cache: Vec::new(),
        };
        let now = dt("2026-05-12T10:00:00Z");

        let views = build_flight_view(&state, now);

        assert_eq!(views.len(), 2);

        assert_eq!(views[0].identifier, "EZY100");
        assert_eq!(views[0].owner_login, "alice");
        assert_eq!(views[0].phase, "Cruise");
        assert_eq!(views[0].altitude_ft, Some(35_000));
        assert!(views[0].last_seen_secs_ago.is_none());

        assert_eq!(views[1].identifier, "4CA87D");
        assert_eq!(views[1].owner_login, "bob");
        assert_eq!(views[1].phase, "Ground");
        assert!(views[1].callsign.is_none());
    }

    #[test]
    fn build_flight_view_computes_last_seen_secs_ago() {
        let seen_at = dt("2026-05-12T09:59:00Z");
        let now = dt("2026-05-12T10:00:00Z");

        let flight = TrackedFlight {
            last_seen: Some(seen_at),
            ..tracked_flight()
        };

        let state = FlightTrackerState {
            flights: vec![flight],
            flight_info_cache: Vec::new(),
        };

        let views = build_flight_view(&state, now);

        assert_eq!(views[0].last_seen_secs_ago, Some(60));
    }

    #[test]
    fn build_flight_view_falls_back_identifier_to_flight_identifier_display() {
        let flight = TrackedFlight {
            identifier: FlightIdentifier::Callsign("RYR42".to_string()),
            callsign: None,
            hex: None,
            ..tracked_flight()
        };

        let state = FlightTrackerState {
            flights: vec![flight],
            flight_info_cache: Vec::new(),
        };
        let now = dt("2026-05-12T10:00:00Z");

        let views = build_flight_view(&state, now);

        assert_eq!(views[0].identifier, "RYR42");
    }

    #[tokio::test]
    async fn debug_journal_appends_to_date_named_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let now = dt("2026-04-18T10:00:00Z");

        append_debug_event(
            dir.path(),
            now,
            &FlightTrackerDebugEvent::track_started(&tracked_flight()),
        )
        .await;

        let path = debug_journal_path(dir.path(), now);
        assert_eq!(
            path,
            dir.path()
                .join("flight-tracker-debug")
                .join("2026-04-18.jsonl")
        );
        let contents = tokio::fs::read_to_string(path).await.unwrap();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let event: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(event["event"], "track_started");
        assert_eq!(event["ts"], "2026-04-18T10:00:00Z");
        assert_eq!(event["identifier"], "DLH1234");
    }

    #[test]
    fn debug_journal_serialization_excludes_raw_bodies_and_secrets() {
        let event = FlightTrackerDebugEvent::aviationstack_metadata(
            &FlightIdentifier::Callsign("DLH1234".to_string()),
            Some("DLH1234"),
            DebugHttpOutcome::client_response("aviationstack", "flight_metadata", 403),
            None,
        );

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"status\":403"));
        assert!(json.contains("client_response"));
        assert!(!json.contains("access_key"));
        assert!(!json.contains("test-key"));
        assert!(!json.contains("raw_body"));

        let forbidden =
            serde_json::to_string(&DebugHttpOutcome::forbidden_parked("adsb.one", "hex", 403))
                .unwrap();
        assert!(forbidden.contains("forbidden_parked"));
        assert!(forbidden.contains("\"status\":403"));

        let rate_limited =
            serde_json::to_string(&DebugHttpOutcome::rate_limited("adsb.lol", "hex")).unwrap();
        assert!(rate_limited.contains("rate_limited"));
        assert!(rate_limited.contains("\"status\":429"));
    }

    #[test]
    fn debug_journal_diversion_decision_records_bearings_and_counter() {
        let event = FlightTrackerDebugEvent::diversion_decision(
            &TrackedFlight {
                phase: FlightPhase::Approach,
                target_confirmation: TargetConfirmation::ConfirmedByCallsign,
                route: Some(("FRA".to_string(), "MUC".to_string())),
                dest_lat: Some(48.3538),
                dest_lon: Some(11.7861),
                ..tracked_flight()
            },
            DiversionDebugInput {
                previous_lat: 49.0,
                previous_lon: 9.0,
                current_lat: 49.2,
                current_lon: 8.5,
                destination_lat: 48.3538,
                destination_lon: 11.7861,
                ground_track: 280.0,
                bearing_to_dest: 110.0,
                diff: 170.0,
                threshold: 90.0,
                anomalous: true,
                counter_before: 2,
                counter_after: 3,
                alert_emitted: true,
            },
        );

        let json = serde_json::to_value(event).unwrap();
        assert_eq!(json["event"], "diversion_decision");
        assert_eq!(json["phase"], "Approach");
        assert_eq!(json["target_confirmation"], "ConfirmedByCallsign");
        assert_eq!(json["ground_track"], 280.0);
        assert_eq!(json["bearing_to_dest"], 110.0);
        assert_eq!(json["counter_before"], 2);
        assert_eq!(json["counter_after"], 3);
        assert_eq!(json["alert_emitted"], true);
        assert_eq!(json["route"]["destination"], "MUC");
    }
}
