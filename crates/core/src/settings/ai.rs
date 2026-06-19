//! AI subsection of dashboard settings.
//!
//! Mirrors the runtime shape of the old `core::config::AiConfig`
//! minus the `api_key` secret. Defaults intentionally match the
//! pre-hoist behavior so existing deployments see no drift after
//! the schema bump.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiSettings {
    pub connection: AiConnection,
    pub behavior: AiBehavior,
    pub history: AiHistory,
    pub memory: AiMemory,
    pub dreamer: AiDreamer,
    pub prefill: Option<AiPrefill>,
    pub web: Option<AiWeb>,
    pub emotes: Option<AiEmotes>,
    pub media: AiMedia,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiConnection {
    pub backend: AiBackendKind,
    pub base_url: Option<String>,
    pub model: String,
    pub timeout: u64,
    pub reasoning_effort: Option<String>,
    /// OpenRouter service tier hint. `None` = default tier (no field sent).
    /// Documented values: `"flex"`, `"priority"`. Only honored when the
    /// connection points at OpenRouter; stripped at serialize time otherwise.
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AiBackendKind {
    OpenAi,
    Ollama,
}

impl AiBackendKind {
    /// Stable lowercase string form — matches the `serde(rename_all)` output
    /// and is consumed by the settings page (`<input value=…>`) plus the POST
    /// parser. Keeping this in code lets templates avoid re-doing the match.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Ollama => "ollama",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiBehavior {
    pub max_turn_rounds: usize,
    pub max_writes_per_turn: usize,
    /// Persona name the bot uses in chat-history identity push and chat-line
    /// rendering. Defaults to `"Aurora"`. Missing in pre-existing settings.ron
    /// files is handled by `#[serde(default = ...)]` so v2 reads stay forward-
    /// compatible without a new migration sentinel.
    #[serde(default = "default_persona_name")]
    pub persona_name: String,
}

fn default_persona_name() -> String {
    "Aurora".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiHistory {
    pub length: u64,
    pub ai_channel_length: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiMemory {
    pub soul_bytes: usize,
    pub lore_bytes: usize,
    pub user_bytes: usize,
    pub state_bytes: usize,
    pub inject_byte_budget: usize,
    pub max_state_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiDreamer {
    pub enabled: bool,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    /// OpenRouter service tier hint for the dreamer pass. Falls back to
    /// `connection.service_tier` when `None`. Documented values: `"flex"`,
    /// `"priority"`.
    pub service_tier: Option<String>,
    pub run_at: String,
    pub timeout_secs: u64,
    pub max_rounds: usize,
    /// Per-turn `write_file` quota for the dreamer ritual. Separate from the
    /// chat-turn cap (`AiBehavior::max_writes_per_turn`): the dreamer rewrites
    /// SOUL + LORE + many user files in one pass, so it defaults higher (32 vs
    /// the chat cap's 8). Both are bounded to the same `1..=64` range.
    pub max_writes_per_turn: usize,
}

/// Prefill config — `threshold` is `f64` compared as bits to allow `Eq`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiPrefill {
    pub base_url: String,
    pub threshold: f64,
}

impl PartialEq for AiPrefill {
    fn eq(&self, other: &Self) -> bool {
        self.base_url == other.base_url && self.threshold.to_bits() == other.threshold.to_bits()
    }
}

impl Eq for AiPrefill {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiWeb {
    pub base_url: String,
    pub timeout: u64,
    pub max_results: usize,
    pub max_rounds: usize,
    pub cache_ttl_secs: u64,
    pub cache_capacity: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiEmotes {
    pub include_global: bool,
    pub refresh_interval_secs: u64,
    pub max_prompt_emotes: usize,
    pub min_baseline_emotes: usize,
    /// Emote codes always injected into the per-turn block (the persona's
    /// reflex emotes from the live persona prompt). Names absent from the
    /// baked glossary are dropped with a warning; the list must not exceed
    /// `max_prompt_emotes`. Owner-managed; not auto-synced from SOUL.md.
    #[serde(default = "default_pinned_emotes")]
    pub pinned_emotes: Vec<String>,
    pub base_url: Option<String>,
}

/// Seed pins to the documented soul-reflex codes that exist in the baked
/// glossary, so the reported "persona told to use emotes it isn't given" bug
/// is fixed out of the box. Owner extends via the dashboard.
fn default_pinned_emotes() -> Vec<String> {
    vec!["PepeLa".to_string(), "okjj".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiMedia {
    pub model: String,
    pub timeout: u64,
    pub max_image_size: bytesize::ByteSize,
    pub max_pdf_size: bytesize::ByteSize,
    pub max_audio_size: bytesize::ByteSize,
    pub max_video_size: bytesize::ByteSize,
    pub max_text_size: bytesize::ByteSize,
}

impl Default for AiConnection {
    fn default() -> Self {
        Self {
            backend: AiBackendKind::OpenAi,
            base_url: None,
            model: String::new(),
            timeout: 30,
            reasoning_effort: None,
            service_tier: None,
        }
    }
}

impl Default for AiBehavior {
    fn default() -> Self {
        Self {
            max_turn_rounds: 4,
            max_writes_per_turn: 8,
            persona_name: default_persona_name(),
        }
    }
}

impl Default for AiHistory {
    fn default() -> Self {
        Self {
            length: crate::ai::chat_history::DEFAULT_HISTORY_LENGTH,
            ai_channel_length: 50,
        }
    }
}

impl Default for AiMemory {
    fn default() -> Self {
        Self {
            soul_bytes: 4096,
            lore_bytes: 12_288,
            user_bytes: 4096,
            state_bytes: 2048,
            inject_byte_budget: 24_576,
            max_state_files: 16,
        }
    }
}

impl Default for AiDreamer {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            reasoning_effort: None,
            service_tier: None,
            run_at: "04:00".into(),
            timeout_secs: 120,
            max_rounds: 20,
            max_writes_per_turn: 32,
        }
    }
}

impl Default for AiPrefill {
    fn default() -> Self {
        Self {
            base_url: "https://logs.zonian.dev".into(),
            threshold: 0.5,
        }
    }
}

impl Default for AiWeb {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8080/search".into(),
            timeout: 15,
            max_results: 5,
            max_rounds: 3,
            cache_ttl_secs: 300,
            cache_capacity: 100,
        }
    }
}

impl Default for AiEmotes {
    fn default() -> Self {
        Self {
            include_global: true,
            refresh_interval_secs: 3600,
            max_prompt_emotes: 20,
            min_baseline_emotes: 4,
            pinned_emotes: default_pinned_emotes(),
            base_url: None,
        }
    }
}

impl Default for AiMedia {
    fn default() -> Self {
        Self {
            model: "~google/gemini-flash-latest".into(),
            timeout: 60,
            max_image_size: bytesize::ByteSize::mib(10),
            max_pdf_size: bytesize::ByteSize::mib(25),
            max_audio_size: bytesize::ByteSize::mib(25),
            max_video_size: bytesize::ByteSize::mib(50),
            max_text_size: bytesize::ByteSize::mib(1),
        }
    }
}

impl AiMedia {
    pub fn cap_for(&self, bucket: crate::ai::content::detect::Bucket) -> bytesize::ByteSize {
        use crate::ai::content::detect::Bucket;
        match bucket {
            Bucket::Image => self.max_image_size,
            Bucket::Pdf => self.max_pdf_size,
            Bucket::Audio => self.max_audio_size,
            Bucket::Video => self.max_video_size,
            Bucket::Text => self.max_text_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_behavior_default_has_persona_aurora() {
        let b = AiBehavior::default();
        assert_eq!(b.persona_name, "Aurora");
    }

    #[test]
    fn ai_emotes_defaults_pin_soul_reflexes_and_widen_window() {
        let e = AiEmotes::default();
        assert_eq!(e.max_prompt_emotes, 20, "default window widened 12 -> 20");
        assert_eq!(
            e.pinned_emotes,
            vec!["PepeLa".to_string(), "okjj".to_string()]
        );
    }

    #[test]
    fn defaults_match_legacy_ai_config_defaults() {
        let s = AiSettings::default();
        assert_eq!(s.connection.timeout, 30);
        assert!(s.connection.base_url.is_none());
        assert_eq!(s.behavior.max_turn_rounds, 4);
        assert_eq!(s.behavior.max_writes_per_turn, 8);
        assert_eq!(
            s.history.length,
            crate::ai::chat_history::DEFAULT_HISTORY_LENGTH
        );
        assert_eq!(s.history.ai_channel_length, 50);
        assert_eq!(s.memory.soul_bytes, 4096);
        assert_eq!(s.memory.lore_bytes, 12_288);
        assert_eq!(s.memory.user_bytes, 4096);
        assert_eq!(s.memory.state_bytes, 2048);
        assert_eq!(s.memory.inject_byte_budget, 24_576);
        assert_eq!(s.memory.max_state_files, 16);
        assert!(s.dreamer.enabled);
        assert_eq!(s.dreamer.run_at, "04:00");
        assert_eq!(s.dreamer.timeout_secs, 120);
        assert_eq!(s.dreamer.max_rounds, 20);
        assert_eq!(s.dreamer.max_writes_per_turn, 32);
        assert!(s.prefill.is_none());
        assert!(s.web.is_none());
        assert!(s.emotes.is_none());
        assert_eq!(s.media.model, "~google/gemini-flash-latest");
        assert_eq!(s.media.timeout, 60);
        assert_eq!(s.media.max_image_size.as_u64(), 10 * 1024 * 1024);
        assert!(s.connection.service_tier.is_none());
        assert!(s.dreamer.service_tier.is_none());
    }
}
