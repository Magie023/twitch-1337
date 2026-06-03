use chrono::DateTime;
use chrono::Utc;

use super::TrackedFlight;

fn format_duration_hm(d: chrono::TimeDelta) -> String {
    let hours = d.num_hours();
    let mins = d.num_minutes() % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else {
        format!("{mins}m")
    }
}

fn format_alt(alt_ft: Option<i64>) -> String {
    match alt_ft {
        Some(ft) if ft >= 1000 => format!("FL{}", ft / 100),
        Some(ft) => format!("{ft}ft"),
        None => "?".to_string(),
    }
}

fn format_route(route: &Option<(String, String)>) -> String {
    match route {
        Some((orig, dest)) => format!(" {orig}\u{2192}{dest}"),
        None => String::new(),
    }
}

fn format_flight_prefix(flight: &TrackedFlight) -> String {
    let name = flight
        .callsign
        .as_deref()
        .unwrap_or(flight.identifier.as_str());
    let typ = flight
        .aircraft_type
        .as_ref()
        .map(|t| format!(" ({t})"))
        .unwrap_or_default();
    let route = format_route(&flight.route);
    format!("{name}{typ}{route}")
}

pub(crate) fn msg_track_started(flight: &TrackedFlight) -> String {
    format!("Tracke {} Okayge", format_flight_prefix(flight))
}

pub(crate) fn msg_takeoff(flight: &TrackedFlight) -> String {
    format!("{} ist gestartet! \u{2708}", format_flight_prefix(flight))
}

pub(crate) fn msg_cruise(flight: &TrackedFlight) -> String {
    format!(
        "{} cruist auf {}",
        format_flight_prefix(flight),
        format_alt(flight.altitude_ft)
    )
}

pub(crate) fn msg_descent(flight: &TrackedFlight) -> String {
    format!("{} hat Descent eingeleitet", format_flight_prefix(flight))
}

pub(crate) fn msg_approach(flight: &TrackedFlight) -> String {
    format!("{} ist im Approach", format_flight_prefix(flight))
}

pub(crate) fn msg_landing(flight: &TrackedFlight, now: DateTime<Utc>) -> String {
    match flight.takeoff_at {
        Some(takeoff_at) => {
            let duration = now.signed_duration_since(takeoff_at);
            format!(
                "{} ist gelandet! Flugzeit: {}",
                format_flight_prefix(flight),
                format_duration_hm(duration)
            )
        }
        None => format!(
            "{} ist gelandet! Flugzeit: unbekannt (Takeoff nicht beobachtet)",
            format_flight_prefix(flight)
        ),
    }
}

pub(crate) fn msg_squawk_emergency(flight: &TrackedFlight, code: &str, meaning: &str) -> String {
    format!(
        "\u{26a0} {} squawkt {code}! ({meaning})",
        format_flight_prefix(flight)
    )
}

pub(crate) fn msg_possible_divert(flight: &TrackedFlight) -> String {
    format!(
        "\u{26a0} {} scheint zu diverten!",
        format_flight_prefix(flight)
    )
}

pub(crate) fn msg_tracking_lost(flight: &TrackedFlight) -> String {
    let name = flight
        .callsign
        .as_deref()
        .unwrap_or(flight.identifier.as_str());
    format!("{name} Signal verloren, wird nicht mehr getrackt")
}

pub(crate) fn msg_adsb_visible(flight: &TrackedFlight) -> String {
    format!(
        "{} ist jetzt im ADS-B sichtbar",
        format_flight_prefix(flight)
    )
}

pub(crate) fn msg_pending_expired(flight: &TrackedFlight) -> String {
    format!(
        "{} ist nicht im ADS-B aufgetaucht, wird nicht mehr getrackt",
        format_flight_prefix(flight)
    )
}

pub(crate) fn msg_flight_status(flight: &TrackedFlight, now: DateTime<Utc>) -> String {
    let prefix = format_flight_prefix(flight);
    if flight.target_confirmation == super::TargetConfirmation::AircraftVisible {
        let observed = flight
            .observed_callsign
            .as_deref()
            .map(|callsign| format!(" | ADS-B aktuell {callsign}"))
            .unwrap_or_default();
        let elapsed = now.signed_duration_since(flight.tracked_at);
        return format!(
            "{prefix} | Aircraft sichtbar, Zielflug noch nicht bestätigt{observed} | seit {} getrackt",
            format_duration_hm(elapsed)
        );
    }

    let alt = format_alt(flight.altitude_ft);
    let speed = flight
        .ground_speed_kts
        .map(|gs| format!(" | {gs:.0}kts"))
        .unwrap_or_default();
    let squawk = flight
        .squawk
        .as_ref()
        .map(|s| format!(" | Squawk {s}"))
        .unwrap_or_default();
    let elapsed = now.signed_duration_since(flight.tracked_at);
    let tracking_time = format!("seit {} getrackt", format_duration_hm(elapsed));
    format!(
        "{prefix} | {} {alt}{speed}{squawk} | {tracking_time}",
        flight.phase
    )
}

pub(crate) fn msg_flights_list(flights: &[TrackedFlight]) -> String {
    if flights.is_empty() {
        return "Keine Fl\u{00fc}ge getrackt".to_string();
    }
    let parts: Vec<String> = flights
        .iter()
        .map(|f| {
            let name = f.callsign.as_deref().unwrap_or(f.identifier.as_str());
            let alt = format_alt(f.altitude_ft);
            let phase = if f.target_confirmation == super::TargetConfirmation::AircraftVisible {
                "AircraftVisible".to_string()
            } else {
                format!("{}", f.phase)
            };
            format!("{name} ({phase} {alt})")
        })
        .collect();
    format!("Getrackte Fl\u{00fc}ge: {}", parts.join(" | "))
}
