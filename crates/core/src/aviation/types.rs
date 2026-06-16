//! API response types for ADS-B aggregators, adsbdb, and Aviationstack.
//!
//! Public types here are re-exported from `aviation/mod.rs`.
//! Internal envelope types (`AdsbDbResponse`, `Aviationstack*`) are
//! `pub(super)` so `client.rs` can deserialize them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// --- ADS-B v2 response types ---

#[derive(Debug, Deserialize)]
pub(super) struct AdsbAircraftResponse {
    #[serde(default, rename = "ac", alias = "aircraft")]
    pub(super) aircraft: Vec<NearbyAircraft>,
}

#[derive(Debug, Deserialize)]
pub struct NearbyAircraft {
    pub hex: Option<String>,
    pub flight: Option<String>,
    pub r: Option<String>,
    pub t: Option<String>,
    pub alt_baro: Option<AltBaro>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub gs: Option<f64>,
    pub baro_rate: Option<i64>,
    pub geom_rate: Option<i64>,
    pub squawk: Option<String>,
    pub nav_modes: Option<Vec<String>>,
    pub rssi: Option<f64>,
    pub seen_pos: Option<f64>,
}

#[derive(Debug, Clone)]
pub enum AltBaro {
    Feet(i64),
    Ground,
}

impl<'de> Deserialize<'de> for AltBaro {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(AltBaro::Feet(i))
                } else {
                    Ok(AltBaro::Feet(n.as_f64().unwrap_or(0.0) as i64))
                }
            }
            serde_json::Value::String(s) if s == "ground" => Ok(AltBaro::Ground),
            _ => Ok(AltBaro::Ground),
        }
    }
}

// --- adsbdb types ---

#[derive(Debug, Deserialize)]
pub(super) struct AdsbDbResponse {
    pub response: AdsbDbResponseInner,
}

#[derive(Debug, Deserialize)]
pub(super) struct AdsbDbResponseInner {
    pub flightroute: Option<FlightRoute>,
}

#[derive(Debug, Deserialize)]
pub struct FlightRoute {
    pub origin: Airport,
    pub destination: Airport,
}

#[derive(Debug, Deserialize)]
pub struct Airport {
    pub iata_code: String,
}

// --- adsbdb airline types ---

#[derive(Debug, Deserialize)]
pub(super) struct AdsbDbAirlineResponse {
    pub response: Vec<AdsbDbAirline>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AdsbDbAirline {
    pub icao: String,
}

// --- aviationstack types ---

#[derive(Debug, Deserialize)]
pub(super) struct AviationstackFlightsResponse {
    #[serde(default)]
    pub data: Vec<AviationstackFlight>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AviationstackFlight {
    #[serde(default)]
    flight_date: Option<String>,
    #[serde(default)]
    flight_status: Option<String>,
    #[serde(default)]
    flight: Option<AviationstackFlightIdentity>,
    #[serde(default)]
    airline: Option<AviationstackAirline>,
    #[serde(default)]
    departure: Option<AviationstackDeparture>,
    #[serde(default)]
    arrival: Option<AviationstackArrival>,
    #[serde(default)]
    aircraft: Option<AviationstackAircraft>,
}

#[derive(Debug, Deserialize)]
struct AviationstackFlightIdentity {
    number: Option<String>,
    iata: Option<String>,
    icao: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AviationstackAirline {
    name: Option<String>,
    iata: Option<String>,
    icao: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AviationstackDeparture {
    airport: Option<String>,
    iata: Option<String>,
    icao: Option<String>,
    terminal: Option<String>,
    gate: Option<String>,
    scheduled: Option<String>,
    estimated: Option<String>,
    actual: Option<String>,
    actual_runway: Option<String>,
    delay: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AviationstackArrival {
    airport: Option<String>,
    iata: Option<String>,
    icao: Option<String>,
    terminal: Option<String>,
    gate: Option<String>,
    baggage: Option<String>,
    scheduled: Option<String>,
    estimated: Option<String>,
    actual: Option<String>,
    delay: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AviationstackAircraft {
    registration: Option<String>,
    icao24: Option<String>,
    icao: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AviationstackFlightMetadata {
    pub flight_date: Option<String>,
    pub flight_status: Option<String>,
    pub flight_iata: Option<String>,
    pub flight_icao: Option<String>,
    pub flight_number: Option<String>,
    pub airline_iata: Option<String>,
    pub airline_icao: Option<String>,
    pub airline_name: Option<String>,
    pub departure_airport: Option<String>,
    pub departure_iata: Option<String>,
    pub departure_icao: Option<String>,
    pub departure_terminal: Option<String>,
    pub departure_gate: Option<String>,
    pub departure_scheduled: Option<DateTime<Utc>>,
    pub departure_estimated: Option<DateTime<Utc>>,
    pub departure_actual: Option<DateTime<Utc>>,
    pub departure_actual_runway: Option<DateTime<Utc>>,
    pub departure_delay_minutes: Option<i64>,
    pub arrival_airport: Option<String>,
    pub arrival_iata: Option<String>,
    pub arrival_icao: Option<String>,
    pub arrival_terminal: Option<String>,
    pub arrival_gate: Option<String>,
    pub arrival_baggage: Option<String>,
    pub arrival_scheduled: Option<DateTime<Utc>>,
    pub arrival_estimated: Option<DateTime<Utc>>,
    pub arrival_actual: Option<DateTime<Utc>>,
    pub arrival_delay_minutes: Option<i64>,
    pub aircraft_registration: Option<String>,
    pub aircraft_icao24: Option<String>,
    pub aircraft_icao: Option<String>,
}

impl AviationstackFlightMetadata {
    pub fn takeoff_time(&self) -> Option<DateTime<Utc>> {
        self.departure_actual_runway
            .as_ref()
            .cloned()
            .or_else(|| self.departure_actual.as_ref().cloned())
    }
}

impl From<AviationstackFlight> for AviationstackFlightMetadata {
    fn from(value: AviationstackFlight) -> Self {
        let flight = value.flight;
        let airline = value.airline;
        let departure = value.departure;
        let arrival = value.arrival;
        let aircraft = value.aircraft;

        Self {
            flight_date: value.flight_date,
            flight_status: value.flight_status,
            flight_iata: flight.as_ref().and_then(|f| f.iata.clone()),
            flight_icao: flight.as_ref().and_then(|f| f.icao.clone()),
            flight_number: flight.as_ref().and_then(|f| f.number.clone()),
            airline_iata: airline.as_ref().and_then(|a| a.iata.clone()),
            airline_icao: airline.as_ref().and_then(|a| a.icao.clone()),
            airline_name: airline.as_ref().and_then(|a| a.name.clone()),
            departure_airport: departure.as_ref().and_then(|d| d.airport.clone()),
            departure_iata: departure.as_ref().and_then(|d| d.iata.clone()),
            departure_icao: departure.as_ref().and_then(|d| d.icao.clone()),
            departure_terminal: departure.as_ref().and_then(|d| d.terminal.clone()),
            departure_gate: departure.as_ref().and_then(|d| d.gate.clone()),
            departure_scheduled: departure
                .as_ref()
                .and_then(|d| parse_aviationstack_datetime(d.scheduled.as_deref())),
            departure_estimated: departure
                .as_ref()
                .and_then(|d| parse_aviationstack_datetime(d.estimated.as_deref())),
            departure_actual: departure
                .as_ref()
                .and_then(|d| parse_aviationstack_datetime(d.actual.as_deref())),
            departure_actual_runway: departure
                .as_ref()
                .and_then(|d| parse_aviationstack_datetime(d.actual_runway.as_deref())),
            departure_delay_minutes: departure.as_ref().and_then(|d| d.delay),
            arrival_airport: arrival.as_ref().and_then(|a| a.airport.clone()),
            arrival_iata: arrival.as_ref().and_then(|a| a.iata.clone()),
            arrival_icao: arrival.as_ref().and_then(|a| a.icao.clone()),
            arrival_terminal: arrival.as_ref().and_then(|a| a.terminal.clone()),
            arrival_gate: arrival.as_ref().and_then(|a| a.gate.clone()),
            arrival_baggage: arrival.as_ref().and_then(|a| a.baggage.clone()),
            arrival_scheduled: arrival
                .as_ref()
                .and_then(|a| parse_aviationstack_datetime(a.scheduled.as_deref())),
            arrival_estimated: arrival
                .as_ref()
                .and_then(|a| parse_aviationstack_datetime(a.estimated.as_deref())),
            arrival_actual: arrival
                .as_ref()
                .and_then(|a| parse_aviationstack_datetime(a.actual.as_deref())),
            arrival_delay_minutes: arrival.as_ref().and_then(|a| a.delay),
            aircraft_registration: aircraft.as_ref().and_then(|a| a.registration.clone()),
            aircraft_icao24: aircraft.as_ref().and_then(|a| a.icao24.clone()),
            aircraft_icao: aircraft.as_ref().and_then(|a| a.icao.clone()),
        }
    }
}

fn parse_aviationstack_datetime(value: Option<&str>) -> Option<DateTime<Utc>> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearby_aircraft_deserializes_signal_fields() {
        let ac: NearbyAircraft = serde_json::from_value(serde_json::json!({
            "hex": "3c6589",
            "rssi": -11.5,
            "seen_pos": 0.7
        }))
        .unwrap();
        assert_eq!(ac.rssi, Some(-11.5));
        assert_eq!(ac.seen_pos, Some(0.7));
    }

    #[test]
    fn nearby_aircraft_signal_fields_default_to_none_when_absent() {
        let ac: NearbyAircraft =
            serde_json::from_value(serde_json::json!({ "hex": "3c6589" })).unwrap();
        assert_eq!(ac.rssi, None);
        assert_eq!(ac.seen_pos, None);
    }
}
