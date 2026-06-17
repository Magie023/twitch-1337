//! OpenRouter model catalog: id → display name lookup with a 5-minute TTL cache.
//!
//! Non-OpenRouter backends skip the fetch entirely; `{model}` falls back to the
//! raw configured model id.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::settings::ai::{AiBackendKind, AiConnection};
use eyre::{Result, WrapErr as _};
use reqwest::Client;
use tracing::warn;

const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const CACHE_TTL: Duration = Duration::from_secs(300);
/// After a failed fetch with no stale catalog, suppress retries for this long.
const FAILURE_BACKOFF: Duration = Duration::from_secs(60);

struct CachedCatalog {
    fetched_at: Instant,
    by_id: HashMap<String, String>,
}

struct CatalogCache {
    entry: Option<CachedCatalog>,
    /// Suppresses refetch attempts until this instant after a failed refresh.
    fetch_failed_until: Option<Instant>,
}

/// Shared OpenRouter model catalog with TTL caching.
pub struct ModelCatalog {
    http: Client,
    models_url: String,
    cache: Mutex<CatalogCache>,
}

impl ModelCatalog {
    pub fn new(http: Client) -> Self {
        Self {
            http,
            models_url: OPENROUTER_MODELS_URL.into(),
            cache: Mutex::new(CatalogCache {
                entry: None,
                fetch_failed_until: None,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_models_url(http: Client, models_url: String) -> Self {
        Self {
            http,
            models_url,
            cache: Mutex::new(CatalogCache {
                entry: None,
                fetch_failed_until: None,
            }),
        }
    }

    /// `true` when the connection uses OpenRouter (default OpenAI base or
    /// explicit `openrouter.ai` URL).
    pub fn is_openrouter(conn: &AiConnection) -> bool {
        conn.backend == AiBackendKind::OpenAi
            && conn
                .base_url
                .as_ref()
                .is_none_or(|u| u.contains("openrouter.ai"))
    }

    /// Turn an upstream OpenRouter `name` into a dashboard / prompt display label.
    pub fn normalize_name(raw: &str) -> String {
        let trimmed = raw.trim();
        let without_free = trimmed.strip_suffix(" (free)").unwrap_or(trimmed);
        without_free.replace(": ", " ")
    }

    /// Pretty display name for `model_id`, or the raw id on miss / failure.
    pub async fn display_name(&self, conn: &AiConnection, model_id: &str) -> String {
        if !Self::is_openrouter(conn) {
            return model_id.to_owned();
        }
        match self.load_openrouter_catalog().await {
            Ok(map) => map
                .get(model_id)
                .map(|name| Self::normalize_name(name))
                .unwrap_or_else(|| model_id.to_owned()),
            Err(_) => model_id.to_owned(),
        }
    }

    /// All OpenRouter models as `(id, normalized_label)` pairs for the dashboard picker.
    pub async fn openrouter_model_entries(&self) -> Result<Vec<(String, String)>> {
        let map = self.load_openrouter_catalog().await?;
        let mut entries: Vec<_> = map
            .into_iter()
            .map(|(id, name)| (id, Self::normalize_name(&name)))
            .collect();
        entries.sort_by(|a, b| a.1.cmp(&b.1));
        Ok(entries)
    }

    async fn load_openrouter_catalog(&self) -> Result<HashMap<String, String>> {
        if let Some(map) = self.fresh_catalog() {
            return Ok(map);
        }

        if self.in_fetch_backoff() {
            if let Some(map) = self.stale_catalog() {
                return Ok(map);
            }
            return Err(eyre::eyre!("openrouter catalog fetch backed off"));
        }

        match self.fetch_openrouter_catalog().await {
            Ok(map) => {
                let mut guard = self.cache.lock().expect("model catalog cache poisoned");
                guard.fetch_failed_until = None;
                guard.entry = Some(CachedCatalog {
                    fetched_at: Instant::now(),
                    by_id: map.clone(),
                });
                Ok(map)
            }
            Err(e) => {
                let mut guard = self.cache.lock().expect("model catalog cache poisoned");
                guard.fetch_failed_until = Some(Instant::now() + FAILURE_BACKOFF);
                if let Some(stale) = guard.entry.as_ref() {
                    warn!(
                        error = ?e,
                        stale_age_secs = stale.fetched_at.elapsed().as_secs(),
                        "OpenRouter catalog refresh failed; using stale cache"
                    );
                    return Ok(stale.by_id.clone());
                }
                warn!(error = ?e, "OpenRouter catalog fetch failed");
                Err(e)
            }
        }
    }

    fn fresh_catalog(&self) -> Option<HashMap<String, String>> {
        let guard = self.cache.lock().expect("model catalog cache poisoned");
        let entry = guard.entry.as_ref()?;
        if entry.fetched_at.elapsed() > CACHE_TTL {
            return None;
        }
        Some(entry.by_id.clone())
    }

    fn stale_catalog(&self) -> Option<HashMap<String, String>> {
        let guard = self.cache.lock().expect("model catalog cache poisoned");
        guard.entry.as_ref().map(|entry| entry.by_id.clone())
    }

    fn in_fetch_backoff(&self) -> bool {
        let guard = self.cache.lock().expect("model catalog cache poisoned");
        guard
            .fetch_failed_until
            .is_some_and(|until| Instant::now() < until)
    }

    async fn fetch_openrouter_catalog(&self) -> Result<HashMap<String, String>> {
        let resp: serde_json::Value = self
            .http
            .get(&self.models_url)
            .send()
            .await
            .wrap_err("openrouter models request failed")?
            .error_for_status()
            .wrap_err("openrouter models returned error status")?
            .json()
            .await
            .wrap_err("openrouter models response is not JSON")?;
        parse_openrouter_models(&resp)
    }

    #[cfg(test)]
    fn test_expire_cache(&self) {
        let mut guard = self.cache.lock().expect("model catalog cache poisoned");
        if let Some(entry) = guard.entry.as_mut() {
            entry.fetched_at = Instant::now() - CACHE_TTL - Duration::from_secs(1);
        }
    }

    #[cfg(test)]
    fn test_set_models_url(&mut self, url: String) {
        self.models_url = url;
    }
}

fn parse_openrouter_models(resp: &serde_json::Value) -> Result<HashMap<String, String>> {
    let data = resp
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| eyre::eyre!("openrouter payload missing 'data' array"))?;
    let mut map = HashMap::new();
    for entry in data {
        let (Some(id), Some(name)) = (
            entry.get("id").and_then(|v| v.as_str()),
            entry.get("name").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        map.insert(id.to_owned(), name.to_owned());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn openrouter_conn() -> AiConnection {
        AiConnection {
            backend: AiBackendKind::OpenAi,
            base_url: Some("https://openrouter.ai/api/v1".into()),
            model: "google/gemma-3-4b-it".into(),
            timeout: 60,
            reasoning_effort: None,
            service_tier: None,
        }
    }

    fn ollama_conn() -> AiConnection {
        AiConnection {
            backend: AiBackendKind::Ollama,
            base_url: Some("http://localhost:11434".into()),
            model: "gemma3:4b".into(),
            timeout: 60,
            reasoning_effort: None,
            service_tier: None,
        }
    }

    fn test_catalog(models_url: String) -> ModelCatalog {
        crate::install_crypto_provider();
        ModelCatalog::with_models_url(reqwest::Client::new(), models_url)
    }

    #[test]
    fn normalize_name_strips_free_suffix_and_colon_separator() {
        assert_eq!(
            ModelCatalog::normalize_name("Google: Gemma 4 31B (free)"),
            "Google Gemma 4 31B"
        );
        assert_eq!(
            ModelCatalog::normalize_name("Google: Gemma 3 4B"),
            "Google Gemma 3 4B"
        );
    }

    #[test]
    fn parse_fixture_builds_id_to_name_map() {
        let fixture = serde_json::json!({
            "data": [
                {"id": "google/gemma-3-4b-it", "name": "Google: Gemma 3 4B"},
                {"id": "openai/gpt-4o", "name": "OpenAI GPT-4o"},
            ]
        });
        let map = parse_openrouter_models(&fixture).unwrap();
        assert_eq!(
            map.get("google/gemma-3-4b-it").map(String::as_str),
            Some("Google: Gemma 3 4B")
        );
        assert_eq!(
            ModelCatalog::normalize_name(map.get("google/gemma-3-4b-it").unwrap()),
            "Google Gemma 3 4B"
        );
    }

    #[tokio::test]
    async fn display_name_unknown_openrouter_id_returns_raw_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "other/model", "name": "Other"}]
            })))
            .mount(&server)
            .await;

        let catalog = test_catalog(format!("{}/api/v1/models", server.uri()));
        let name = catalog
            .display_name(&openrouter_conn(), "google/gemma-3-4b-it")
            .await;
        assert_eq!(name, "google/gemma-3-4b-it");
    }

    #[tokio::test]
    async fn display_name_non_openrouter_returns_raw_id_without_fetch() {
        let catalog = test_catalog("http://127.0.0.1:1/unreachable".into());
        let name = catalog.display_name(&ollama_conn(), "gemma3:4b").await;
        assert_eq!(name, "gemma3:4b");
    }

    #[tokio::test]
    async fn display_name_on_fetch_failure_returns_raw_id() {
        let catalog = test_catalog("http://127.0.0.1:1/unreachable".into());
        let name = catalog
            .display_name(&openrouter_conn(), "google/gemma-3-4b-it")
            .await;
        assert_eq!(name, "google/gemma-3-4b-it");
    }

    #[tokio::test]
    async fn display_name_fetch_failure_is_not_retried_during_backoff() {
        let catalog = test_catalog("http://127.0.0.1:1/unreachable".into());
        let conn = openrouter_conn();

        let first = catalog.display_name(&conn, "google/gemma-3-4b-it").await;
        assert_eq!(first, "google/gemma-3-4b-it");

        // Second call within the backoff window must not hit the network again.
        let second = catalog.display_name(&conn, "google/gemma-3-4b-it").await;
        assert_eq!(second, "google/gemma-3-4b-it");
        assert!(catalog.in_fetch_backoff());
    }

    #[tokio::test]
    async fn display_name_uses_stale_cache_when_refresh_fails() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id": "google/gemma-3-4b-it", "name": "Google: Gemma 3 4B"},
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut catalog = test_catalog(format!("{}/api/v1/models", server.uri()));
        let conn = openrouter_conn();

        let name = catalog.display_name(&conn, "google/gemma-3-4b-it").await;
        assert_eq!(name, "Google Gemma 3 4B");

        catalog.test_expire_cache();
        catalog.test_set_models_url("http://127.0.0.1:1/unreachable".into());
        let name = catalog.display_name(&conn, "google/gemma-3-4b-it").await;
        assert_eq!(name, "Google Gemma 3 4B");
    }

    #[tokio::test]
    async fn display_name_resolves_known_openrouter_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id": "google/gemma-3-4b-it", "name": "Google: Gemma 3 4B"},
                ]
            })))
            .mount(&server)
            .await;

        let catalog = test_catalog(format!("{}/api/v1/models", server.uri()));
        let name = catalog
            .display_name(&openrouter_conn(), "google/gemma-3-4b-it")
            .await;
        assert_eq!(name, "Google Gemma 3 4B");
    }
}
