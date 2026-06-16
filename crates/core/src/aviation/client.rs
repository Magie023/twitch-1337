//! HTTP client for ADS-B aggregators (adsb.lol + fallbacks), adsbdb,
//! Nominatim, and Aviationstack.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eyre::{Result, WrapErr as _};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::util::APP_USER_AGENT;

use super::location::{
    ResolvedLocation, airline_table, is_iata_flight_number, is_icao_flight_number,
};
use super::tracker::FlightIdentifier;
use super::types::{
    AdsbAircraftResponse, AdsbDbAirlineResponse, AdsbDbResponse, AviationstackFlightMetadata,
    AviationstackFlightsResponse, FlightRoute, NearbyAircraft,
};

/// Aviation-crate-internal runtime config for the Aviationstack HTTP enrichment
/// endpoint. `api_key` comes from config.toml bootstrap; `base_url` and
/// `timeout_secs` come from the dashboard settings store.
#[derive(Clone)]
pub(crate) struct AviationstackConfig {
    pub(crate) api_key: SecretString,
    pub(crate) base_url: String,
    pub(crate) timeout_secs: u64,
}

const ADSBDB_BASE_URL: &str = "https://api.adsbdb.com/v0";
const ADSBLOL_BASE_URL: &str = "https://api.adsb.lol/v2";
const AIRPLANES_LIVE_BASE_URL: &str = "https://api.airplanes.live/v2";
const ADSBFI_BASE_URL: &str = "https://opendata.adsb.fi/api/v2";
const ADSBONE_BASE_URL: &str = "https://api.adsb.one/v2";
const NOMINATIM_BASE_URL: &str = "https://nominatim.openstreetmap.org";
const AIRLINE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const ADSB_AGGREGATOR_TIMEOUT: Duration = Duration::from_secs(2);

/// Freshness equivalence window, in seconds. Position fixes whose `seen_pos`
/// falls in the same bucket are treated as equally fresh, so `rssi` decides.
const FRESH_WINDOW: f64 = 5.0;

/// Cooldown duration a backend is parked for after a 429 or error streak.
const ADSB_COOLDOWN: Duration = Duration::from_secs(60);
/// Consecutive non-timeout retryable errors that trigger a cooldown park.
const ERROR_STREAK_PARK: u8 = 3;

/// Per-backend health, index-aligned with `AviationClient::adsb_aggregators`.
struct BackendHealth {
    /// Backend is available when `parked_until <= now`.
    parked_until: Instant,
    /// Consecutive non-timeout retryable errors.
    error_streak: u8,
}

/// Quantize `seen_pos` into a freshness bucket (lower = fresher). Missing,
/// negative, or non-finite values sort last — a bare `as u64` cast would
/// saturate negative/NaN to 0 (the freshest bucket) and let garbage win.
fn freshness_bucket(seen_pos: Option<f64>) -> u64 {
    match seen_pos {
        Some(s) if s.is_finite() && s >= 0.0 => (s / FRESH_WINDOW).floor() as u64,
        _ => u64::MAX,
    }
}

/// Pick the most-reliable copy of one aircraft: freshest `seen_pos` bucket,
/// then highest `rssi`. On a full tie, keeps the first element (callers push
/// in aggregator priority order, so the highest-priority backend wins).
fn pick_winner(group: Vec<NearbyAircraft>) -> NearbyAircraft {
    group
        .into_iter()
        .min_by(|a, b| {
            freshness_bucket(a.seen_pos)
                .cmp(&freshness_bucket(b.seen_pos))
                .then_with(|| {
                    b.rssi
                        .unwrap_or(f64::MIN)
                        .total_cmp(&a.rssi.unwrap_or(f64::MIN))
                })
        })
        .expect("group is never empty")
}

/// Union the per-backend aircraft lists into a coverage superset, deduping by
/// hex (case-insensitive key; the stored hex is left untouched) and keeping the
/// best copy of each. Hex-less aircraft are passed through unmerged. Output
/// order follows first-seen order across backends.
fn merge_aircraft(per_backend: Vec<Vec<NearbyAircraft>>) -> Vec<NearbyAircraft> {
    let mut groups: HashMap<String, Vec<NearbyAircraft>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut hexless: Vec<NearbyAircraft> = Vec::new();

    for backend in per_backend {
        for aircraft in backend {
            match aircraft.hex.as_deref() {
                Some(hex) if !hex.is_empty() => {
                    let key = hex.to_ascii_lowercase();
                    if !groups.contains_key(&key) {
                        order.push(key.clone());
                    }
                    groups.entry(key).or_default().push(aircraft);
                }
                _ => hexless.push(aircraft),
            }
        }
    }

    let mut merged: Vec<NearbyAircraft> = order
        .into_iter()
        .map(|key| pick_winner(groups.remove(&key).expect("key came from groups")))
        .collect();
    merged.extend(hexless);
    merged
}

#[derive(Clone)]
pub(super) struct AdsbAggregator {
    name: &'static str,
    base_url: String,
    path_style: AdsbPathStyle,
}

impl AdsbAggregator {
    pub(super) fn readsb_v2(name: &'static str, base_url: String) -> Self {
        Self {
            name,
            base_url,
            path_style: AdsbPathStyle::ReadsbV2,
        }
    }

    pub(super) fn adsb_fi(base_url: String) -> Self {
        Self {
            name: "adsb.fi",
            base_url,
            path_style: AdsbPathStyle::AdsbFiOpenData,
        }
    }
}

#[derive(Clone, Copy)]
enum AdsbPathStyle {
    ReadsbV2,
    AdsbFiOpenData,
}

#[derive(Clone, Copy)]
pub(super) enum AdsbEndpoint<'a> {
    Point { lat: f64, lon: f64, radius_nm: u16 },
    Hex(&'a str),
    Callsign(&'a str),
}

impl AdsbEndpoint<'_> {
    pub(super) fn url(&self, aggregator: &AdsbAggregator) -> String {
        let base = aggregator.base_url.trim_end_matches('/');
        match (aggregator.path_style, self) {
            (
                AdsbPathStyle::ReadsbV2,
                Self::Point {
                    lat,
                    lon,
                    radius_nm,
                },
            ) => format!("{base}/point/{lat}/{lon}/{radius_nm}"),
            (AdsbPathStyle::ReadsbV2, Self::Hex(hex)) => format!("{base}/hex/{hex}"),
            (AdsbPathStyle::ReadsbV2, Self::Callsign(callsign)) => {
                format!("{base}/callsign/{callsign}")
            }
            (
                AdsbPathStyle::AdsbFiOpenData,
                Self::Point {
                    lat,
                    lon,
                    radius_nm,
                },
            ) => format!("{base}/lat/{lat}/lon/{lon}/dist/{radius_nm}"),
            (AdsbPathStyle::AdsbFiOpenData, Self::Hex(hex)) => {
                format!("{base}/hex/{hex}")
            }
            (AdsbPathStyle::AdsbFiOpenData, Self::Callsign(callsign)) => {
                format!("{base}/callsign/{callsign}")
            }
        }
    }
}

enum AdsbFetchError {
    /// HTTP 429 — back off this backend (cooldown park).
    RateLimited,
    /// Request timed out. Retryable, but NOT counted toward parking.
    Timeout(eyre::Report),
    /// Other retryable failure (5xx, connect-refused, parse error).
    Retryable(eyre::Report),
    /// Non-retryable send error.
    Fatal(eyre::Report),
}

#[derive(Debug, Deserialize)]
struct NominatimResult {
    lat: String,
    lon: String,
    display_name: String,
}

#[derive(Clone)]
pub struct AviationClient {
    http: reqwest::Client,
    adsb_aggregators: Vec<AdsbAggregator>,
    adsb_aggregator_timeout: Duration,
    adsbdb_base_url: String,
    nominatim_base_url: String,
    aviationstack: Option<AviationstackConfig>,
    adsb_cooldown: Duration,
    health: Arc<Mutex<Vec<BackendHealth>>>,
}

impl AviationClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .wrap_err("Failed to build aviation HTTP client")?;
        Ok(Self::new_with_adsb_aggregators(
            Self::default_adsb_aggregators(),
            ADSBDB_BASE_URL.to_owned(),
            NOMINATIM_BASE_URL.to_owned(),
            http,
        ))
    }

    pub fn new_with_base_url(
        adsb_base_url: String,
        adsbdb_base_url: String,
        nominatim_base_url: String,
        http_client: reqwest::Client,
    ) -> Self {
        Self::new_with_adsb_aggregators(
            vec![AdsbAggregator::readsb_v2("adsb.lol", adsb_base_url)],
            adsbdb_base_url,
            nominatim_base_url,
            http_client,
        )
    }

    pub(super) fn new_with_adsb_aggregators(
        adsb_aggregators: Vec<AdsbAggregator>,
        adsbdb_base_url: String,
        nominatim_base_url: String,
        http_client: reqwest::Client,
    ) -> Self {
        let now = Instant::now();
        let health = adsb_aggregators
            .iter()
            .map(|_| BackendHealth {
                parked_until: now,
                error_streak: 0,
            })
            .collect();
        Self {
            http: http_client,
            adsb_aggregators,
            adsb_aggregator_timeout: ADSB_AGGREGATOR_TIMEOUT,
            adsbdb_base_url,
            nominatim_base_url,
            aviationstack: None,
            adsb_cooldown: ADSB_COOLDOWN,
            health: Arc::new(Mutex::new(health)),
        }
    }

    fn default_adsb_aggregators() -> Vec<AdsbAggregator> {
        vec![
            AdsbAggregator::readsb_v2("adsb.lol", ADSBLOL_BASE_URL.to_owned()),
            AdsbAggregator::readsb_v2("airplanes.live", AIRPLANES_LIVE_BASE_URL.to_owned()),
            AdsbAggregator::adsb_fi(ADSBFI_BASE_URL.to_owned()),
            AdsbAggregator::readsb_v2("ADSB.One", ADSBONE_BASE_URL.to_owned()),
        ]
    }

    /// Enable Aviationstack flight-metadata enrichment.
    ///
    /// `api_key` comes from the bootstrap secret in config.toml.
    /// `base_url` and `timeout_secs` come from the dashboard settings store.
    /// Passing `None` for `api_key` disables Aviationstack enrichment.
    pub fn with_aviationstack(
        mut self,
        api_key: Option<SecretString>,
        base_url: String,
        timeout_secs: u64,
    ) -> Self {
        self.aviationstack = api_key.map(|key| AviationstackConfig {
            api_key: key,
            base_url,
            timeout_secs,
        });
        self
    }

    fn live_backend_indices(&self) -> Vec<usize> {
        let now = Instant::now();
        let health = self.health.lock().expect("health mutex poisoned");
        health
            .iter()
            .enumerate()
            .filter(|(_, h)| h.parked_until <= now)
            .map(|(i, _)| i)
            .collect()
    }

    fn park_backend(&self, index: usize) {
        let until = Instant::now() + self.adsb_cooldown;
        let mut health = self.health.lock().expect("health mutex poisoned");
        if let Some(h) = health.get_mut(index) {
            h.parked_until = until;
            h.error_streak = 0;
        }
    }

    fn record_success(&self, index: usize) {
        let mut health = self.health.lock().expect("health mutex poisoned");
        if let Some(h) = health.get_mut(index) {
            h.error_streak = 0;
        }
    }

    /// Count a non-timeout retryable error; park the backend on the
    /// `ERROR_STREAK_PARK`-th consecutive one.
    fn record_retryable_error(&self, index: usize) {
        let until = Instant::now() + self.adsb_cooldown;
        let mut health = self.health.lock().expect("health mutex poisoned");
        if let Some(h) = health.get_mut(index) {
            h.error_streak = h.error_streak.saturating_add(1);
            if h.error_streak >= ERROR_STREAK_PARK {
                h.parked_until = until;
                h.error_streak = 0;
            }
        }
    }

    #[cfg(test)]
    fn with_adsb_aggregator_timeout(mut self, timeout: Duration) -> Self {
        self.adsb_aggregator_timeout = timeout;
        self
    }

    #[cfg(test)]
    fn with_adsb_cooldown(mut self, cooldown: Duration) -> Self {
        self.adsb_cooldown = cooldown;
        self
    }

    pub fn aviationstack_enabled(&self) -> bool {
        self.aviationstack.is_some()
    }

    async fn fetch_adsb_merged(&self, endpoint: AdsbEndpoint<'_>) -> Result<AdsbAircraftResponse> {
        if self.adsb_aggregators.is_empty() {
            return Err(eyre::eyre!("No ADS-B aggregators configured"));
        }

        let live = self.live_backend_indices();
        if live.is_empty() {
            return Err(eyre::eyre!("All ADS-B aggregators parked (rate-limited)"));
        }

        let futures = live.iter().map(|&idx| {
            let aggregator = &self.adsb_aggregators[idx];
            let url = endpoint.url(aggregator);
            async move {
                let result = self.fetch_adsb_response_once(aggregator, &url).await;
                (idx, aggregator.name, result)
            }
        });
        let results = futures_util::future::join_all(futures).await;

        let mut per_backend: Vec<Vec<NearbyAircraft>> = Vec::new();
        let mut last_error: Option<eyre::Report> = None;

        for (idx, name, result) in results {
            match result {
                Ok(resp) => {
                    debug!(
                        provider = name,
                        count = resp.aircraft.len(),
                        "Received aircraft from ADS-B aggregator"
                    );
                    self.record_success(idx);
                    per_backend.push(resp.aircraft);
                }
                Err(AdsbFetchError::RateLimited) => {
                    warn!(
                        provider = name,
                        "ADS-B aggregator rate-limited (429); parking"
                    );
                    self.park_backend(idx);
                    last_error = Some(eyre::eyre!("{name} returned 429"));
                }
                Err(AdsbFetchError::Timeout(error)) => {
                    warn!(provider = name, error = ?error, "ADS-B aggregator timed out");
                    last_error = Some(error);
                }
                Err(AdsbFetchError::Retryable(error)) => {
                    warn!(provider = name, error = ?error, "ADS-B aggregator failed");
                    self.record_retryable_error(idx);
                    last_error = Some(error);
                }
                Err(AdsbFetchError::Fatal(error)) => {
                    warn!(provider = name, error = ?error, "ADS-B aggregator fatal; continuing");
                    last_error = Some(error);
                }
            }
        }

        if per_backend.is_empty() {
            return Err(last_error
                .unwrap_or_else(|| eyre::eyre!("All ADS-B aggregators failed"))
                .wrap_err("All ADS-B aggregators failed"));
        }

        Ok(AdsbAircraftResponse {
            aircraft: merge_aircraft(per_backend),
        })
    }

    async fn fetch_adsb_response_once(
        &self,
        aggregator: &AdsbAggregator,
        url: &str,
    ) -> std::result::Result<AdsbAircraftResponse, AdsbFetchError> {
        let resp = self
            .http
            .get(url)
            .timeout(self.adsb_aggregator_timeout)
            .send()
            .await
            .map_err(|e| {
                let error = eyre::eyre!("Failed to send request to {}: {e}", aggregator.name);
                if e.is_timeout() {
                    AdsbFetchError::Timeout(error)
                } else if e.is_connect() {
                    AdsbFetchError::Retryable(error)
                } else {
                    // Only known-retryable transport failures (timeout, connect) get
                    // retried; all other send errors are treated as fatal.
                    AdsbFetchError::Fatal(error)
                }
            })?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(AdsbFetchError::RateLimited);
        }
        if !status.is_success() {
            return Err(AdsbFetchError::Retryable(eyre::eyre!(
                "{} returned {status}",
                aggregator.name
            )));
        }

        resp.json().await.map_err(|e| {
            AdsbFetchError::Retryable(eyre::eyre!(
                "Failed to parse {} response: {e}",
                aggregator.name
            ))
        })
    }

    pub(super) async fn get_aircraft_nearby(
        &self,
        lat: f64,
        lon: f64,
        radius_nm: u16,
    ) -> Result<Vec<NearbyAircraft>> {
        let resp = self
            .fetch_adsb_merged(AdsbEndpoint::Point {
                lat,
                lon,
                radius_nm,
            })
            .await?;
        Ok(resp.aircraft)
    }

    pub async fn get_aircraft_by_hex(&self, hex: &str) -> Result<Option<NearbyAircraft>> {
        debug!(hex = %hex, "Fetching aircraft by hex from ADS-B aggregators");

        let resp = self.fetch_adsb_merged(AdsbEndpoint::Hex(hex)).await?;

        Ok(resp.aircraft.into_iter().next())
    }

    pub async fn get_aircraft_by_callsign(&self, callsign: &str) -> Result<Option<NearbyAircraft>> {
        debug!(callsign = %callsign, "Fetching aircraft by callsign from ADS-B aggregators");

        let resp = self
            .fetch_adsb_merged(AdsbEndpoint::Callsign(callsign))
            .await?;

        Ok(resp.aircraft.into_iter().next())
    }

    pub async fn get_flight_route(&self, callsign: &str) -> Result<Option<FlightRoute>> {
        let url = format!("{}/callsign/{callsign}", self.adsbdb_base_url);
        debug!(callsign = %callsign, "Fetching flight route from adsbdb");

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsbdb")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let body: AdsbDbResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb response")?;

        Ok(body.response.flightroute)
    }

    pub async fn get_aviationstack_flight_metadata(
        &self,
        identifier: &FlightIdentifier,
        callsign: Option<&str>,
    ) -> Result<Option<AviationstackFlightMetadata>> {
        let Some(config) = &self.aviationstack else {
            return Ok(None);
        };
        let Some((query_key, query_value)) = aviationstack_query(identifier, callsign) else {
            debug!(identifier = %identifier, "Skipping aviationstack lookup: no callsign query");
            return Ok(None);
        };

        let url = format!("{}/flights", config.base_url.trim_end_matches('/'));
        debug!(
            query_key,
            query_value = %query_value,
            "Fetching flight metadata from aviationstack"
        );

        let timeout = Duration::from_secs(config.timeout_secs);
        let resp: AviationstackFlightsResponse = self
            .http
            .get(&url)
            .query(&[
                ("access_key", config.api_key.expose_secret()),
                (query_key, query_value.as_str()),
                ("limit", "1"),
            ])
            .timeout(timeout)
            .send()
            .await
            .wrap_err("Failed to send request to aviationstack")?
            .error_for_status()
            .wrap_err("aviationstack returned error status")?
            .json()
            .await
            .wrap_err("Failed to parse aviationstack response")?;

        Ok(resp
            .data
            .into_iter()
            .next()
            .map(AviationstackFlightMetadata::from))
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

        if !resp.status().is_success() {
            return Ok(None);
        }

        let body: AdsbDbAirlineResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb airline response")?;

        Ok(body.response.into_iter().next().map(|a| a.icao))
    }

    pub(super) async fn geocode_nominatim(&self, query: &str) -> Result<Option<ResolvedLocation>> {
        let url = format!("{}/search", self.nominatim_base_url);
        debug!(query = %query, "Geocoding via Nominatim");

        let resp = self
            .http
            .get(&url)
            .query(&[("q", query), ("format", "json"), ("limit", "1")])
            .send()
            .await
            .wrap_err("Failed to send request to Nominatim")?
            .error_for_status()
            .wrap_err("Nominatim returned error status")?;

        let results: Vec<NominatimResult> = resp
            .json()
            .await
            .wrap_err("Failed to parse Nominatim response")?;

        let Some(first) = results.into_iter().next() else {
            debug!(query = %query, "Nominatim returned no results");
            return Ok(None);
        };

        let lat: f64 = first.lat.parse().wrap_err("Invalid lat from Nominatim")?;
        let lon: f64 = first.lon.parse().wrap_err("Invalid lon from Nominatim")?;

        // Trim display_name to first comma-separated segment
        let display_name = first
            .display_name
            .split(',')
            .next()
            .unwrap_or(&first.display_name)
            .trim()
            .to_string();

        debug!(query = %query, lat = %lat, lon = %lon, display = %display_name, "Nominatim resolved");
        Ok(Some(ResolvedLocation {
            lat,
            lon,
            display_name,
        }))
    }
}

pub(super) fn aviationstack_query(
    identifier: &FlightIdentifier,
    callsign: Option<&str>,
) -> Option<(&'static str, String)> {
    let candidate = match identifier {
        FlightIdentifier::Callsign(value) => value.as_str(),
        FlightIdentifier::Hex(_) => callsign?,
    }
    .trim();

    if candidate.is_empty() {
        return None;
    }

    let candidate = candidate.to_uppercase();
    if is_iata_flight_number(&candidate) {
        Some(("flight_iata", candidate))
    } else if is_icao_flight_number(&candidate)
        || !matches!(identifier, FlightIdentifier::Hex(_))
        || callsign.is_some()
    {
        Some(("flight_icao", candidate))
    } else {
        None
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

    fn test_client_with_aggregators(adsb_aggregators: Vec<AdsbAggregator>) -> AviationClient {
        crate::install_crypto_provider();
        AviationClient::new_with_adsb_aggregators(
            adsb_aggregators,
            "http://adsbdb.test".to_string(),
            "http://nominatim.test".to_string(),
            reqwest::Client::new(),
        )
    }

    fn aircraft_response(flight: &str) -> serde_json::Value {
        aircraft_response_full(flight, None, None)
    }

    fn aircraft_response_full(
        flight: &str,
        seen_pos: Option<f64>,
        rssi: Option<f64>,
    ) -> serde_json::Value {
        serde_json::json!({
            "ac": [{
                "hex": "3c6589",
                "flight": flight,
                "alt_baro": 35000,
                "lat": 50.0,
                "lon": 8.5,
                "seen_pos": seen_pos,
                "rssi": rssi
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })
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

    #[test]
    fn aviationstack_query_detects_iata_and_icao_flight_numbers() {
        assert_eq!(
            aviationstack_query(&FlightIdentifier::Callsign("LH1929".to_string()), None),
            Some(("flight_iata", "LH1929".to_string()))
        );
        assert_eq!(
            aviationstack_query(&FlightIdentifier::Callsign("DLH1929".to_string()), None),
            Some(("flight_icao", "DLH1929".to_string()))
        );
    }

    // Under parallel fan-out both backends are queried; the 5xx one is
    // dropped from the merge and the healthy backend's aircraft is returned.
    #[tokio::test]
    async fn adsb_falls_back_from_5xx_to_next_aggregator() {
        let (primary, backup) = tokio::join!(MockServer::start(), MockServer::start());

        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&primary)
            .await;

        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(200).set_body_json(aircraft_response("DLH1234")))
            .mount(&backup)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("primary", primary.uri()),
            AdsbAggregator::readsb_v2("backup", backup.uri()),
        ]);

        let aircraft = client
            .get_aircraft_by_hex("3c6589")
            .await
            .expect("lookup succeeds through fallback")
            .expect("aircraft returned");

        assert_eq!(aircraft.flight.as_deref(), Some("DLH1234"));
        assert_eq!(primary.received_requests().await.unwrap().len(), 1);
        assert_eq!(backup.received_requests().await.unwrap().len(), 1);
    }

    // Under parallel fan-out both backends are queried concurrently; the
    // slow one times out (not parked) and the fast backend's data is merged.
    #[tokio::test]
    async fn adsb_falls_back_from_timeout_to_next_aggregator() {
        let (primary, backup) = tokio::join!(MockServer::start(), MockServer::start());

        Mock::given(method("GET"))
            .and(path("/callsign/DLH1234"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(500))
                    .set_body_json(aircraft_response("DLH1234")),
            )
            .mount(&primary)
            .await;

        Mock::given(method("GET"))
            .and(path("/callsign/DLH1234"))
            .respond_with(ResponseTemplate::new(200).set_body_json(aircraft_response("DLH1234")))
            .mount(&backup)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("primary", primary.uri()),
            AdsbAggregator::readsb_v2("backup", backup.uri()),
        ])
        .with_adsb_aggregator_timeout(Duration::from_millis(50));

        let aircraft = client
            .get_aircraft_by_callsign("DLH1234")
            .await
            .expect("lookup succeeds through fallback")
            .expect("aircraft returned");

        assert_eq!(aircraft.flight.as_deref(), Some("DLH1234"));
        assert_eq!(primary.received_requests().await.unwrap().len(), 1);
        assert_eq!(backup.received_requests().await.unwrap().len(), 1);
    }

    #[test]
    fn adsb_fi_point_url_uses_open_data_path() {
        let aggregator = AdsbAggregator::adsb_fi("https://opendata.adsb.fi/api/v2/".to_string());
        let url = AdsbEndpoint::Point {
            lat: 1.5,
            lon: 2.25,
            radius_nm: 15,
        }
        .url(&aggregator);

        assert_eq!(
            url,
            "https://opendata.adsb.fi/api/v2/lat/1.5/lon/2.25/dist/15"
        );
    }

    #[test]
    fn adsb_response_accepts_aircraft_alias() {
        let response: AdsbAircraftResponse = serde_json::from_value(serde_json::json!({
            "aircraft": [{
                "hex": "3c6589",
                "flight": "DLH1234",
                "alt_baro": 35000
            }]
        }))
        .unwrap();

        assert_eq!(response.aircraft.len(), 1);
        assert_eq!(response.aircraft[0].flight.as_deref(), Some("DLH1234"));
    }

    fn ac(hex: Option<&str>, seen_pos: Option<f64>, rssi: Option<f64>) -> NearbyAircraft {
        NearbyAircraft {
            hex: hex.map(str::to_owned),
            flight: None,
            r: None,
            t: None,
            alt_baro: None,
            lat: None,
            lon: None,
            gs: None,
            baro_rate: None,
            geom_rate: None,
            squawk: None,
            nav_modes: None,
            rssi,
            seen_pos,
        }
    }

    #[test]
    fn merge_unions_distinct_aircraft() {
        let merged = merge_aircraft(vec![
            vec![ac(Some("aaa111"), Some(1.0), Some(-10.0))],
            vec![ac(Some("bbb222"), Some(1.0), Some(-10.0))],
        ]);
        let hexes: Vec<_> = merged.iter().filter_map(|a| a.hex.clone()).collect();
        assert_eq!(merged.len(), 2);
        assert!(hexes.contains(&"aaa111".to_owned()));
        assert!(hexes.contains(&"bbb222".to_owned()));
    }

    #[test]
    fn merge_keeps_fresher_copy() {
        let merged = merge_aircraft(vec![
            vec![ac(Some("aaa111"), Some(1.0), Some(-30.0))],
            vec![ac(Some("aaa111"), Some(30.0), Some(-5.0))],
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].seen_pos, Some(1.0));
    }

    #[test]
    fn merge_breaks_freshness_tie_by_rssi() {
        let merged = merge_aircraft(vec![
            vec![ac(Some("aaa111"), Some(1.0), Some(-30.0))],
            vec![ac(Some("aaa111"), Some(2.0), Some(-5.0))],
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].rssi, Some(-5.0));
    }

    #[test]
    fn merge_bucket_beats_rssi() {
        let merged = merge_aircraft(vec![
            vec![ac(Some("aaa111"), Some(1.0), Some(-30.0))],
            vec![ac(Some("aaa111"), Some(20.0), Some(-1.0))],
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].seen_pos, Some(1.0));
    }

    #[test]
    fn merge_missing_seen_pos_loses() {
        let merged = merge_aircraft(vec![
            vec![ac(Some("aaa111"), None, Some(-1.0))],
            vec![ac(Some("aaa111"), Some(10.0), Some(-30.0))],
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].seen_pos, Some(10.0));
    }

    #[test]
    fn merge_garbage_seen_pos_loses() {
        for garbage in [-1.0_f64, f64::NAN, f64::INFINITY] {
            let merged = merge_aircraft(vec![
                vec![ac(Some("aaa111"), Some(garbage), Some(-1.0))],
                vec![ac(Some("aaa111"), Some(10.0), Some(-30.0))],
            ]);
            assert_eq!(merged.len(), 1);
            assert_eq!(
                merged[0].seen_pos,
                Some(10.0),
                "garbage {garbage} must lose"
            );
        }
    }

    #[test]
    fn merge_tie_keeps_first_backend() {
        let mut first = ac(Some("aaa111"), Some(1.0), Some(-10.0));
        first.flight = Some("FIRST".to_owned());
        let mut second = ac(Some("aaa111"), Some(1.0), Some(-10.0));
        second.flight = Some("SECOND".to_owned());
        let merged = merge_aircraft(vec![vec![first], vec![second]]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].flight.as_deref(), Some("FIRST"));
    }

    #[test]
    fn merge_dedups_case_insensitively_but_keeps_original_case() {
        let merged = merge_aircraft(vec![
            vec![ac(Some("3C6589"), Some(1.0), Some(-5.0))],
            vec![ac(Some("3c6589"), Some(30.0), Some(-50.0))],
        ]);
        assert_eq!(merged.len(), 1, "different-case hex must dedup");
        assert_eq!(
            merged[0].hex.as_deref(),
            Some("3C6589"),
            "stored hex unchanged"
        );
    }

    #[test]
    fn merge_keeps_hexless_aircraft() {
        let merged = merge_aircraft(vec![vec![
            ac(Some("aaa111"), Some(1.0), Some(-10.0)),
            ac(None, Some(1.0), Some(-10.0)),
        ]]);
        assert_eq!(merged.len(), 2);
    }

    #[tokio::test]
    async fn fetch_once_classifies_429_as_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let client =
            test_client_with_aggregators(vec![AdsbAggregator::readsb_v2("primary", server.uri())]);
        let aggregator = AdsbAggregator::readsb_v2("primary", server.uri());
        let url = format!("{}/hex/3c6589", server.uri());

        let err = client
            .fetch_adsb_response_once(&aggregator, &url)
            .await
            .expect_err("429 is an error");
        assert!(matches!(err, AdsbFetchError::RateLimited));
    }

    #[test]
    fn health_all_available_on_construction() {
        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("a", "http://a.test".to_owned()),
            AdsbAggregator::readsb_v2("b", "http://b.test".to_owned()),
        ]);
        assert_eq!(client.live_backend_indices().len(), 2);
    }

    #[test]
    fn health_parks_and_restores() {
        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("a", "http://a.test".to_owned()),
            AdsbAggregator::readsb_v2("b", "http://b.test".to_owned()),
        ])
        .with_adsb_cooldown(Duration::from_millis(50));

        client.park_backend(0);
        assert_eq!(client.live_backend_indices(), vec![1]);

        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(client.live_backend_indices(), vec![0, 1]);
    }

    #[test]
    fn health_error_streak_parks_on_third() {
        let client = test_client_with_aggregators(vec![AdsbAggregator::readsb_v2(
            "a",
            "http://a.test".to_owned(),
        )])
        .with_adsb_cooldown(Duration::from_secs(60));

        client.record_retryable_error(0);
        client.record_retryable_error(0);
        assert_eq!(
            client.live_backend_indices(),
            vec![0],
            "2 errors: still live"
        );
        client.record_retryable_error(0);
        assert!(
            client.live_backend_indices().is_empty(),
            "3rd error parks the backend"
        );
    }

    #[test]
    fn health_success_resets_error_streak() {
        let client = test_client_with_aggregators(vec![AdsbAggregator::readsb_v2(
            "a",
            "http://a.test".to_owned(),
        )]);
        client.record_retryable_error(0);
        client.record_retryable_error(0);
        client.record_success(0);
        client.record_retryable_error(0);
        client.record_retryable_error(0);
        assert_eq!(
            client.live_backend_indices(),
            vec![0],
            "streak reset by success → 2 fresh errors don't park"
        );
    }

    #[tokio::test]
    async fn merged_query_unions_two_backends() {
        let (a, b) = tokio::join!(MockServer::start(), MockServer::start());
        Mock::given(method("GET"))
            .and(path("/point/50/8/15"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ac": [{ "hex": "aaa111", "lat": 50.0, "lon": 8.0, "seen_pos": 1.0, "rssi": -10.0 }]
            })))
            .mount(&a)
            .await;
        Mock::given(method("GET"))
            .and(path("/point/50/8/15"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ac": [{ "hex": "bbb222", "lat": 50.0, "lon": 8.0, "seen_pos": 1.0, "rssi": -10.0 }]
            })))
            .mount(&b)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("a", a.uri()),
            AdsbAggregator::readsb_v2("b", b.uri()),
        ]);

        let aircraft = client.get_aircraft_nearby(50.0, 8.0, 15).await.unwrap();
        let hexes: Vec<_> = aircraft.iter().filter_map(|a| a.hex.clone()).collect();
        assert_eq!(aircraft.len(), 2);
        assert!(hexes.contains(&"aaa111".to_owned()));
        assert!(hexes.contains(&"bbb222".to_owned()));
    }

    #[tokio::test]
    async fn merged_query_keeps_freshest_duplicate() {
        let (a, b) = tokio::join!(MockServer::start(), MockServer::start());
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(aircraft_response_full(
                    "FRESH",
                    Some(1.0),
                    Some(-30.0),
                )),
            )
            .mount(&a)
            .await;
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(aircraft_response_full(
                    "STALE",
                    Some(30.0),
                    Some(-1.0),
                )),
            )
            .mount(&b)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("a", a.uri()),
            AdsbAggregator::readsb_v2("b", b.uri()),
        ]);

        let aircraft = client.get_aircraft_by_hex("3c6589").await.unwrap().unwrap();
        assert_eq!(aircraft.flight.as_deref(), Some("FRESH"));
    }

    #[tokio::test]
    async fn merged_query_parks_backend_on_429() {
        let (a, b) = tokio::join!(MockServer::start(), MockServer::start());
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&a)
            .await;
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(200).set_body_json(aircraft_response("DLH1")))
            .mount(&b)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("a", a.uri()),
            AdsbAggregator::readsb_v2("b", b.uri()),
        ])
        .with_adsb_cooldown(Duration::from_secs(60));

        let first = client.get_aircraft_by_hex("3c6589").await.unwrap().unwrap();
        assert_eq!(first.flight.as_deref(), Some("DLH1"));
        let _ = client.get_aircraft_by_hex("3c6589").await.unwrap();
        assert_eq!(a.received_requests().await.unwrap().len(), 1, "A parked");
        assert_eq!(b.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn merged_query_timeout_does_not_park() {
        let (slow, fast) = tokio::join!(MockServer::start(), MockServer::start());
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(500))
                    .set_body_json(aircraft_response("SLOW")),
            )
            .mount(&slow)
            .await;
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(200).set_body_json(aircraft_response("FAST")))
            .mount(&fast)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("slow", slow.uri()),
            AdsbAggregator::readsb_v2("fast", fast.uri()),
        ])
        .with_adsb_aggregator_timeout(Duration::from_millis(50));

        for _ in 0..3 {
            let _ = client.get_aircraft_by_hex("3c6589").await.unwrap();
        }
        let _ = client.get_aircraft_by_hex("3c6589").await.unwrap();
        assert_eq!(
            slow.received_requests().await.unwrap().len(),
            4,
            "timeouts must not park the backend"
        );
    }

    #[tokio::test]
    async fn merged_query_all_fail_is_error() {
        let (a, b) = tokio::join!(MockServer::start(), MockServer::start());
        for server in [&a, &b] {
            Mock::given(method("GET"))
                .and(path("/hex/3c6589"))
                .respond_with(ResponseTemplate::new(503))
                .mount(server)
                .await;
        }
        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("a", a.uri()),
            AdsbAggregator::readsb_v2("b", b.uri()),
        ]);
        assert!(client.get_aircraft_by_hex("3c6589").await.is_err());
    }
}
