use crate::aviation::{AltBaro, NearbyAircraft};

use super::{DIVERT_CONSECUTIVE_POLLS, FlightPhase, TrackedFlight};

const CLIMB_RATE_THRESHOLD: i64 = 500;
const DESCENT_RATE_THRESHOLD: i64 = -500;
const CRUISE_RATE_THRESHOLD: i64 = 300;
const CRUISE_MIN_ALTITUDE: i64 = 10_000;
const APPROACH_MAX_ALTITUDE: i64 = 10_000;
const GROUND_MAX_ALTITUDE: i64 = 200;
const GROUND_MAX_SPEED: f64 = 30.0;
const TAKEOFF_MIN_SPEED: f64 = 60.0;
pub(crate) const CRUISE_STABLE_POLLS: u32 = 2;

const SQUAWK_HIJACK: &str = "7500";
const SQUAWK_RADIO_FAILURE: &str = "7600";
const SQUAWK_EMERGENCY: &str = "7700";

pub(crate) fn altitude_ft(ac: &NearbyAircraft) -> Option<i64> {
    match &ac.alt_baro {
        Some(AltBaro::Feet(ft)) => Some(*ft),
        Some(AltBaro::Ground) => Some(0),
        None => None,
    }
}

pub(crate) fn vertical_rate(ac: &NearbyAircraft) -> Option<i64> {
    ac.baro_rate.or(ac.geom_rate)
}

fn is_on_ground(ac: &NearbyAircraft) -> bool {
    match &ac.alt_baro {
        Some(AltBaro::Ground) => true,
        Some(AltBaro::Feet(ft)) => {
            *ft < GROUND_MAX_ALTITUDE && ac.gs.unwrap_or(0.0) < GROUND_MAX_SPEED
        }
        None => false,
    }
}

pub(crate) fn is_airborne_phase(phase: FlightPhase) -> bool {
    matches!(
        phase,
        FlightPhase::Takeoff
            | FlightPhase::Climb
            | FlightPhase::Cruise
            | FlightPhase::Descent
            | FlightPhase::Approach
    )
}

pub(crate) fn update_divert_counter(
    divert_consecutive_polls: &mut u32,
    is_anomalous: bool,
) -> bool {
    if is_anomalous {
        *divert_consecutive_polls = (*divert_consecutive_polls).saturating_add(1);
        *divert_consecutive_polls >= DIVERT_CONSECUTIVE_POLLS
    } else {
        *divert_consecutive_polls = 0;
        false
    }
}

pub(crate) fn detect_phase(flight: &TrackedFlight, ac: &NearbyAircraft) -> FlightPhase {
    let on_ground = is_on_ground(ac);
    let alt = altitude_ft(ac);
    let vrate = vertical_rate(ac);
    let gs = ac.gs.unwrap_or(0.0);
    let has_approach_mode = ac
        .nav_modes
        .as_ref()
        .is_some_and(|modes| modes.iter().any(|m| m == "approach"));

    if on_ground && !matches!(flight.phase, FlightPhase::Ground | FlightPhase::Unknown) {
        return FlightPhase::Landing;
    }

    if on_ground {
        return FlightPhase::Ground;
    }

    if matches!(flight.phase, FlightPhase::Ground | FlightPhase::Unknown)
        && gs > TAKEOFF_MIN_SPEED
        && vrate.unwrap_or(0) > 0
    {
        return FlightPhase::Takeoff;
    }

    if let Some(vr) = vrate
        && vr < DESCENT_RATE_THRESHOLD
    {
        if let Some(alt_val) = alt
            && (alt_val < APPROACH_MAX_ALTITUDE
                || has_approach_mode
                || matches!(flight.phase, FlightPhase::Approach))
        {
            return FlightPhase::Approach;
        }
        return FlightPhase::Descent;
    }

    if has_approach_mode && vrate.unwrap_or(0) < 0 {
        return FlightPhase::Approach;
    }

    if let (Some(alt_val), Some(vr)) = (alt, vrate)
        && alt_val > CRUISE_MIN_ALTITUDE
        && vr.abs() < CRUISE_RATE_THRESHOLD
        && flight.polls_since_change >= CRUISE_STABLE_POLLS
        && !matches!(flight.phase, FlightPhase::Descent | FlightPhase::Approach)
    {
        return FlightPhase::Cruise;
    }

    if let Some(vr) = vrate
        && vr > CLIMB_RATE_THRESHOLD
    {
        return FlightPhase::Climb;
    }

    flight.phase
}

pub(crate) fn emergency_squawk_meaning(squawk: &str) -> Option<&'static str> {
    match squawk {
        SQUAWK_HIJACK => Some("Hijack"),
        SQUAWK_RADIO_FAILURE => Some("Radio Failure"),
        SQUAWK_EMERGENCY => Some("Emergency"),
        _ => None,
    }
}
