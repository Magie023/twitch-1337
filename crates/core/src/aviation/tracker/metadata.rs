use tracing::{debug, warn};

use crate::aviation::{AviationClient, AviationstackFlightMetadata, iata_to_coords};

use super::{FlightIdentifier, TrackedFlight};

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

    if flight.callsign.is_none()
        && let Some(callsign) = metadata_callsign(&metadata)
    {
        flight.callsign = Some(callsign);
    }

    if let (Some(origin), Some(dest)) = (
        metadata.departure_iata.as_deref(),
        metadata.arrival_iata.as_deref(),
    ) {
        set_route_from_iata(flight, origin, dest);
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

pub(crate) async fn fetch_aviationstack_metadata_for_tracking(
    aviation_client: &AviationClient,
    identifier: &FlightIdentifier,
    callsign: Option<&str>,
) -> Option<AviationstackFlightMetadata> {
    match aviation_client
        .get_aviationstack_flight_metadata(identifier, callsign)
        .await
    {
        Ok(Some(metadata)) => Some(metadata),
        Ok(None) => {
            debug!(identifier = %identifier, "No aviationstack metadata found for flight");
            None
        }
        Err(e) => {
            warn!(error = ?e, identifier = %identifier, "Aviationstack metadata lookup failed");
            None
        }
    }
}
