//! Aviationstack metadata enrichment settings. `api_key` stays in
//! config.toml as a secret; the dashboard manages the rest.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AviationstackSettings {
    pub enabled: bool,
    pub base_url: String,
    pub timeout_secs: u64,
}

impl Default for AviationstackSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.aviationstack.com/v1".to_owned(),
            timeout_secs: 5,
        }
    }
}
