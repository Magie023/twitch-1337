//! Aviationstack flight-metadata enrichment: the per-query HTTP call, the
//! ordered IATA→ICAO query plan, and the runtime config carrying the key.

use std::time::Duration;

use eyre::{Result, WrapErr as _};
use secrecy::{ExposeSecret as _, SecretString};
use tracing::{debug, warn};

use crate::aviation::location::{is_iata_flight_number, is_icao_flight_number};
use crate::aviation::tracker::FlightIdentifier;
use crate::aviation::types::{AviationstackFlightMetadata, AviationstackFlightsResponse};

use super::AviationClient;

/// Aviation-crate-internal runtime config for the Aviationstack HTTP enrichment
/// endpoint. `api_key` comes from config.toml bootstrap; `base_url` and
/// `timeout_secs` come from the dashboard settings store.
#[derive(Clone)]
pub(crate) struct AviationstackConfig {
    pub(crate) api_key: SecretString,
    pub(crate) base_url: String,
    pub(crate) timeout_secs: u64,
}

impl AviationClient {
    pub async fn get_aviationstack_flight_metadata(
        &self,
        identifier: &FlightIdentifier,
        callsign: Option<&str>,
    ) -> Result<Option<AviationstackFlightMetadata>> {
        let Some(config) = &self.aviationstack else {
            return Ok(None);
        };
        let queries = aviationstack_queries(identifier, callsign);
        if queries.is_empty() {
            debug!(identifier = %identifier, "Skipping aviationstack lookup: no callsign query");
            return Ok(None);
        }

        // Tried in order: a marketing IATA number (e.g. DE1513) may be missing
        // from aviationstack even when the resolved operating ICAO callsign
        // (CFG1513) is indexed, so `aviationstack_queries` appends the ICAO form.
        // A transient error on a non-final query must not block the fallback;
        // only the last query's error propagates.
        let last = queries.len() - 1;
        for (i, (query_key, query_value)) in queries.iter().enumerate() {
            match self
                .aviationstack_query_one(config, query_key, query_value)
                .await
            {
                Ok(Some(metadata)) => return Ok(Some(metadata)),
                Ok(None) => {}
                Err(_) if i < last => {
                    warn!(
                        query_key,
                        "aviationstack query failed; trying fallback query"
                    );
                }
                Err(e) => return Err(e),
            }
        }

        Ok(None)
    }

    async fn aviationstack_query_one(
        &self,
        config: &AviationstackConfig,
        query_key: &str,
        query_value: &str,
    ) -> Result<Option<AviationstackFlightMetadata>> {
        let url = format!("{}/flights", config.base_url.trim_end_matches('/'));
        debug!(
            query_key,
            query_value = %query_value,
            "Fetching flight metadata from aviationstack"
        );

        let timeout = Duration::from_secs(config.timeout_secs);
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("access_key", config.api_key.expose_secret()),
                (query_key, query_value),
                ("limit", "1"),
            ])
            .timeout(timeout)
            .send()
            .await
            .wrap_err("Failed to send request to aviationstack")?;

        let status = resp.status();
        if status.is_client_error() {
            let code = status.as_u16();
            if aviationstack_4xx_is_actionable(code) {
                warn!(
                    provider = "aviationstack",
                    endpoint_kind = "flight_metadata",
                    status = code,
                    query_key,
                    "aviationstack refused request (auth/quota); enrichment degraded"
                );
            } else {
                debug!(
                    provider = "aviationstack",
                    endpoint_kind = "flight_metadata",
                    status = code,
                    query_key,
                    "aviationstack returned client response"
                );
            }
            return Ok(None);
        }
        if !status.is_success() {
            return Err(eyre::eyre!("aviationstack returned {status}"));
        }

        let resp: AviationstackFlightsResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse aviationstack response")?;

        Ok(resp
            .data
            .into_iter()
            .next()
            .map(AviationstackFlightMetadata::from))
    }
}

/// Whether an aviationstack 4xx is operationally actionable (bad key, plan
/// limit, quota) and so warrants a `warn` rather than a quiet `debug`. Other
/// 4xx (e.g. 404 no-match) are routine and stay quiet.
fn aviationstack_4xx_is_actionable(status: u16) -> bool {
    matches!(status, 401 | 403 | 429)
}

/// Ordered aviationstack `/flights` queries to try for an identifier.
///
/// A marketing IATA number (e.g. DE1513) may be missing from aviationstack even
/// when the resolved operating ICAO callsign (CFG1513) is indexed, so an IATA
/// query is followed by the resolved ICAO as a fallback. Empty when there is
/// nothing to query.
fn aviationstack_queries(
    identifier: &FlightIdentifier,
    callsign: Option<&str>,
) -> Vec<(&'static str, String)> {
    let candidate = match identifier {
        FlightIdentifier::Callsign(value) => value.as_str(),
        FlightIdentifier::Hex(_) => match callsign {
            Some(callsign) => callsign,
            None => return Vec::new(),
        },
    }
    .trim();

    if candidate.is_empty() {
        return Vec::new();
    }

    let candidate = candidate.to_uppercase();
    if is_iata_flight_number(&candidate) {
        let mut queries = vec![("flight_iata", candidate.clone())];
        if let Some(icao) = callsign.map(str::trim).filter(|c| !c.is_empty()) {
            let icao = icao.to_uppercase();
            if is_icao_flight_number(&icao) && icao != candidate {
                queries.push(("flight_icao", icao));
            }
        }
        queries
    } else if is_icao_flight_number(&candidate)
        || !matches!(identifier, FlightIdentifier::Hex(_))
        || callsign.is_some()
    {
        vec![("flight_icao", candidate)]
    } else {
        Vec::new()
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

    #[test]
    fn aviationstack_query_detects_iata_and_icao_flight_numbers() {
        assert_eq!(
            aviationstack_queries(&FlightIdentifier::Callsign("LH1929".to_string()), None),
            vec![("flight_iata", "LH1929".to_string())]
        );
        assert_eq!(
            aviationstack_queries(&FlightIdentifier::Callsign("DLH1929".to_string()), None),
            vec![("flight_icao", "DLH1929".to_string())]
        );
    }

    #[test]
    fn aviationstack_query_appends_resolved_icao_fallback() {
        assert_eq!(
            aviationstack_queries(
                &FlightIdentifier::Callsign("DE1513".to_string()),
                Some("CFG1513")
            ),
            vec![
                ("flight_iata", "DE1513".to_string()),
                ("flight_icao", "CFG1513".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn aviationstack_4xx_is_quiet_miss_without_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/flights"))
            .respond_with(ResponseTemplate::new(404).set_body_string("no matching flights"))
            .mount(&server)
            .await;

        let client = aviation_client().with_aviationstack(
            Some(SecretString::new("test-key".into())),
            server.uri(),
            5,
        );

        let metadata = client
            .get_aviationstack_flight_metadata(
                &FlightIdentifier::Callsign("DLH1234".to_string()),
                Some("DLH1234"),
            )
            .await
            .expect("4xx is a sanitized provider outcome");
        assert!(metadata.is_none());
    }

    #[test]
    fn aviationstack_auth_and_quota_4xx_are_actionable() {
        // Auth / plan / quota refusals warrant an operator-visible warn.
        assert!(aviationstack_4xx_is_actionable(401));
        assert!(aviationstack_4xx_is_actionable(403));
        assert!(aviationstack_4xx_is_actionable(429));
        // Routine "no match" / bad-request responses stay quiet.
        assert!(!aviationstack_4xx_is_actionable(404));
        assert!(!aviationstack_4xx_is_actionable(400));
        assert!(!aviationstack_4xx_is_actionable(422));
    }
}
