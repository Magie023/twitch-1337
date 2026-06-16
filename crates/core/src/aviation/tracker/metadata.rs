use crate::aviation::{AviationstackFlightMetadata, iata_to_coords};

use super::{HexSource, TrackedFlight};

pub(crate) fn set_route_from_iata(flight: &mut TrackedFlight, origin: &str, dest: &str) {
    let origin = origin.trim().to_uppercase();
    let dest = dest.trim().to_uppercase();
    if origin.is_empty() || dest.is_empty() {
        return;
    }

    if let Some((lat, lon, _)) = iata_to_coords(&dest) {
        flight.dest_lat = Some(lat);
        flight.dest_lon = Some(lon);
    }
    flight.route = Some((origin, dest));
}

pub(crate) fn normalize_flight_code(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_uppercase())
    }
}

pub(crate) fn normalize_icao24(value: &str) -> Option<String> {
    let value = value.trim();
    if value.len() == 6 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(value.to_uppercase())
    } else {
        None
    }
}

pub(crate) fn set_hex_if_consistent(
    flight: &mut TrackedFlight,
    candidate: &str,
    source: HexSource,
    promote_aviationstack_to_adsb: bool,
) -> bool {
    let Some(hex) = normalize_icao24(candidate) else {
        return false;
    };

    if flight
        .hex
        .as_deref()
        .is_some_and(|current| !current.eq_ignore_ascii_case(&hex))
    {
        return false;
    }

    flight.hex = Some(hex);
    match source {
        HexSource::UserInput => {
            if flight.hex_source.is_none() {
                flight.hex_source = Some(HexSource::UserInput);
            }
        }
        HexSource::AviationStack => {
            if flight.hex_source.is_none() {
                flight.hex_source = Some(HexSource::AviationStack);
            }
        }
        HexSource::Adsb => {
            if flight.hex_source.is_none()
                || (promote_aviationstack_to_adsb
                    && flight.hex_source == Some(HexSource::AviationStack))
            {
                flight.hex_source = Some(HexSource::Adsb);
            }
        }
    }

    true
}

pub(crate) fn metadata_callsign(metadata: &AviationstackFlightMetadata) -> Option<String> {
    metadata
        .flight_icao
        .as_deref()
        .and_then(normalize_flight_code)
        .or_else(|| {
            let airline = metadata.airline_icao.as_deref()?.trim();
            let number = metadata.flight_number.as_deref()?.trim();
            if airline.is_empty() || number.is_empty() {
                None
            } else {
                Some(format!("{}{}", airline.to_uppercase(), number))
            }
        })
}

pub(crate) fn apply_aviationstack_metadata(
    flight: &mut TrackedFlight,
    metadata: AviationstackFlightMetadata,
) {
    let takeoff_at = metadata.takeoff_time();

    if flight.scheduled_departure_at.is_none() {
        flight
            .scheduled_departure_at
            .clone_from(&metadata.departure_scheduled);
    }

    if let Some(callsign) = metadata_callsign(&metadata) {
        let should_apply = flight.callsign.is_none()
            || flight
                .callsign
                .as_deref()
                .zip(metadata.flight_iata.as_deref())
                .is_some_and(|(current, iata)| current.eq_ignore_ascii_case(iata));
        if should_apply {
            flight.callsign = Some(callsign);
        }
    }

    if let (Some(origin), Some(dest)) = (
        metadata.departure_iata.as_deref(),
        metadata.arrival_iata.as_deref(),
    ) {
        set_route_from_iata(flight, origin, dest);
    }

    if let Some(hex) = metadata.aircraft_icao24.as_deref() {
        set_hex_if_consistent(flight, hex, HexSource::AviationStack, false);
    }

    if flight.aircraft_type.is_none()
        && let Some(aircraft_type) = metadata.aircraft_icao
    {
        flight.aircraft_type = Some(aircraft_type.to_uppercase());
    }

    if let Some(takeoff_at) = takeoff_at {
        flight.takeoff_at = Some(takeoff_at);
    }
}
