//! Owner-only proxy that fetches the upstream LLM provider's model list and
//! caches it for 5 minutes. Used by the settings page model picker.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Query, State};
use reqwest::Client;
use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize};
use tracing::warn;
use twitch_1337_core::settings::ai::AiBackendKind;

use crate::state::WebState;

const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
pub struct Params {
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "connection".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelEntry>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct ModelListCache {
    inner: Mutex<Option<CacheEntry>>,
}

struct CacheEntry {
    key: (AiBackendKind, Option<String>),
    fetched_at: Instant,
    models: Vec<ModelEntry>,
}

impl ModelListCache {
    pub fn get(&self, key: &(AiBackendKind, Option<String>)) -> Option<Vec<ModelEntry>> {
        let guard = self.inner.lock().expect("model cache poisoned");
        let entry = guard.as_ref()?;
        if entry.key != *key {
            return None;
        }
        if entry.fetched_at.elapsed() > CACHE_TTL {
            return None;
        }
        Some(entry.models.clone())
    }

    pub fn put(&self, key: (AiBackendKind, Option<String>), models: Vec<ModelEntry>) {
        let mut guard = self.inner.lock().expect("model cache poisoned");
        *guard = Some(CacheEntry {
            key,
            fetched_at: Instant::now(),
            models,
        });
    }
}

pub async fn get_ai_models(
    State(state): State<WebState>,
    Query(_params): Query<Params>,
) -> Json<ModelsResponse> {
    let settings = state.settings.load();
    let conn = &settings.ai.connection;
    let key = (conn.backend, conn.base_url.clone());

    if let Some(models) = state.model_cache.get(&key) {
        return Json(ModelsResponse {
            models,
            error: None,
        });
    }

    let api_key = state
        .ai_bootstrap
        .as_ref()
        .map(|b| b.api_key.expose_secret().to_owned());

    let result = match conn.backend {
        AiBackendKind::OpenAi => {
            fetch_openai(
                &state.http,
                conn.base_url
                    .as_deref()
                    .unwrap_or("https://api.openai.com/v1"),
                api_key.as_deref().unwrap_or(""),
            )
            .await
        }
        AiBackendKind::Ollama => {
            fetch_ollama(
                &state.http,
                conn.base_url.as_deref().unwrap_or("http://localhost:11434"),
            )
            .await
        }
    };

    match result {
        Ok(models) => {
            state.model_cache.put(key, models.clone());
            Json(ModelsResponse {
                models,
                error: None,
            })
        }
        Err(e) => {
            warn!(error = ?e, "model list fetch failed");
            Json(ModelsResponse {
                models: Vec::new(),
                error: Some(format!("{e:#}")),
            })
        }
    }
}

async fn fetch_openai(http: &Client, base: &str, api_key: &str) -> eyre::Result<Vec<ModelEntry>> {
    let url = format!("{}/models", base.trim_end_matches('/'));
    let resp: serde_json::Value = http
        .get(&url)
        .bearer_auth(api_key)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let data = resp
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| eyre::eyre!("upstream payload missing 'data' array"))?;
    Ok(data
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_owned))
        .map(|id| ModelEntry {
            label: id.clone(),
            id,
        })
        .collect())
}

async fn fetch_ollama(http: &Client, base: &str) -> eyre::Result<Vec<ModelEntry>> {
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    let resp: serde_json::Value = http
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let arr = resp
        .get("models")
        .and_then(|v| v.as_array())
        .ok_or_else(|| eyre::eyre!("upstream payload missing 'models' array"))?;
    Ok(arr
        .iter()
        .filter_map(|m| m.get("name").and_then(|v| v.as_str()).map(str::to_owned))
        .map(|id| ModelEntry {
            label: id.clone(),
            id,
        })
        .collect())
}
