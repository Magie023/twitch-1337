//! The Advance: the pure per-`Observation` transition of a `TrackedFlight`.
//!
//! `advance_flight` mutates a flight in place and returns a `FlightUpdate`
//! describing the side effects the poll loop should perform. It does no I/O
//! and is deterministic given `(flight, observation, now)`.

use chrono::{DateTime, TimeDelta, Utc};
use tracing::debug;

use super::debug_journal::{AdsbPollDebugInput, DiversionDebugInput, FlightTrackerDebugEvent};
use super::format::{
    msg_adsb_visible, msg_approach, msg_cruise, msg_descent, msg_landing, msg_pending_expired,
    msg_possible_divert, msg_squawk_emergency, msg_takeoff, msg_tracking_lost,
};
use super::metadata::{
    add_alias_callsign, aircraft_callsign, set_hex_if_consistent, set_route_from_iata,
};
use super::phase::{
    altitude_ft, detect_phase, emergency_squawk_meaning, is_airborne_phase, update_divert_counter,
    vertical_rate,
};
use super::schedule::is_pending_adsb;
use super::{
    DIVERT_BEARING_THRESHOLD, FlightIdentifier, FlightPhase, HexSource, TRACKING_LOST_REMOVAL,
    TRACKING_LOST_THRESHOLD, TargetConfirmation, TrackedFlight,
};
use crate::aviation::types::NearbyAircraft;

#[derive(Debug)]
pub(crate) enum PollOutcome {
    Hit(Box<NearbyAircraft>),
    Miss,
    Error,
    Timeout,
}

#[derive(Debug)]
pub(crate) struct Observation {
    pub used_hex: bool,
    pub aliases: Vec<String>,
    pub outcome: PollOutcome,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Emit {
    AdsbVisible,
    Takeoff,
    Cruise,
    Descent,
    Approach,
    Landing { takeoff_at: Option<DateTime<Utc>> },
    SquawkEmergency { code: String, meaning: String },
    PossibleDivert,
    TrackingLost,
    PendingExpired,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RemovalReason {
    TrackingLost { secs: i64 },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Followup {
    FetchRoute { callsign: String },
}

#[derive(Default)]
pub(crate) struct FlightUpdate {
    pub emits: Vec<Emit>,
    pub debug: Vec<FlightTrackerDebugEvent>,
    pub followups: Vec<Followup>,
    pub removal: Option<RemovalReason>,
}

/// Maps a semantic `Emit` to the existing chat formatter.
pub(crate) fn format_emit(flight: &TrackedFlight, emit: &Emit, now: DateTime<Utc>) -> String {
    match emit {
        Emit::AdsbVisible => msg_adsb_visible(flight),
        Emit::Takeoff => msg_takeoff(flight),
        Emit::Cruise => msg_cruise(flight),
        Emit::Descent => msg_descent(flight),
        Emit::Approach => msg_approach(flight),
        Emit::Landing { takeoff_at } => msg_landing(flight, *takeoff_at, now),
        Emit::SquawkEmergency { code, meaning } => msg_squawk_emergency(flight, code, meaning),
        Emit::PossibleDivert => msg_possible_divert(flight),
        Emit::TrackingLost => msg_tracking_lost(flight),
        Emit::PendingExpired => msg_pending_expired(flight),
    }
}

/// Applies a freshly fetched route to a flight (also sets dest lat/lon).
/// Same body as the inline `set_route_from_iata` call in the old loop.
pub(crate) fn apply_route(flight: &mut TrackedFlight, origin: &str, dest: &str) {
    set_route_from_iata(flight, origin, dest);
}

fn tracking_lost_threshold_delta() -> TimeDelta {
    TimeDelta::from_std(TRACKING_LOST_THRESHOLD).unwrap_or_else(|_| TimeDelta::zero())
}

pub(crate) fn flight_matches_callsign(flight: &TrackedFlight, actual: &str) -> bool {
    flight
        .callsign
        .as_deref()
        .is_some_and(|callsign| callsign.eq_ignore_ascii_case(actual))
        || flight
            .alias_callsigns
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(actual))
        || match &flight.identifier {
            FlightIdentifier::Callsign(identifier_callsign) => {
                identifier_callsign.eq_ignore_ascii_case(actual)
            }
            FlightIdentifier::Hex(_) => false,
        }
}

pub(crate) fn candidate_callsign_matches_flight(flight: &TrackedFlight, candidate: &str) -> bool {
    flight_matches_callsign(flight, candidate)
        || flight
            .observed_callsign
            .as_deref()
            .is_some_and(|observed| observed.eq_ignore_ascii_case(candidate))
}

pub(crate) fn identifier_callsign(identifier: &FlightIdentifier) -> Option<&str> {
    match identifier {
        FlightIdentifier::Callsign(callsign) => Some(callsign.as_str()),
        FlightIdentifier::Hex(_) => None,
    }
}

fn inferred_by_hex_window(flight: &TrackedFlight, now: DateTime<Utc>) -> bool {
    if matches!(&flight.identifier, FlightIdentifier::Hex(_)) {
        return true;
    }

    let Some(scheduled_departure_at) = flight.scheduled_departure_at else {
        return false;
    };
    now >= scheduled_departure_at - TimeDelta::minutes(30)
        && now <= scheduled_departure_at + TimeDelta::hours(3)
}

pub(crate) fn last_seen_age_secs(flight: &TrackedFlight, now: DateTime<Utc>) -> Option<i64> {
    flight
        .last_seen
        .map(|last_seen| now.signed_duration_since(last_seen).num_seconds().max(0))
}

fn should_keep_prior_confirmation(
    flight: &TrackedFlight,
    confirmation: TargetConfirmation,
    used_hex: bool,
    now: DateTime<Utc>,
) -> bool {
    used_hex
        && flight.target_confirmation.is_target_confirmed()
        && confirmation == TargetConfirmation::AircraftVisible
        && flight.last_seen.is_some_and(|last_seen| {
            now.signed_duration_since(last_seen) <= tracking_lost_threshold_delta()
        })
}

pub(crate) fn target_confirmation_for_aircraft(
    flight: &TrackedFlight,
    ac: &NearbyAircraft,
    used_hex: bool,
    now: DateTime<Utc>,
) -> Option<TargetConfirmation> {
    if aircraft_callsign(ac).is_some_and(|actual| flight_matches_callsign(flight, actual)) {
        return Some(TargetConfirmation::ConfirmedByCallsign);
    }

    if !used_hex {
        return None;
    }

    if matches!(&flight.identifier, FlightIdentifier::Hex(_)) || aircraft_callsign(ac).is_none() {
        if inferred_by_hex_window(flight, now) {
            Some(TargetConfirmation::InferredByAssignedHex)
        } else {
            Some(TargetConfirmation::AircraftVisible)
        }
    } else {
        Some(TargetConfirmation::AircraftVisible)
    }
}

/// Seed a flight's identity and telemetry from a confirming observation's
/// aircraft. Shared by [`advance_flight`] (the per-cycle transition) and the
/// `!track` command's initial poll, which previously duplicated this mutation.
///
/// Assigns the confirmation, observed callsign, resolved callsign/hex, aircraft
/// type, and position/velocity telemetry, and returns the callsign newly
/// resolved by this observation (if any) so the caller can decide whether to
/// look up its route. It deliberately does not touch `last_seen`,
/// `last_visible_at`, or `phase`, nor emit anything — those are the caller's
/// transition and announcement concerns.
pub(super) fn apply_observed_aircraft(
    flight: &mut TrackedFlight,
    ac: &NearbyAircraft,
    confirmation: TargetConfirmation,
    direct_target_confirmed: bool,
) -> Option<String> {
    flight.target_confirmation = confirmation;
    flight.observed_callsign = aircraft_callsign(ac).map(std::string::ToString::to_string);

    let newly_resolved = if (confirmation == TargetConfirmation::ConfirmedByCallsign
        || matches!(&flight.identifier, FlightIdentifier::Hex(_)))
        && flight.callsign.is_none()
        && let Some(cs) = flight.observed_callsign.clone()
    {
        debug!(identifier = %flight.identifier, callsign = %cs, "Resolved callsign");
        flight.callsign = Some(cs.clone());
        add_alias_callsign(flight, &cs);
        Some(cs)
    } else {
        None
    };

    if let Some(hex) = ac.hex.as_deref()
        && set_hex_if_consistent(flight, hex, HexSource::Adsb, direct_target_confirmed)
    {
        debug!(identifier = %flight.identifier, hex = %hex, "Resolved hex");
    }
    if flight.aircraft_type.is_none()
        && let Some(t) = &ac.t
    {
        flight.aircraft_type = Some(t.clone());
    }

    flight.altitude_ft = altitude_ft(ac);
    flight.vertical_rate_fpm = vertical_rate(ac);
    flight.ground_speed_kts = ac.gs;
    flight.lat = ac.lat;
    flight.lon = ac.lon;
    flight.squawk = ac.squawk.clone();

    newly_resolved
}

/// The pure per-`Observation` transition. Mutates `flight` in place and
/// returns the side effects (chat emits, debug-journal events, route
/// followups, removal) for the poll loop to perform. No I/O.
pub(crate) fn advance_flight(
    flight: &mut TrackedFlight,
    obs: &Observation,
    now: DateTime<Utc>,
) -> FlightUpdate {
    let mut update = FlightUpdate::default();
    let was_pending = is_pending_adsb(flight);
    let was_target_confirmed = flight.target_confirmation.is_target_confirmed();
    flight.last_adsb_poll_at = Some(now);

    let removal_threshold = TimeDelta::from_std(TRACKING_LOST_REMOVAL).unwrap_or(TimeDelta::zero());
    let lost_threshold = tracking_lost_threshold_delta();

    let ac = match &obs.outcome {
        PollOutcome::Hit(ac) => ac.as_ref(),
        PollOutcome::Miss => {
            update.debug.push(FlightTrackerDebugEvent::adsb_poll_result(
                flight,
                AdsbPollDebugInput {
                    lookup_mode: if obs.used_hex { "hex" } else { "callsign" },
                    aliases: &obs.aliases,
                    outcome: "miss",
                    aircraft: None,
                    direct_target_confirmed: None,
                    sticky_confirmation: None,
                    phase_sample_eligible: None,
                    last_seen_age_secs: last_seen_age_secs(flight, now),
                },
            ));
            // Removal keys off `last_visible_at` (last poll the assigned
            // aircraft was on ADS-B), NOT `last_seen` (last target-confirmed
            // sighting). A flight stuck in AircraftVisible freezes
            // `last_seen` while its hex keeps showing up, so using it here
            // would collapse the grace window on the first empty poll. A
            // flight whose hex stays visible indefinitely is intentionally
            // kept (trust-the-hex; the wrong-plane case is an inherent
            // tradeoff and never reaches this branch).
            if flight.last_visible_at.is_none() {
                flight.polls_since_change = flight.polls_since_change.saturating_add(1);
            } else if let Some(last_visible_at) = flight.last_visible_at {
                let lost_duration = now.signed_duration_since(last_visible_at);
                if lost_duration >= removal_threshold {
                    update.debug.push(FlightTrackerDebugEvent::tracking_removal(
                        flight,
                        "tracking_lost",
                        Some(lost_duration.num_seconds()),
                    ));
                    update.emits.push(Emit::TrackingLost);
                    update.removal = Some(RemovalReason::TrackingLost {
                        secs: lost_duration.num_seconds(),
                    });
                } else if lost_duration >= lost_threshold {
                    debug!(
                        identifier = %flight.identifier,
                        last_seen_secs_ago = lost_duration.num_seconds(),
                        "Flight not visible via ADS-B"
                    );
                }
            }
            return update;
        }
        PollOutcome::Error => {
            update.debug.push(FlightTrackerDebugEvent::adsb_poll_result(
                flight,
                AdsbPollDebugInput {
                    lookup_mode: if obs.used_hex { "hex" } else { "callsign" },
                    aliases: &obs.aliases,
                    outcome: "error",
                    aircraft: None,
                    direct_target_confirmed: None,
                    sticky_confirmation: None,
                    phase_sample_eligible: None,
                    last_seen_age_secs: last_seen_age_secs(flight, now),
                },
            ));
            return update;
        }
        PollOutcome::Timeout => {
            update.debug.push(FlightTrackerDebugEvent::adsb_poll_result(
                flight,
                AdsbPollDebugInput {
                    lookup_mode: if obs.used_hex { "hex" } else { "callsign" },
                    aliases: &obs.aliases,
                    outcome: "timeout",
                    aircraft: None,
                    direct_target_confirmed: None,
                    sticky_confirmation: None,
                    phase_sample_eligible: None,
                    last_seen_age_secs: last_seen_age_secs(flight, now),
                },
            ));
            return update;
        }
    };

    let Some(raw_confirmation) = target_confirmation_for_aircraft(flight, ac, obs.used_hex, now)
    else {
        debug!(
            identifier = %flight.identifier,
            aircraft_callsign = aircraft_callsign(ac).unwrap_or("<missing>"),
            "Ignoring ADS-B aircraft that does not confirm target"
        );
        return update;
    };

    // The assigned aircraft is visible on ADS-B (any confirmation kind:
    // ConfirmedByCallsign / InferredByAssignedHex / AircraftVisible). This
    // anchors the tracking-lost removal timer, separate from `last_seen`,
    // which only moves on direct target confirmation below.
    flight.last_visible_at = Some(now);

    let direct_target_confirmed = raw_confirmation.is_target_confirmed();
    let sticky_confirmation =
        should_keep_prior_confirmation(flight, raw_confirmation, obs.used_hex, now);
    let confirmation = if sticky_confirmation {
        flight.target_confirmation
    } else {
        raw_confirmation
    };
    let phase_sample_confirmed =
        direct_target_confirmed || (sticky_confirmation && aircraft_callsign(ac).is_none());
    let became_target_confirmed = !was_target_confirmed && direct_target_confirmed;
    update.debug.push(FlightTrackerDebugEvent::adsb_poll_result(
        flight,
        AdsbPollDebugInput {
            lookup_mode: if obs.used_hex { "hex" } else { "callsign" },
            aliases: &obs.aliases,
            outcome: "hit",
            aircraft: Some(ac),
            direct_target_confirmed: Some(direct_target_confirmed),
            sticky_confirmation: Some(sticky_confirmation),
            phase_sample_eligible: Some(phase_sample_confirmed),
            last_seen_age_secs: last_seen_age_secs(flight, now),
        },
    ));

    if direct_target_confirmed {
        flight.last_seen = Some(now);
    }

    // Capture pre-observation telemetry the emit/divert decisions below compare
    // against, before `apply_observed_aircraft` overwrites these fields.
    let prev_lat = flight.lat;
    let prev_lon = flight.lon;
    let prev_squawk = flight.squawk.clone();
    let old_target_confirmation = flight.target_confirmation;
    let old_hex = flight.hex.clone();
    let old_hex_source = flight.hex_source;

    if let Some(cs) = apply_observed_aircraft(flight, ac, confirmation, direct_target_confirmed)
        && flight.route.is_none()
    {
        update.followups.push(Followup::FetchRoute { callsign: cs });
    }

    if old_target_confirmation != flight.target_confirmation {
        update
            .debug
            .push(FlightTrackerDebugEvent::target_confirmation_transition(
                flight,
                &format!("{old_target_confirmation:?}"),
                &format!("{:?}", flight.target_confirmation),
            ));
    }
    if old_hex != flight.hex || old_hex_source != flight.hex_source {
        update.debug.push(FlightTrackerDebugEvent::hex_assignment(
            flight,
            old_hex.as_deref(),
            flight.hex.as_deref(),
            old_hex_source
                .map(|source| format!("{source:?}"))
                .as_deref(),
            flight
                .hex_source
                .map(|source| format!("{source:?}"))
                .as_deref(),
        ));
    }

    if let Some(new_squawk) = &ac.squawk {
        let squawk_changed = prev_squawk.as_ref() != Some(new_squawk);
        if phase_sample_confirmed
            && squawk_changed
            && let Some(meaning) = emergency_squawk_meaning(new_squawk)
        {
            update.emits.push(Emit::SquawkEmergency {
                code: new_squawk.clone(),
                meaning: meaning.to_string(),
            });
        }
    }

    if became_target_confirmed {
        update.emits.push(Emit::AdsbVisible);
        if was_pending {
            flight.phase = FlightPhase::Unknown;
            flight.last_phase_change = None;
            flight.polls_since_change = 0;
        }
    }

    if phase_sample_confirmed {
        let new_phase = detect_phase(flight, ac);
        let old_phase = flight.phase;

        if new_phase != old_phase {
            flight.phase = new_phase;
            flight.last_phase_change = Some(now);
            flight.polls_since_change = 0;

            if old_phase == FlightPhase::Ground
                && is_airborne_phase(new_phase)
                && flight.takeoff_at.is_none()
            {
                flight.takeoff_at = Some(now);
            }

            match new_phase {
                FlightPhase::Takeoff => update.emits.push(Emit::Takeoff),
                FlightPhase::Cruise => update.emits.push(Emit::Cruise),
                FlightPhase::Descent => update.emits.push(Emit::Descent),
                FlightPhase::Approach => update.emits.push(Emit::Approach),
                // Capture takeoff_at BEFORE the landing branch below clears it,
                // so the deferred `format_emit` renders the same flight time the
                // original inline `msg_landing(flight, now)` did.
                FlightPhase::Landing => update.emits.push(Emit::Landing {
                    takeoff_at: flight.takeoff_at,
                }),
                _ => {}
            }

            if new_phase == FlightPhase::Landing {
                flight.phase = FlightPhase::Ground;
                flight.takeoff_at = None;
            }
            update.debug.push(FlightTrackerDebugEvent::phase_transition(
                flight,
                &format!("{old_phase:?}"),
                &format!("{new_phase:?}"),
                new_phase == FlightPhase::Landing,
            ));
        } else {
            flight.polls_since_change += 1;
        }
    }

    if phase_sample_confirmed
        && matches!(flight.phase, FlightPhase::Descent | FlightPhase::Approach)
    {
        if let (
            Some(dest_lat),
            Some(dest_lon),
            Some(cur_lat),
            Some(cur_lon),
            Some(p_lat),
            Some(p_lon),
        ) = (
            flight.dest_lat,
            flight.dest_lon,
            flight.lat,
            flight.lon,
            prev_lat,
            prev_lon,
        ) {
            let ground_track = random_flight::geo::initial_bearing(p_lat, p_lon, cur_lat, cur_lon);
            let bearing_to_dest =
                random_flight::geo::initial_bearing(cur_lat, cur_lon, dest_lat, dest_lon);

            let mut diff = (ground_track - bearing_to_dest).abs();
            if diff > 180.0 {
                diff = 360.0 - diff;
            }

            let counter_before = flight.divert_consecutive_polls;
            let alert_emitted = update_divert_counter(
                &mut flight.divert_consecutive_polls,
                diff > DIVERT_BEARING_THRESHOLD,
            );
            update
                .debug
                .push(FlightTrackerDebugEvent::diversion_decision(
                    flight,
                    DiversionDebugInput {
                        previous_lat: p_lat,
                        previous_lon: p_lon,
                        current_lat: cur_lat,
                        current_lon: cur_lon,
                        destination_lat: dest_lat,
                        destination_lon: dest_lon,
                        ground_track,
                        bearing_to_dest,
                        diff,
                        threshold: DIVERT_BEARING_THRESHOLD,
                        anomalous: diff > DIVERT_BEARING_THRESHOLD,
                        counter_before,
                        counter_after: flight.divert_consecutive_polls,
                        alert_emitted,
                    },
                ));
            if alert_emitted {
                update.emits.push(Emit::PossibleDivert);
            }
        }
    } else {
        flight.divert_consecutive_polls = 0;
    }

    update
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aviation::AltBaro;
    use crate::aviation::tracker::test_support::dt;

    fn tracked_flight() -> TrackedFlight {
        TrackedFlight {
            identifier: FlightIdentifier::Callsign("DLH1234".to_string()),
            callsign: Some("DLH1234".to_string()),
            alias_callsigns: vec!["DLH1234".to_string()],
            hex: None,
            hex_source: None,
            observed_callsign: None,
            target_confirmation: TargetConfirmation::Pending,
            phase: FlightPhase::Unknown,
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

    fn aircraft_on_ground() -> NearbyAircraft {
        NearbyAircraft {
            hex: Some("3C6589".to_string()),
            flight: Some("DLH1234".to_string()),
            r: None,
            t: None,
            alt_baro: Some(AltBaro::Ground),
            lat: Some(50.0),
            lon: Some(8.5),
            gs: Some(15.0),
            baro_rate: Some(0),
            geom_rate: None,
            squawk: Some("1000".to_string()),
            nav_modes: None,
            rssi: None,
            seen_pos: None,
        }
    }

    fn hit(ac: NearbyAircraft, used_hex: bool) -> Observation {
        Observation {
            used_hex,
            aliases: Vec::new(),
            outcome: PollOutcome::Hit(Box::new(ac)),
        }
    }

    /// Airborne aircraft fixture; callsign intentionally differs from
    /// `tracked_flight()`'s "DLH1234" so hex-hit tests exercise the
    /// sticky-confirmation / `AircraftVisible` paths explicitly.
    fn aircraft() -> NearbyAircraft {
        NearbyAircraft {
            hex: Some("3C6497".to_string()),
            flight: Some("DLH1929".to_string()),
            r: None,
            t: None,
            alt_baro: Some(AltBaro::Feet(12_000)),
            lat: Some(52.4),
            lon: Some(13.5),
            gs: Some(280.0),
            baro_rate: Some(-1_800),
            geom_rate: None,
            squawk: Some("1000".to_string()),
            nav_modes: None,
            rssi: None,
            seen_pos: None,
        }
    }

    fn miss(used_hex: bool) -> Observation {
        Observation {
            used_hex,
            aliases: Vec::new(),
            outcome: PollOutcome::Miss,
        }
    }

    #[test]
    fn apply_observed_aircraft_seeds_identity_and_telemetry() {
        let mut f = tracked_flight(); // Pending, callsign DLH1234, hex/type/telemetry empty
        let mut ac = aircraft_on_ground(); // callsign DLH1234, hex 3C6589, gs 15, squawk 1000
        ac.t = Some("A320".to_string());

        let newly =
            apply_observed_aircraft(&mut f, &ac, TargetConfirmation::ConfirmedByCallsign, true);

        assert_eq!(
            f.target_confirmation,
            TargetConfirmation::ConfirmedByCallsign
        );
        assert_eq!(f.observed_callsign.as_deref(), Some("DLH1234"));
        assert_eq!(f.hex.as_deref(), Some("3C6589"));
        assert_eq!(f.hex_source, Some(HexSource::Adsb));
        assert_eq!(f.aircraft_type.as_deref(), Some("A320"));
        assert_eq!(f.ground_speed_kts, Some(15.0));
        assert_eq!(f.lat, Some(50.0));
        assert_eq!(f.lon, Some(8.5));
        assert_eq!(f.squawk.as_deref(), Some("1000"));
        // Callsign was already known, so nothing was newly resolved.
        assert_eq!(newly, None);
        // The core seeds identity only; the caller owns these transition fields.
        assert_eq!(f.last_seen, None);
        assert_eq!(f.last_visible_at, None);
        assert_eq!(f.phase, FlightPhase::Unknown);
    }

    #[test]
    fn apply_observed_aircraft_resolves_callsign_for_hex_tracked_flight() {
        let mut f = tracked_flight();
        f.identifier = FlightIdentifier::Hex("3C6497".to_string());
        f.callsign = None;
        f.alias_callsigns = Vec::new();

        let ac = aircraft(); // callsign DLH1929, hex 3C6497

        let newly = apply_observed_aircraft(
            &mut f,
            &ac,
            TargetConfirmation::InferredByAssignedHex,
            false,
        );

        assert_eq!(newly.as_deref(), Some("DLH1929"));
        assert_eq!(f.callsign.as_deref(), Some("DLH1929"));
        assert!(f.alias_callsigns.iter().any(|a| a == "DLH1929"));
        assert_eq!(f.observed_callsign.as_deref(), Some("DLH1929"));
    }

    #[test]
    fn miss_within_grace_window_keeps_flight() {
        let mut f = tracked_flight();
        f.last_visible_at = Some(dt("2026-04-18T12:00:00Z"));
        // 5 min later: past lost-threshold (300 s) but well under removal (1800 s)
        let upd = advance_flight(&mut f, &miss(true), dt("2026-04-18T12:05:00Z"));
        assert_eq!(upd.removal, None);
        assert!(upd.emits.is_empty());
    }

    #[test]
    fn miss_past_removal_threshold_removes_and_emits_tracking_lost() {
        let mut f = tracked_flight();
        f.last_visible_at = Some(dt("2026-04-18T12:00:00Z"));
        // 31 min later: past removal (1800 s)
        let upd = advance_flight(&mut f, &miss(true), dt("2026-04-18T12:31:00Z"));
        assert_eq!(
            upd.removal,
            Some(RemovalReason::TrackingLost { secs: 31 * 60 })
        );
        assert_eq!(upd.emits, vec![Emit::TrackingLost]);
    }

    #[test]
    fn sticky_confirmation_keeps_prior_when_hex_visible_within_threshold() {
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        f.last_seen = Some(dt("2026-04-18T12:00:00Z"));
        // Override the observed callsign to "DLH9999", which mismatches the
        // tracked flight's "DLH1234". Without the sticky guard, this hex hit
        // would decay the confirmation to AircraftVisible; because we are
        // within the lost-threshold it must stay ConfirmedByCallsign.
        let mut ac = aircraft();
        ac.flight = Some("DLH9999".to_string());
        let upd = advance_flight(&mut f, &hit(ac, true), dt("2026-04-18T12:02:00Z"));
        assert_eq!(
            f.target_confirmation,
            TargetConfirmation::ConfirmedByCallsign
        );
        assert_eq!(f.last_visible_at, Some(dt("2026-04-18T12:02:00Z")));
        // A within-threshold hex hit must never remove or report tracking-lost.
        // (Not asserting `emits.is_empty()`: a phase-change emit is legitimately
        // possible here depending on the fixture's altitude/rate.)
        assert_eq!(upd.removal, None);
        assert!(!upd.emits.contains(&Emit::TrackingLost));
    }

    #[test]
    fn format_emit_landing_preserves_flight_time_from_carried_takeoff() {
        let flight = tracked_flight();
        let msg = format_emit(
            &flight,
            &Emit::Landing {
                takeoff_at: Some(dt("2026-04-18T10:30:00Z")),
            },
            dt("2026-04-18T12:00:00Z"),
        );

        assert!(msg.contains("Flugzeit: 1h30m"), "got: {msg}");
        assert!(!msg.contains("unbekannt"), "got: {msg}");
    }

    #[test]
    fn advance_landing_carries_pre_clear_takeoff_at_and_clears_flight_state() {
        let mut flight = tracked_flight();
        flight.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        flight.phase = FlightPhase::Approach;
        let takeoff = dt("2026-04-18T10:30:00Z");
        flight.takeoff_at = Some(takeoff);

        let update = advance_flight(
            &mut flight,
            &hit(aircraft_on_ground(), false),
            dt("2026-04-18T12:00:00Z"),
        );

        // The emitted Landing carries the takeoff time as it was BEFORE the
        // transition cleared it, so the deferred render shows the real duration.
        assert!(
            update.emits.iter().any(|emit| matches!(
                emit,
                Emit::Landing { takeoff_at } if *takeoff_at == Some(takeoff)
            )),
            "expected Emit::Landing carrying the pre-clear takeoff, got: {:?}",
            update.emits
        );
        // The mutated flight is reset for the next cycle (Task 2 contract).
        assert_eq!(flight.phase, FlightPhase::Ground);
        assert_eq!(flight.takeoff_at, None);
    }

    #[test]
    fn ground_to_airborne_sets_takeoff_at() {
        let mut f = tracked_flight();
        // Must be already confirmed so the became_target_confirmed block doesn't
        // fire and reset phase back to Unknown before we can see the transition.
        f.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        f.phase = FlightPhase::Ground;
        f.takeoff_at = None;

        // Aircraft callsign MUST match the tracked callsign ("DLH1234") so
        // target_confirmation_for_aircraft returns ConfirmedByCallsign →
        // direct_target_confirmed = true → phase_sample_confirmed = true.
        // aircraft() defaults to "DLH1929" — override it here.
        let mut ac = aircraft();
        ac.flight = Some("DLH1234".to_string());
        // AltBaro::Feet(1_500): is_on_ground checks ft < 200 → false, so not on
        // ground. detect_phase sees Ground prior + gs(180) > TAKEOFF_MIN_SPEED(60)
        // + vrate(2_500) > 0 → FlightPhase::Takeoff.
        ac.alt_baro = Some(AltBaro::Feet(1_500));
        ac.baro_rate = Some(2_500);
        ac.gs = Some(180.0);

        let now = dt("2026-04-18T12:10:00Z");
        let upd = advance_flight(&mut f, &hit(ac, false), now);

        assert_eq!(f.phase, FlightPhase::Takeoff, "phase = {:?}", f.phase);
        assert_eq!(f.takeoff_at, Some(now));
        assert!(
            upd.emits.iter().any(|e| matches!(e, Emit::Takeoff)),
            "expected Emit::Takeoff, got: {:?}",
            upd.emits
        );
    }

    // ── Task 4 tests ─────────────────────────────────────────────────────────

    #[test]
    fn newly_resolved_callsign_with_no_route_emits_fetch_route_followup() {
        // A HEX-identified flight with no callsign yet. When the observed
        // aircraft carries a callsign the HEX branch resolves it and, because
        // route is None, appends a FetchRoute followup.
        let mut f = tracked_flight();
        f.identifier = FlightIdentifier::Hex("3C6497".to_string());
        f.callsign = None;
        f.route = None;
        let mut ac = aircraft(); // flight = "DLH1929" — any callsign works for Hex id
        ac.flight = Some("DLH1929".to_string());
        let upd = advance_flight(&mut f, &hit(ac, true), dt("2026-04-18T12:05:00Z"));
        assert_eq!(f.callsign.as_deref(), Some("DLH1929"));
        assert_eq!(
            upd.followups,
            vec![Followup::FetchRoute {
                callsign: "DLH1929".to_string()
            }]
        );
    }

    #[test]
    fn first_target_confirmation_resets_phase_and_emits_adsb_visible() {
        // Pending flight gets its first callsign-confirmed hit.
        // The brief's assertions on f.phase==Unknown and polls_since_change==0
        // are mutually inconsistent: after the reset, detect_phase runs in the
        // same call and either changes the phase (breaking ==Unknown) or keeps
        // it and increments polls_since_change (breaking ==0). We assert only
        // the stable first-confirmation contract instead.
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::Pending;
        f.last_visible_at = None;
        f.phase = FlightPhase::Cruise;
        // ac.flight MUST match the tracked callsign "DLH1234" so that
        // target_confirmation_for_aircraft returns ConfirmedByCallsign →
        // became_target_confirmed = true → AdsbVisible is emitted.
        let mut ac = aircraft();
        ac.flight = Some("DLH1234".to_string());
        let upd = advance_flight(&mut f, &hit(ac, false), dt("2026-04-18T12:05:00Z"));
        assert!(
            f.target_confirmation.is_target_confirmed(),
            "expected target_confirmation to be confirmed, got: {:?}",
            f.target_confirmation
        );
        assert!(
            upd.emits.iter().any(|e| matches!(e, Emit::AdsbVisible)),
            "expected Emit::AdsbVisible on first confirmation, got: {:?}",
            upd.emits
        );
    }

    #[test]
    fn sustained_off_heading_in_descent_eventually_emits_possible_divert() {
        // Divert fires after DIVERT_CONSECUTIVE_POLLS (3) anomalous polls.
        // Flight is already confirmed; position starts north of destination
        // and moves further north so the ground-track bearing (~0°/north)
        // diverges from the bearing-to-destination (~190°/south) by >> 90°.
        // alt=8000 ft, baro_rate=-1500 fpm → detect_phase returns Approach
        // (vrate < -500, alt < 10000), keeping the flight in {Descent|Approach}
        // so the divert block runs on every poll.
        //
        // f.lat is seeded one step south of poll-0's observation (51.5 vs 52.0)
        // so the very first poll already has a non-degenerate (non-zero-length)
        // ground track and all 3+ consecutive anomalous polls count toward the
        // divert threshold.
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        f.phase = FlightPhase::Descent;
        f.dest_lat = Some(48.35);
        f.dest_lon = Some(11.78);
        // Seed prior position one step south so poll 0's observation (lat=52.0)
        // gives a northward ground track from the start.
        f.lat = Some(51.5);
        f.lon = Some(13.0);

        let mut emitted = false;
        for i in 0..6_i32 {
            let mut ac = aircraft();
            // Callsign must match so ConfirmedByCallsign fires each poll →
            // phase_sample_confirmed = true → divert block runs.
            ac.flight = Some("DLH1234".to_string());
            ac.alt_baro = Some(AltBaro::Feet(8_000));
            ac.baro_rate = Some(-1_500);
            // Move progressively north (away from the southern destination).
            // i=0 → 52.0, i=1 → 52.5, …; every poll is genuinely anomalous.
            ac.lat = Some(52.0 + f64::from(i) * 0.5);
            ac.lon = Some(13.0);
            let upd = advance_flight(&mut f, &hit(ac, false), dt("2026-04-18T12:05:00Z"));
            if upd.emits.iter().any(|e| matches!(e, Emit::PossibleDivert)) {
                emitted = true;
                break;
            }
        }
        assert!(
            emitted,
            "expected PossibleDivert after sustained off-heading polls"
        );
    }

    // ── Follow-up tests: untested advance_flight branches ────────────────────

    #[test]
    fn error_outcome_sets_poll_at_and_records_debug_event_only() {
        let mut f = tracked_flight();
        // Capture telemetry before the call; the error branch must not mutate it.
        let alt_before = f.altitude_ft;
        let lat_before = f.lat;
        let lon_before = f.lon;
        let vrate_before = f.vertical_rate_fpm;
        let gs_before = f.ground_speed_kts;
        let squawk_before = f.squawk.clone();

        let now = dt("2026-04-18T12:00:00Z");
        let obs = Observation {
            used_hex: true,
            aliases: Vec::new(),
            outcome: PollOutcome::Error,
        };
        let upd = advance_flight(&mut f, &obs, now);

        assert_eq!(f.last_adsb_poll_at, Some(now));
        assert!(upd.emits.is_empty(), "Error must not emit: {:?}", upd.emits);
        assert!(
            upd.removal.is_none(),
            "Error must not remove: {:?}",
            upd.removal
        );
        assert!(
            upd.followups.is_empty(),
            "Error must not produce followups: {:?}",
            upd.followups
        );
        assert!(!upd.debug.is_empty(), "Error must record a debug event");
        // Telemetry must be unchanged.
        assert_eq!(f.altitude_ft, alt_before);
        assert_eq!(f.lat, lat_before);
        assert_eq!(f.lon, lon_before);
        assert_eq!(f.vertical_rate_fpm, vrate_before);
        assert_eq!(f.ground_speed_kts, gs_before);
        assert_eq!(f.squawk, squawk_before);
    }

    #[test]
    fn timeout_outcome_sets_poll_at_and_records_debug_event_only() {
        let mut f = tracked_flight();
        let alt_before = f.altitude_ft;
        let lat_before = f.lat;
        let lon_before = f.lon;
        let vrate_before = f.vertical_rate_fpm;
        let gs_before = f.ground_speed_kts;
        let squawk_before = f.squawk.clone();

        let now = dt("2026-04-18T12:01:00Z");
        let obs = Observation {
            used_hex: false,
            aliases: Vec::new(),
            outcome: PollOutcome::Timeout,
        };
        let upd = advance_flight(&mut f, &obs, now);

        assert_eq!(f.last_adsb_poll_at, Some(now));
        assert!(
            upd.emits.is_empty(),
            "Timeout must not emit: {:?}",
            upd.emits
        );
        assert!(
            upd.removal.is_none(),
            "Timeout must not remove: {:?}",
            upd.removal
        );
        assert!(
            upd.followups.is_empty(),
            "Timeout must not produce followups: {:?}",
            upd.followups
        );
        assert!(!upd.debug.is_empty(), "Timeout must record a debug event");
        // Telemetry must be unchanged.
        assert_eq!(f.altitude_ft, alt_before);
        assert_eq!(f.lat, lat_before);
        assert_eq!(f.lon, lon_before);
        assert_eq!(f.vertical_rate_fpm, vrate_before);
        assert_eq!(f.ground_speed_kts, gs_before);
        assert_eq!(f.squawk, squawk_before);
    }

    #[test]
    fn squawk_7700_on_confirmed_flight_emits_squawk_emergency() {
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        f.last_seen = Some(dt("2026-04-18T12:00:00Z"));
        f.squawk = Some("1000".to_string()); // non-emergency prior squawk

        let mut ac = aircraft_on_ground();
        // Must match tracked callsign "DLH1234" to get ConfirmedByCallsign →
        // direct_target_confirmed = true → phase_sample_confirmed = true.
        ac.flight = Some("DLH1234".to_string());
        ac.squawk = Some("7700".to_string());

        let now = dt("2026-04-18T12:02:00Z");
        let upd = advance_flight(&mut f, &hit(ac, false), now);

        let expected_meaning = emergency_squawk_meaning("7700").unwrap();
        assert!(
            upd.emits.iter().any(|e| matches!(
                e,
                Emit::SquawkEmergency { code, meaning }
                    if code == "7700" && meaning == expected_meaning
            )),
            "expected SquawkEmergency(7700, {expected_meaning:?}), got: {:?}",
            upd.emits
        );
    }

    #[test]
    fn unconfirmed_aircraft_early_return_does_not_bump_visible_or_polls() {
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::Pending;
        let known_visible_at = dt("2026-04-18T11:00:00Z");
        f.last_visible_at = Some(known_visible_at);
        let known_polls = 5u32;
        f.polls_since_change = known_polls;

        // ac.flight does NOT match the tracked callsign "DLH1234"; used_hex=false
        // so only the callsign path runs → target_confirmation_for_aircraft
        // returns None → early return before last_visible_at is touched.
        let mut ac = aircraft();
        ac.flight = Some("DLH9999".to_string());

        let now = dt("2026-04-18T12:00:00Z");
        let upd = advance_flight(&mut f, &hit(ac, false), now);

        assert_eq!(f.last_adsb_poll_at, Some(now));
        assert_eq!(
            f.last_visible_at,
            Some(known_visible_at),
            "last_visible_at must not be bumped on unconfirmed early return"
        );
        assert_eq!(
            f.polls_since_change, known_polls,
            "polls_since_change must not be bumped on unconfirmed early return"
        );
        assert!(
            upd.emits.is_empty(),
            "unconfirmed early return must not emit: {:?}",
            upd.emits
        );
        assert!(
            upd.removal.is_none(),
            "unconfirmed early return must not remove: {:?}",
            upd.removal
        );
        assert!(
            upd.followups.is_empty(),
            "unconfirmed early return must not produce followups: {:?}",
            upd.followups
        );
        // The None-return branch in advance_flight does not push a debug event.
        assert!(
            upd.debug.is_empty(),
            "unconfirmed early return must not record debug events: {:?}",
            upd.debug
        );
    }
}
