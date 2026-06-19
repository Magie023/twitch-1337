//! adsbdb upstream: flight-route lookup, airline ICAO lookup, and the
//! IATA→ICAO callsign resolution that falls back to the airline API.

use std::time::Duration;

use eyre::{Result, WrapErr as _};
use tracing::{debug, warn};

use crate::aviation::location::{airline_table, is_iata_flight_number};
use crate::aviation::types::{AdsbDbAirlineResponse, AdsbDbResponse, FlightRoute};

use super::AviationClient;

const AIRLINE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

impl AviationClient {
    pub async fn get_flight_route(&self, callsign: &str) -> Result<Option<FlightRoute>> {
        let url = format!("{}/callsign/{callsign}", self.adsbdb_base_url);
        debug!(callsign = %callsign, "Fetching flight route from adsbdb");

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsbdb")?;

        let status = resp.status();
        if status.is_client_error() {
            debug!(
                provider = "adsbdb",
                endpoint_kind = "flight_route",
                status = status.as_u16(),
                "adsbdb returned client response"
            );
            return Ok(None);
        }
        if !status.is_success() {
            return Ok(None);
        }

        let body: AdsbDbResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb response")?;

        Ok(body.response.flightroute)
    }

    /// Resolve a potential IATA flight number to an ICAO callsign.
    pub async fn resolve_callsign(&self, input: &str) -> String {
        if !is_iata_flight_number(input) {
            return input.to_string();
        }

        let (airline_iata, flight_num) = input.split_at(2);

        // Try static CSV lookup first
        if let Some(&icao) = airline_table().get(airline_iata) {
            debug!(iata = %airline_iata, icao = %icao, "Resolved airline code via CSV");
            return format!("{icao}{flight_num}");
        }

        // Fallback: query adsbdb airline API
        debug!(iata = %airline_iata, "Airline not in CSV, trying adsbdb API");
        match tokio::time::timeout(
            AIRLINE_LOOKUP_TIMEOUT,
            self.lookup_airline_icao(airline_iata),
        )
        .await
        {
            Ok(Ok(Some(icao))) => {
                warn!(
                    iata = %airline_iata,
                    icao = %icao,
                    "Resolved airline via adsbdb API — consider adding to airlines.csv"
                );
                format!("{icao}{flight_num}")
            }
            Ok(Ok(None)) => {
                debug!(iata = %airline_iata, "Airline not found in adsbdb");
                input.to_string()
            }
            Ok(Err(e)) => {
                warn!(error = ?e, iata = %airline_iata, "adsbdb airline lookup failed");
                input.to_string()
            }
            Err(_) => {
                warn!(iata = %airline_iata, "adsbdb airline lookup timed out");
                input.to_string()
            }
        }
    }

    /// Query adsbdb for an airline's ICAO code by IATA code.
    async fn lookup_airline_icao(&self, iata: &str) -> Result<Option<String>> {
        let url = format!("{}/airline/{iata}", self.adsbdb_base_url);
        debug!(url = %url, "Fetching airline from adsbdb");

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsbdb")?;

        let status = resp.status();
        if status.is_client_error() {
            debug!(
                provider = "adsbdb",
                endpoint_kind = "airline",
                status = status.as_u16(),
                "adsbdb airline lookup returned client response"
            );
            return Ok(None);
        }
        if !status.is_success() {
            return Ok(None);
        }

        let body: AdsbDbAirlineResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb airline response")?;

        Ok(body.response.into_iter().next().map(|a| a.icao))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn aviation_client() -> AviationClient {
        crate::install_crypto_provider();
        AviationClient::new().unwrap()
    }

    #[tokio::test]
    async fn resolve_callsign_translates_iata() {
        let client = aviation_client();
        assert_eq!(client.resolve_callsign("TP247").await, "TAP247");
        assert_eq!(client.resolve_callsign("LH5765").await, "DLH5765");
    }

    #[tokio::test]
    async fn resolve_callsign_passes_through_icao() {
        let client = aviation_client();
        assert_eq!(client.resolve_callsign("TAP247").await, "TAP247");
        assert_eq!(client.resolve_callsign("DLH5765").await, "DLH5765");
    }

    #[tokio::test]
    async fn resolve_callsign_passes_through_hex() {
        let client = aviation_client();
        assert_eq!(client.resolve_callsign("4CA87D").await, "4CA87D");
    }

    #[tokio::test]
    async fn adsbdb_route_4xx_is_quiet_miss() {
        crate::install_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/callsign/DLH1234"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden secret body"))
            .mount(&server)
            .await;

        let client = AviationClient::new_with_adsb_aggregators(
            Vec::new(),
            server.uri(),
            "http://nominatim.test".to_string(),
            reqwest::Client::new(),
        );

        let route = client
            .get_flight_route("DLH1234")
            .await
            .expect("4xx is a sanitized provider outcome");
        assert!(route.is_none());
    }
}
