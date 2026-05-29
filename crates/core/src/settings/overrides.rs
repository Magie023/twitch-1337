//! Sparse override types written to `$DATA_DIR/settings.ron`.
//!
//! Every field is `Option`; `Some` wins on resolve, `None` falls through
//! to `Settings::compiled_defaults()`. The sparse shape removes "what does
//! an empty value mean" ambiguity and lets the dashboard's "reset" button
//! clear individual sections without inventing a sentinel.

use serde::{Deserialize, Serialize};

use super::SCHEMA_VERSION;
use super::ai::AiBackendKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsOverrides {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub cooldowns: CooldownsOverrides,
    #[serde(default)]
    pub pings: PingsOverrides,
    #[serde(default)]
    pub ai: AiOverrides,
    #[serde(default)]
    pub twitch: TwitchOverrides,
    #[serde(default)]
    pub aviationstack: AviationstackOverrides,
    #[serde(default)]
    pub suspend: SuspendOverrides,
    #[serde(default)]
    pub web: WebRuntimeOverrides,
    #[serde(default)]
    pub schedules: Option<Vec<crate::schedule::Schedule>>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl Default for SettingsOverrides {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            cooldowns: CooldownsOverrides::default(),
            pings: PingsOverrides::default(),
            ai: AiOverrides::default(),
            twitch: TwitchOverrides::default(),
            aviationstack: AviationstackOverrides::default(),
            suspend: SuspendOverrides::default(),
            web: WebRuntimeOverrides::default(),
            schedules: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CooldownsOverrides {
    #[serde(default)]
    pub ai: Option<u64>,
    #[serde(default)]
    pub news: Option<u64>,
    #[serde(default)]
    pub up: Option<u64>,
    #[serde(default)]
    pub feedback: Option<u64>,
    #[serde(default)]
    pub doener: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PingsOverrides {
    #[serde(default)]
    pub cooldown: Option<u64>,
    #[serde(default)]
    pub public: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiOverrides {
    #[serde(default)]
    pub connection: AiConnectionOverrides,
    #[serde(default)]
    pub behavior: AiBehaviorOverrides,
    #[serde(default)]
    pub history: AiHistoryOverrides,
    #[serde(default)]
    pub memory: AiMemoryOverrides,
    #[serde(default)]
    pub dreamer: AiDreamerOverrides,
    #[serde(default)]
    pub prefill: AiPrefillOverrides,
    #[serde(default)]
    pub web: AiWebOverrides,
    #[serde(default)]
    pub emotes: AiEmotesOverrides,
    #[serde(default)]
    pub media: AiMediaOverrides,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiConnectionOverrides {
    #[serde(default)]
    pub backend: Option<AiBackendKind>,
    /// `Option<Option<String>>`: outer `None` = leave at default, outer
    /// `Some(None)` = explicitly clear, `Some(Some(x))` = set to x.
    #[serde(default)]
    pub base_url: Option<Option<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub reasoning_effort: Option<Option<String>>,
    #[serde(default)]
    pub service_tier: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiBehaviorOverrides {
    #[serde(default)]
    pub max_turn_rounds: Option<usize>,
    #[serde(default)]
    pub max_writes_per_turn: Option<usize>,
    #[serde(default)]
    pub persona_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiHistoryOverrides {
    #[serde(default)]
    pub length: Option<u64>,
    #[serde(default)]
    pub ai_channel_length: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiMemoryOverrides {
    #[serde(default)]
    pub soul_bytes: Option<usize>,
    #[serde(default)]
    pub lore_bytes: Option<usize>,
    #[serde(default)]
    pub user_bytes: Option<usize>,
    #[serde(default)]
    pub state_bytes: Option<usize>,
    #[serde(default)]
    pub inject_byte_budget: Option<usize>,
    #[serde(default)]
    pub max_state_files: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiDreamerOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub model: Option<Option<String>>,
    #[serde(default)]
    pub reasoning_effort: Option<Option<String>>,
    #[serde(default)]
    pub service_tier: Option<Option<String>>,
    #[serde(default)]
    pub run_at: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_rounds: Option<usize>,
}

/// `threshold` is `f64`; manual `PartialEq`/`Eq` use bit comparison.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiPrefillOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub threshold: Option<f64>,
}

impl PartialEq for AiPrefillOverrides {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.base_url == other.base_url
            && match (self.threshold, other.threshold) {
                (Some(a), Some(b)) => a.to_bits() == b.to_bits(),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for AiPrefillOverrides {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiWebOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub max_results: Option<usize>,
    #[serde(default)]
    pub max_rounds: Option<usize>,
    #[serde(default)]
    pub cache_ttl_secs: Option<u64>,
    #[serde(default)]
    pub cache_capacity: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiEmotesOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub include_global: Option<bool>,
    #[serde(default)]
    pub refresh_interval_secs: Option<u64>,
    #[serde(default)]
    pub max_prompt_emotes: Option<usize>,
    #[serde(default)]
    pub min_baseline_emotes: Option<usize>,
    #[serde(default)]
    pub base_url: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiMediaOverrides {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub max_image_size: Option<bytesize::ByteSize>,
    #[serde(default)]
    pub max_pdf_size: Option<bytesize::ByteSize>,
    #[serde(default)]
    pub max_audio_size: Option<bytesize::ByteSize>,
    #[serde(default)]
    pub max_video_size: Option<bytesize::ByteSize>,
    #[serde(default)]
    pub max_text_size: Option<bytesize::ByteSize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TwitchOverrides {
    #[serde(default)]
    pub expected_latency: Option<u32>,
    #[serde(default)]
    pub hidden_admins: Option<Vec<String>>,
    #[serde(default)]
    pub viewer_allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub admin_channel: Option<Option<String>>,
    #[serde(default)]
    pub ai_channel: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AviationstackOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuspendOverrides {
    #[serde(default)]
    pub default_duration_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebRuntimeOverrides {
    #[serde(default)]
    pub session_ttl_secs: Option<u64>,
    #[serde(default)]
    pub mod_check_refresh_secs: Option<u64>,
}
