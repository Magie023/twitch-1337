//! HTTP client for ADS-B aggregators (adsb.lol + fallbacks), adsbdb,
//! Nominatim, and Aviationstack.
//!
//! The single shared `AviationClient` (struct + constructors below) talks to
//! four distinct upstreams, one per submodule:
//! - `adsb`: adsb.lol position aggregation (+ fallback aggregators, backend
//!   health, merge).
//! - `adsbdb`: route/airline lookup + IATA→ICAO callsign resolution.
//! - `nominatim`: free-text geocoding.
//! - `aviationstack`: flight-metadata enrichment.

mod adsb;
mod adsbdb;
mod aviationstack;
mod nominatim;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eyre::{Result, WrapErr as _};
use secrecy::SecretString;

use crate::util::APP_USER_AGENT;

use adsb::{AdsbAggregator, BackendHealth};
use aviationstack::AviationstackConfig;

const ADSBDB_BASE_URL: &str = "https://api.adsbdb.com/v0";
const ADSBLOL_BASE_URL: &str = "https://api.adsb.lol/v2";
const AIRPLANES_LIVE_BASE_URL: &str = "https://api.airplanes.live/v2";
const ADSBFI_BASE_URL: &str = "https://opendata.adsb.fi/api/v2";
const ADSBONE_BASE_URL: &str = "https://api.adsb.one/v2";
const NOMINATIM_BASE_URL: &str = "https://nominatim.openstreetmap.org";
const ADSB_AGGREGATOR_TIMEOUT: Duration = Duration::from_secs(2);

/// Cooldown duration a backend is parked for after a 429 or error streak.
const ADSB_COOLDOWN: Duration = Duration::from_secs(60);

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
    last_adsb_aggregate_outage_log: Arc<Mutex<Option<Instant>>>,
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
            last_adsb_aggregate_outage_log: Arc::new(Mutex::new(None)),
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

    pub fn aviationstack_enabled(&self) -> bool {
        self.aviationstack.is_some()
    }
}
