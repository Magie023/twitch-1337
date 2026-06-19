use chrono::DateTime;
use chrono::Utc;

use crate::aviation::AviationstackFlightMetadata;

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

fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn format_time(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|dt| dt.format("%H:%M").to_string())
}

fn first_present<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    values
        .into_iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
}

fn format_airport(
    iata: Option<&str>,
    icao: Option<&str>,
    airport: Option<&str>,
    terminal: Option<&str>,
    gate: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    parts.push(
        first_present([iata, icao, airport])
            .map(str::trim)
            .unwrap_or("?")
            .to_string(),
    );
    if let Some(terminal) = present(terminal) {
        parts.push(format!("T{terminal}"));
    }
    if let Some(gate) = present(gate) {
        parts.push(format!("Gate {gate}"));
    }
    parts.join(" ")
}

fn delay_part(label: &str, delay: Option<i64>) -> Option<String> {
    let delay = delay?;
    if delay > 0 {
        Some(format!("{label} delay: {delay}m"))
    } else {
        None
    }
}

pub(crate) fn msg_aviationstack_info(metadata: &AviationstackFlightMetadata) -> String {
    let flight = first_present([
        metadata.flight_iata.as_deref(),
        metadata.flight_icao.as_deref(),
        metadata.flight_number.as_deref(),
    ])
    .unwrap_or("Flight");
    let airline = present(metadata.airline_name.as_deref())
        .map(|name| format!(" {name}"))
        .unwrap_or_default();
    let dep = format_airport(
        metadata.departure_iata.as_deref(),
        metadata.departure_icao.as_deref(),
        metadata.departure_airport.as_deref(),
        metadata.departure_terminal.as_deref(),
        metadata.departure_gate.as_deref(),
    );
    let arr = format_airport(
        metadata.arrival_iata.as_deref(),
        metadata.arrival_icao.as_deref(),
        metadata.arrival_airport.as_deref(),
        metadata.arrival_terminal.as_deref(),
        metadata.arrival_gate.as_deref(),
    );

    let mut parts = vec![format!("🛫 {flight}{airline}"), format!("{dep} -> {arr}")];

    if let Some(dep_time) = format_time(
        metadata
            .departure_actual_runway
            .as_ref()
            .cloned()
            .or_else(|| metadata.departure_actual.as_ref().cloned())
            .or_else(|| metadata.departure_estimated.as_ref().cloned())
            .or_else(|| metadata.departure_scheduled.as_ref().cloned()),
    ) {
        parts.push(format!("Dep: {dep_time}"));
    }
    if let Some(arr_time) = format_time(
        metadata
            .arrival_actual
            .as_ref()
            .cloned()
            .or_else(|| metadata.arrival_estimated.as_ref().cloned())
            .or_else(|| metadata.arrival_scheduled.as_ref().cloned()),
    ) {
        parts.push(format!("Arr: {arr_time}"));
    }
    if let Some(baggage) = present(metadata.arrival_baggage.as_deref()) {
        parts.push(format!("Baggage: {baggage}"));
    }
    if let Some(part) = delay_part("Dep", metadata.departure_delay_minutes) {
        parts.push(part);
    }
    if let Some(part) = delay_part("Arr", metadata.arrival_delay_minutes) {
        parts.push(part);
    }
    if let Some(status) = present(metadata.flight_status.as_deref()) {
        parts.push(format!("Status: {status}"));
    }
    if let Some(flight_date) = present(metadata.flight_date.as_deref()) {
        parts.push(format!("Date: {flight_date}"));
    }

    let mut aircraft = Vec::new();
    if let Some(aircraft_type) = present(metadata.aircraft_icao.as_deref()) {
        aircraft.push(aircraft_type.to_string());
    }
    if let Some(registration) = present(metadata.aircraft_registration.as_deref()) {
        aircraft.push(registration.to_string());
    }
    if !aircraft.is_empty() {
        parts.push(format!("Aircraft: {}", aircraft.join(" ")));
    }
    if let Some(icao24) = present(metadata.aircraft_icao24.as_deref()) {
        parts.push(format!("ICAO24: {}", icao24.to_uppercase()));
    }

    parts.join(" | ")
}

fn format_adsb_link_suffix(hex: Option<&str>) -> String {
    present(hex)
        .map(|hex| {
            format!(
                " | https://globe.adsbexchange.com/?icao={}",
                hex.to_ascii_lowercase()
            )
        })
        .unwrap_or_default()
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
    format!(
        "Tracke {} Okayge{}",
        format_flight_prefix(flight),
        format_adsb_link_suffix(flight.hex.as_deref())
    )
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

pub(crate) fn msg_landing(
    flight: &TrackedFlight,
    takeoff_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> String {
    match takeoff_at {
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
    let adsb_link = format_adsb_link_suffix(flight.hex.as_deref());
    let prefix = format_flight_prefix(flight);
    if flight.target_confirmation == super::TargetConfirmation::AircraftVisible {
        let observed = flight
            .observed_callsign
            .as_deref()
            .map(|callsign| format!(" | ADS-B aktuell {callsign}"))
            .unwrap_or_default();
        let elapsed = now.signed_duration_since(flight.tracked_at);
        return format!(
            "{prefix} | Aircraft sichtbar, Zielflug noch nicht bestätigt{observed} | seit {} getrackt{adsb_link}",
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
        "{prefix} | {} {alt}{speed}{squawk} | {tracking_time}{adsb_link}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aviation::tracker::{
        TargetConfirmation,
        test_support::{dt, tracked_flight},
    };

    /// A confirmed, non-visible flight with `hex` swapped in, off the shared
    /// fixture so this module doesn't carry its own full-field copy.
    fn status_flight(hex: Option<&str>) -> TrackedFlight {
        TrackedFlight {
            hex: hex.map(str::to_string),
            target_confirmation: TargetConfirmation::ConfirmedByCallsign,
            ..tracked_flight()
        }
    }

    #[test]
    fn msg_track_started_includes_adsb_link_when_hex_known() {
        let msg = msg_track_started(&status_flight(Some("3C6589")));

        assert_eq!(
            msg,
            "Tracke DLH1929 Okayge | https://globe.adsbexchange.com/?icao=3c6589"
        );
    }

    #[test]
    fn msg_track_started_omits_adsb_link_without_hex() {
        let msg = msg_track_started(&status_flight(None));

        assert_eq!(msg, "Tracke DLH1929 Okayge");
    }

    #[test]
    fn msg_flight_status_includes_adsb_link_when_hex_known() {
        let now = dt("2026-04-18T12:30:00Z");

        let msg = msg_flight_status(&status_flight(Some("3C6589")), now);

        assert_eq!(
            msg,
            "DLH1929 | Cruise FL120 | 280kts | Squawk 1000 | seit 30m getrackt | https://globe.adsbexchange.com/?icao=3c6589"
        );
    }

    #[test]
    fn msg_flight_status_omits_adsb_link_without_hex() {
        let now = dt("2026-04-18T12:30:00Z");

        let msg = msg_flight_status(&status_flight(None), now);

        assert_eq!(
            msg,
            "DLH1929 | Cruise FL120 | 280kts | Squawk 1000 | seit 30m getrackt"
        );
    }
}
