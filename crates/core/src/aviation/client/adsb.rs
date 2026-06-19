//! adsb.lol position aggregation: the readsb-v2/adsb.fi aggregator set, the
//! per-backend health/parking state machine, the merge of per-backend aircraft
//! lists into a coverage superset, and the `get_aircraft_by_*` entry points.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eyre::Result;
use tracing::{debug, error, warn};

use crate::aviation::types::{AdsbAircraftResponse, NearbyAircraft};

use super::AviationClient;

/// Freshness equivalence window, in seconds. Position fixes whose `seen_pos`
/// falls in the same bucket are treated as equally fresh, so `rssi` decides.
const FRESH_WINDOW: f64 = 5.0;

/// Consecutive non-timeout retryable errors that trigger a cooldown park.
const ERROR_STREAK_PARK: u8 = 3;
/// Minimum spacing for aggregate ADS-B outage error logs.
const ADSB_AGGREGATE_OUTAGE_LOG_COOLDOWN: Duration = Duration::from_secs(60);

/// Per-backend health, index-aligned with `AviationClient::adsb_aggregators`.
pub(super) struct BackendHealth {
    /// Backend is available when `parked_until <= now`.
    pub(super) parked_until: Instant,
    /// Consecutive non-timeout retryable errors.
    pub(super) error_streak: u8,
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
pub(in crate::aviation) struct AdsbAggregator {
    name: &'static str,
    base_url: String,
    path_style: AdsbPathStyle,
}

impl AdsbAggregator {
    pub(in crate::aviation) fn readsb_v2(name: &'static str, base_url: String) -> Self {
        Self {
            name,
            base_url,
            path_style: AdsbPathStyle::ReadsbV2,
        }
    }

    pub(in crate::aviation) fn adsb_fi(base_url: String) -> Self {
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
enum AdsbEndpoint<'a> {
    Point { lat: f64, lon: f64, radius_nm: u16 },
    Hex(&'a str),
    Callsign(&'a str),
}

impl AdsbEndpoint<'_> {
    fn kind(&self) -> &'static str {
        match self {
            Self::Point { .. } => "point",
            Self::Hex(_) => "hex",
            Self::Callsign(_) => "callsign",
        }
    }

    fn url(&self, aggregator: &AdsbAggregator) -> String {
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

#[derive(Debug)]
enum AdsbFetchError {
    /// HTTP 429 — back off this backend (cooldown park).
    RateLimited,
    /// HTTP 403 or another 4xx provider refusal. Body is intentionally omitted.
    ProviderRefused { status: u16 },
    /// Request timed out. Retryable, but NOT counted toward parking.
    Timeout(eyre::Report),
    /// Other retryable failure (5xx, connect-refused, parse error).
    Retryable(eyre::Report),
    /// Non-retryable send error.
    Fatal(eyre::Report),
}

impl AviationClient {
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

    fn parked_backend_count(&self) -> usize {
        let now = Instant::now();
        let health = self.health.lock().expect("health mutex poisoned");
        health.iter().filter(|h| h.parked_until > now).count()
    }

    fn log_aggregate_adsb_outage(
        &self,
        endpoint_kind: &str,
        parked_backends: usize,
        last_provider_outcome: Option<&str>,
    ) {
        let now = Instant::now();
        let mut last_logged = self
            .last_adsb_aggregate_outage_log
            .lock()
            .expect("outage log mutex poisoned");
        if last_logged.is_some_and(|previous| {
            now.duration_since(previous) < ADSB_AGGREGATE_OUTAGE_LOG_COOLDOWN
        }) {
            return;
        }
        *last_logged = Some(now);
        error!(
            endpoint_kind,
            configured_backends = self.adsb_aggregators.len(),
            parked_backends,
            last_provider_outcome,
            "all_backends_unavailable"
        );
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

    async fn fetch_adsb_merged(&self, endpoint: AdsbEndpoint<'_>) -> Result<AdsbAircraftResponse> {
        if self.adsb_aggregators.is_empty() {
            return Err(eyre::eyre!("No ADS-B aggregators configured"));
        }

        let live = self.live_backend_indices();
        if live.is_empty() {
            let configured_backends = self.adsb_aggregators.len();
            let parked_backends = self.parked_backend_count();
            self.log_aggregate_adsb_outage(endpoint.kind(), parked_backends, Some("all_parked"));
            return Err(eyre::eyre!(
                "all_backends_unavailable endpoint_kind={} configured_backends={} parked_backends={} last_provider_outcome=all_parked",
                endpoint.kind(),
                configured_backends,
                parked_backends
            ));
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
        let mut last_provider_outcome: Option<String> = None;

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
                    last_provider_outcome = Some(format!("{name}:rate_limited:429"));
                    last_error = Some(eyre::eyre!("{name} returned 429"));
                }
                Err(AdsbFetchError::ProviderRefused { status }) if status == 403 => {
                    warn!(
                        provider = name,
                        status, "ADS-B aggregator refused request; parking"
                    );
                    self.park_backend(idx);
                    last_provider_outcome = Some(format!("{name}:forbidden_parked:403"));
                    last_error = Some(eyre::eyre!("{name} returned {status}"));
                }
                Err(AdsbFetchError::ProviderRefused { status }) => {
                    debug!(
                        provider = name,
                        status, "ADS-B aggregator returned client response"
                    );
                    // Non-403 4xx still feeds the error streak so a backend
                    // persistently refusing requests eventually parks.
                    self.record_retryable_error(idx);
                    last_provider_outcome = Some(format!("{name}:client_response:{status}"));
                    last_error = Some(eyre::eyre!("{name} returned {status}"));
                }
                Err(AdsbFetchError::Timeout(error)) => {
                    warn!(provider = name, error = ?error, "ADS-B aggregator timed out");
                    last_provider_outcome = Some(format!("{name}:timeout"));
                    last_error = Some(error);
                }
                Err(AdsbFetchError::Retryable(error)) => {
                    warn!(provider = name, error = ?error, "ADS-B aggregator failed");
                    self.record_retryable_error(idx);
                    last_provider_outcome = Some(format!("{name}:retryable_error"));
                    last_error = Some(error);
                }
                Err(AdsbFetchError::Fatal(error)) => {
                    warn!(provider = name, error = ?error, "ADS-B aggregator fatal; continuing");
                    last_provider_outcome = Some(format!("{name}:fatal_error"));
                    last_error = Some(error);
                }
            }
        }

        if per_backend.is_empty() {
            let parked_backends = self.parked_backend_count();
            self.log_aggregate_adsb_outage(
                endpoint.kind(),
                parked_backends,
                last_provider_outcome.as_deref(),
            );
            return Err(last_error
                .unwrap_or_else(|| eyre::eyre!("All ADS-B aggregators failed"))
                .wrap_err(format!(
                    "all_backends_unavailable endpoint_kind={} configured_backends={} parked_backends={} last_provider_outcome={}",
                    endpoint.kind(),
                    self.adsb_aggregators.len(),
                    parked_backends,
                    last_provider_outcome.as_deref().unwrap_or("unknown")
                )));
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
        if status.is_client_error() {
            return Err(AdsbFetchError::ProviderRefused {
                status: status.as_u16(),
            });
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

    pub(in crate::aviation) async fn get_aircraft_nearby(
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_client_with_aggregators(adsb_aggregators: Vec<AdsbAggregator>) -> AviationClient {
        crate::install_crypto_provider();
        AviationClient::new_with_adsb_aggregators(
            adsb_aggregators,
            "http://adsbdb.test".to_string(),
            "http://nominatim.test".to_string(),
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("test client builds"),
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
        let mut hexless_a = ac(None, Some(1.0), Some(-10.0));
        hexless_a.flight = Some("HEXLESS_A".to_owned());
        let mut hexless_b = ac(None, Some(1.0), Some(-10.0));
        hexless_b.flight = Some("HEXLESS_B".to_owned());

        let merged = merge_aircraft(vec![vec![
            ac(Some("aaa111"), Some(1.0), Some(-10.0)),
            hexless_a,
            hexless_b,
        ]]);

        let flights: Vec<_> = merged.iter().filter_map(|a| a.flight.as_deref()).collect();
        assert_eq!(merged.len(), 3);
        assert!(flights.contains(&"HEXLESS_A"));
        assert!(flights.contains(&"HEXLESS_B"));
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

    #[tokio::test]
    async fn fetch_once_classifies_403_as_provider_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden secret body"))
            .mount(&server)
            .await;

        let client =
            test_client_with_aggregators(vec![AdsbAggregator::readsb_v2("primary", server.uri())]);
        let aggregator = AdsbAggregator::readsb_v2("primary", server.uri());
        let url = format!("{}/hex/3c6589", server.uri());

        let err = client
            .fetch_adsb_response_once(&aggregator, &url)
            .await
            .expect_err("403 is an error");
        assert!(matches!(
            err,
            AdsbFetchError::ProviderRefused { status: 403 }
        ));
        assert!(!format!("{err:?}").contains("secret body"));
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
    async fn merged_query_streak_parks_backend_on_persistent_4xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client =
            test_client_with_aggregators(vec![AdsbAggregator::readsb_v2("only", server.uri())])
                .with_adsb_cooldown(Duration::from_secs(60));

        // A non-403 4xx must feed the error streak like any other failure, so a
        // backend that keeps returning it parks after ERROR_STREAK_PARK strikes
        // and is no longer queried.
        for _ in 0..(ERROR_STREAK_PARK + 1) {
            let _ = client.get_aircraft_by_hex("3c6589").await;
        }

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            ERROR_STREAK_PARK as usize,
            "non-403 4xx parks the backend after the streak threshold"
        );
    }

    #[tokio::test]
    async fn merged_query_streak_parks_backend_on_retryable_5xx() {
        let (retrying, healthy) = tokio::join!(MockServer::start(), MockServer::start());
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&retrying)
            .await;
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(200).set_body_json(aircraft_response("OK")))
            .mount(&healthy)
            .await;

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("retrying", retrying.uri()),
            AdsbAggregator::readsb_v2("healthy", healthy.uri()),
        ])
        .with_adsb_cooldown(Duration::from_secs(60));

        for _ in 0..(ERROR_STREAK_PARK + 1) {
            let aircraft = client.get_aircraft_by_hex("3c6589").await.unwrap().unwrap();
            assert_eq!(aircraft.flight.as_deref(), Some("OK"));
        }

        assert_eq!(
            retrying.received_requests().await.unwrap().len(),
            ERROR_STREAK_PARK as usize,
            "retryable 5xx parks the failing backend after the streak threshold"
        );
        assert_eq!(
            healthy.received_requests().await.unwrap().len(),
            ERROR_STREAK_PARK as usize + 1,
            "healthy backend remains live after its peer is parked"
        );
    }

    #[tokio::test]
    async fn merged_query_parks_backend_on_403() {
        let (a, b) = tokio::join!(MockServer::start(), MockServer::start());
        Mock::given(method("GET"))
            .and(path("/hex/3c6589"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden secret body"))
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

    #[tokio::test]
    async fn merged_query_all_backends_parked_reports_aggregate_outage() {
        let (a, b) = tokio::join!(MockServer::start(), MockServer::start());
        for server in [&a, &b] {
            Mock::given(method("GET"))
                .and(path("/hex/3c6589"))
                .respond_with(ResponseTemplate::new(403))
                .mount(server)
                .await;
        }

        let client = test_client_with_aggregators(vec![
            AdsbAggregator::readsb_v2("a", a.uri()),
            AdsbAggregator::readsb_v2("b", b.uri()),
        ])
        .with_adsb_cooldown(Duration::from_secs(60));

        let first = client.get_aircraft_by_hex("3c6589").await;
        assert!(first.is_err());
        let second = client.get_aircraft_by_hex("3c6589").await.unwrap_err();
        let message = format!("{second:#}");
        assert!(message.contains("all_backends_unavailable"), "{message}");
        assert!(message.contains("configured_backends=2"), "{message}");
        assert!(!message.contains("forbidden secret body"));
    }
}
