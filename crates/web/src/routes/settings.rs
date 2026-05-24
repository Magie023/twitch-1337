//! Owner-only settings page: live cooldowns + pings runtime knobs.
//!
//! The page reads `state.settings` for the current effective values and
//! `state.settings_store.defaults()` to show the compile-time fallbacks
//! beside each input. Saves go through `SettingsStore::apply`, which
//! validates, atomically persists, swaps the shared handle, and records an
//! audit entry. Reset clears one section (`cooldowns` or `pings`) back to
//! its defaults.

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::settings::overrides::{
    AiBehaviorOverrides, AiConnectionOverrides, AiDreamerOverrides, AiEmotesOverrides,
    AiHistoryOverrides, AiMediaOverrides, AiMemoryOverrides, AiPrefillOverrides, AiWebOverrides,
    AviationstackOverrides, SuspendOverrides, TwitchOverrides, WebRuntimeOverrides,
};
use twitch_1337_core::settings::{
    Actor, AiBackendKind, AiOverrides, AiSettings, Cooldowns, CooldownsOverrides, FieldError,
    PingsOverrides, PingsSettings, Settings, SettingsError, SettingsOverrides, SettingsSection,
};

use crate::auth::Role;
use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::routes::{render, render_with};
use crate::state::WebState;

pub fn owner_router() -> Router<WebState> {
    Router::new()
        .route("/settings", get(show).post(save))
        .route("/settings/reset/{section}", post(reset))
}

#[derive(Template)]
#[template(path = "settings/index.html")]
struct ShowTpl {
    csrf: String,
    flash: Option<String>,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
    current: Settings,
    defaults: Settings,
    errors: Vec<FieldError>,
}

async fn show(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let current = (**state.settings.load()).clone();
    let defaults = state.settings_store.defaults().clone();
    render(&ShowTpl {
        csrf: csrf::encode(&session.csrf_value),
        flash: flash::take(&cookies),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::SETTINGS,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, Role::Owner),
        current,
        defaults,
        errors: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Maps a submitted optional string to a tri-state override value:
/// - `None`      -> field absent from form (no change)
/// - `Some("")`  -> present but empty -> explicit clear (`Some(None)`)
/// - `Some(v)`   -> set to v (`Some(Some(v))`)
fn tri_state(val: Option<String>) -> Option<Option<String>> {
    match val {
        None => None,
        Some(s) if s.is_empty() => Some(None),
        Some(s) => Some(Some(s)),
    }
}

/// Variant of [`tri_state`] for segmented-selector fields that use the literal
/// `"none"` as their "no value selected" option. Normalises `"none"` to `""`
/// before delegating so the sentinel stays at the call site, not in the shared
/// helper (a future plain-text field whose value happens to be "none" must not
/// accidentally clear itself).
fn segmented_tri_state(val: Option<String>) -> Option<Option<String>> {
    tri_state(val.map(|s| {
        let t = s.trim();
        if t == "none" || t.is_empty() {
            String::new()
        } else {
            s
        }
    }))
}

/// Resolves the `enabled` field for toggle cards that use a `*_card_visible`
/// hidden input to differentiate "card rendered but unchecked" from "card absent".
fn enabled_from_card(visible: &Option<String>, checked: &Option<String>) -> Option<bool> {
    visible.is_some().then(|| checked.is_some())
}

// ---------------------------------------------------------------------------
// Per-card form structs
// ---------------------------------------------------------------------------

#[derive(Default, Deserialize)]
struct CooldownsForm {
    #[serde(default)]
    cooldown_ai: u64,
    #[serde(default)]
    cooldown_news: u64,
    #[serde(default)]
    cooldown_up: u64,
    #[serde(default)]
    cooldown_feedback: u64,
    #[serde(default)]
    cooldown_doener: u64,
}

impl CooldownsForm {
    fn into_overrides(self) -> CooldownsOverrides {
        CooldownsOverrides {
            ai: Some(self.cooldown_ai),
            news: Some(self.cooldown_news),
            up: Some(self.cooldown_up),
            feedback: Some(self.cooldown_feedback),
            doener: Some(self.cooldown_doener),
        }
    }
}

#[derive(Default, Deserialize)]
struct PingsForm {
    #[serde(default)]
    ping_cooldown: u64,
    /// Unchecked HTML checkboxes don't submit a value at all, so a missing
    /// `ping_public` key means "false". Form fields with `value="1"` send
    /// `Some("1")` when checked.
    #[serde(default)]
    ping_public: Option<String>,
}

impl PingsForm {
    fn into_overrides(self) -> PingsOverrides {
        PingsOverrides {
            cooldown: Some(self.ping_cooldown),
            public: Some(self.ping_public.is_some()),
        }
    }
}

#[derive(Default, Deserialize)]
struct AiConnectionForm {
    #[serde(default)]
    ai_connection_backend: Option<String>,
    #[serde(default)]
    ai_connection_base_url: Option<String>,
    #[serde(default)]
    ai_connection_model: Option<String>,
    #[serde(default)]
    ai_connection_timeout: Option<u64>,
    #[serde(default)]
    ai_connection_reasoning_effort: Option<String>,
    #[serde(default)]
    ai_connection_service_tier: Option<String>,
}

impl AiConnectionForm {
    fn into_overrides(self) -> AiConnectionOverrides {
        AiConnectionOverrides {
            backend: self.ai_connection_backend.as_deref().and_then(|s| match s {
                "openai" => Some(AiBackendKind::OpenAi),
                "ollama" => Some(AiBackendKind::Ollama),
                _ => None,
            }),
            base_url: self
                .ai_connection_base_url
                .map(|s| if s.trim().is_empty() { None } else { Some(s) }),
            model: self.ai_connection_model,
            timeout: self.ai_connection_timeout,
            reasoning_effort: segmented_tri_state(self.ai_connection_reasoning_effort),
            service_tier: segmented_tri_state(self.ai_connection_service_tier),
        }
    }
}

#[derive(Default, Deserialize)]
struct AiBehaviorForm {
    #[serde(default)]
    ai_behavior_max_turn_rounds: Option<usize>,
    #[serde(default)]
    ai_behavior_max_writes_per_turn: Option<usize>,
    #[serde(default)]
    ai_behavior_persona_name: Option<String>,
}

impl AiBehaviorForm {
    fn into_overrides(self) -> AiBehaviorOverrides {
        AiBehaviorOverrides {
            max_turn_rounds: self.ai_behavior_max_turn_rounds,
            max_writes_per_turn: self.ai_behavior_max_writes_per_turn,
            persona_name: self
                .ai_behavior_persona_name
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        }
    }
}

#[derive(Default, Deserialize)]
struct AiHistoryForm {
    #[serde(default)]
    ai_history_length: Option<u64>,
    #[serde(default)]
    ai_history_ai_channel_length: Option<u64>,
}

impl AiHistoryForm {
    fn into_overrides(self) -> AiHistoryOverrides {
        AiHistoryOverrides {
            length: self.ai_history_length,
            ai_channel_length: self.ai_history_ai_channel_length,
        }
    }
}

#[derive(Default, Deserialize)]
struct AiMemoryForm {
    #[serde(default)]
    ai_memory_soul_bytes: Option<usize>,
    #[serde(default)]
    ai_memory_lore_bytes: Option<usize>,
    #[serde(default)]
    ai_memory_user_bytes: Option<usize>,
    #[serde(default)]
    ai_memory_state_bytes: Option<usize>,
    #[serde(default)]
    ai_memory_inject_byte_budget: Option<usize>,
    #[serde(default)]
    ai_memory_max_state_files: Option<usize>,
}

impl AiMemoryForm {
    fn into_overrides(self) -> AiMemoryOverrides {
        AiMemoryOverrides {
            soul_bytes: self.ai_memory_soul_bytes,
            lore_bytes: self.ai_memory_lore_bytes,
            user_bytes: self.ai_memory_user_bytes,
            state_bytes: self.ai_memory_state_bytes,
            inject_byte_budget: self.ai_memory_inject_byte_budget,
            max_state_files: self.ai_memory_max_state_files,
        }
    }
}

#[derive(Default, Deserialize)]
struct AiDreamerForm {
    #[serde(default)]
    ai_dreamer_enabled: Option<String>,
    #[serde(default)]
    ai_dreamer_model: Option<String>,
    #[serde(default)]
    ai_dreamer_reasoning_effort: Option<String>,
    #[serde(default)]
    ai_dreamer_service_tier: Option<String>,
    #[serde(default)]
    ai_dreamer_run_at: Option<String>,
    #[serde(default)]
    ai_dreamer_timeout_secs: Option<u64>,
    #[serde(default)]
    ai_dreamer_max_rounds: Option<usize>,
}

impl AiDreamerForm {
    fn into_overrides(self) -> AiDreamerOverrides {
        AiDreamerOverrides {
            // Dreamer card always renders, so checkbox semantics mirror
            // `ping_public`: missing key = false, present = true.
            enabled: Some(self.ai_dreamer_enabled.is_some()),
            model: self
                .ai_dreamer_model
                .map(|v| if v.is_empty() { None } else { Some(v) }),
            reasoning_effort: segmented_tri_state(self.ai_dreamer_reasoning_effort),
            service_tier: segmented_tri_state(self.ai_dreamer_service_tier),
            run_at: self.ai_dreamer_run_at,
            timeout_secs: self.ai_dreamer_timeout_secs,
            max_rounds: self.ai_dreamer_max_rounds,
        }
    }
}

#[derive(Default, Deserialize)]
struct AiPrefillForm {
    /// Hidden marker emitted by the rendered card so the handler can tell
    /// "card visible but unchecked" from "card not in form".
    #[serde(default)]
    ai_prefill_card_visible: Option<String>,
    #[serde(default)]
    ai_prefill_enabled: Option<String>,
    #[serde(default)]
    ai_prefill_base_url: Option<String>,
    #[serde(default)]
    ai_prefill_threshold: Option<f64>,
}

impl AiPrefillForm {
    fn into_overrides(self) -> AiPrefillOverrides {
        AiPrefillOverrides {
            enabled: enabled_from_card(&self.ai_prefill_card_visible, &self.ai_prefill_enabled),
            base_url: self.ai_prefill_base_url,
            threshold: self.ai_prefill_threshold,
        }
    }
}

#[derive(Default, Deserialize)]
struct AiWebForm {
    #[serde(default)]
    ai_web_card_visible: Option<String>,
    #[serde(default)]
    ai_web_enabled: Option<String>,
    #[serde(default)]
    ai_web_base_url: Option<String>,
    #[serde(default)]
    ai_web_timeout: Option<u64>,
    #[serde(default)]
    ai_web_max_results: Option<usize>,
    #[serde(default)]
    ai_web_max_rounds: Option<usize>,
    #[serde(default)]
    ai_web_cache_ttl_secs: Option<u64>,
    #[serde(default)]
    ai_web_cache_capacity: Option<usize>,
}

impl AiWebForm {
    fn into_overrides(self) -> AiWebOverrides {
        AiWebOverrides {
            enabled: enabled_from_card(&self.ai_web_card_visible, &self.ai_web_enabled),
            base_url: self.ai_web_base_url,
            timeout: self.ai_web_timeout,
            max_results: self.ai_web_max_results,
            max_rounds: self.ai_web_max_rounds,
            cache_ttl_secs: self.ai_web_cache_ttl_secs,
            cache_capacity: self.ai_web_cache_capacity,
        }
    }
}

#[derive(Default, Deserialize)]
struct AiEmotesForm {
    #[serde(default)]
    ai_emotes_card_visible: Option<String>,
    #[serde(default)]
    ai_emotes_enabled: Option<String>,
    #[serde(default)]
    ai_emotes_include_global: Option<String>,
    #[serde(default)]
    ai_emotes_refresh_interval_secs: Option<u64>,
    #[serde(default)]
    ai_emotes_max_prompt_emotes: Option<usize>,
    #[serde(default)]
    ai_emotes_min_baseline_emotes: Option<usize>,
    #[serde(default)]
    ai_emotes_base_url: Option<String>,
}

impl AiEmotesForm {
    fn into_overrides(self) -> AiEmotesOverrides {
        AiEmotesOverrides {
            enabled: enabled_from_card(&self.ai_emotes_card_visible, &self.ai_emotes_enabled),
            // include_global only meaningful when the card is visible; mirror
            // the prefill/web pattern so the inner checkbox can be unchecked
            // explicitly without disabling the whole card.
            include_global: self
                .ai_emotes_card_visible
                .as_ref()
                .map(|_| self.ai_emotes_include_global.is_some()),
            refresh_interval_secs: self.ai_emotes_refresh_interval_secs,
            max_prompt_emotes: self.ai_emotes_max_prompt_emotes,
            min_baseline_emotes: self.ai_emotes_min_baseline_emotes,
            base_url: self
                .ai_emotes_base_url
                .map(|v| if v.is_empty() { None } else { Some(v) }),
        }
    }
}

#[derive(Default, Deserialize)]
struct AiMediaForm {
    #[serde(default)]
    ai_media_model: Option<String>,
    #[serde(default)]
    ai_media_timeout: Option<u64>,
    #[serde(default)]
    ai_media_max_image_size: Option<String>,
    #[serde(default)]
    ai_media_max_pdf_size: Option<String>,
    #[serde(default)]
    ai_media_max_audio_size: Option<String>,
    #[serde(default)]
    ai_media_max_video_size: Option<String>,
    #[serde(default)]
    ai_media_max_text_size: Option<String>,
}

impl AiMediaForm {
    fn into_overrides(self) -> AiMediaOverrides {
        AiMediaOverrides {
            model: self.ai_media_model,
            timeout: self.ai_media_timeout,
            max_image_size: self
                .ai_media_max_image_size
                .as_deref()
                .and_then(|s| s.parse().ok()),
            max_pdf_size: self
                .ai_media_max_pdf_size
                .as_deref()
                .and_then(|s| s.parse().ok()),
            max_audio_size: self
                .ai_media_max_audio_size
                .as_deref()
                .and_then(|s| s.parse().ok()),
            max_video_size: self
                .ai_media_max_video_size
                .as_deref()
                .and_then(|s| s.parse().ok()),
            max_text_size: self
                .ai_media_max_text_size
                .as_deref()
                .and_then(|s| s.parse().ok()),
        }
    }
}

fn parse_id_list(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

#[derive(Default, Deserialize)]
struct TwitchPermissionsForm {
    #[serde(default)]
    twitch_hidden_admins: Option<String>,
    #[serde(default)]
    twitch_viewer_allowlist: Option<String>,
}

impl TwitchPermissionsForm {
    fn into_overrides(self) -> TwitchOverrides {
        TwitchOverrides {
            hidden_admins: self.twitch_hidden_admins.as_deref().map(parse_id_list),
            viewer_allowlist: self.twitch_viewer_allowlist.as_deref().map(parse_id_list),
            ..Default::default()
        }
    }
}

#[derive(Default, Deserialize)]
struct TwitchChannelsForm {
    #[serde(default)]
    twitch_expected_latency: Option<u32>,
    #[serde(default)]
    twitch_admin_channel: Option<String>,
    #[serde(default)]
    twitch_ai_channel: Option<String>,
}

impl TwitchChannelsForm {
    fn into_overrides(self) -> TwitchOverrides {
        TwitchOverrides {
            expected_latency: self.twitch_expected_latency,
            admin_channel: tri_state(self.twitch_admin_channel),
            ai_channel: tri_state(self.twitch_ai_channel),
            ..Default::default()
        }
    }
}

#[derive(Default, Deserialize)]
struct AviationstackForm {
    /// Hidden sentinel emitted by the rendered card so the handler can tell
    /// "card visible but checkbox unchecked" from "card absent (don't touch)".
    #[serde(default)]
    aviationstack_card_visible: Option<String>,
    /// Checkbox: `"1"` when checked, absent when unchecked.
    #[serde(default)]
    aviationstack_enabled: Option<String>,
    #[serde(default)]
    aviationstack_base_url: Option<String>,
    #[serde(default)]
    aviationstack_timeout_secs: Option<u64>,
}

impl AviationstackForm {
    fn into_overrides(self) -> AviationstackOverrides {
        AviationstackOverrides {
            enabled: enabled_from_card(
                &self.aviationstack_card_visible,
                &self.aviationstack_enabled,
            ),
            base_url: self.aviationstack_base_url,
            timeout_secs: self.aviationstack_timeout_secs,
        }
    }
}

#[derive(Default, Deserialize)]
struct SuspendForm {
    #[serde(default)]
    suspend_default_duration_secs: Option<u64>,
}

impl SuspendForm {
    fn into_overrides(self) -> SuspendOverrides {
        SuspendOverrides {
            default_duration_secs: self.suspend_default_duration_secs,
        }
    }
}

#[derive(Default, Deserialize)]
struct WebRuntimeForm {
    #[serde(default)]
    web_session_ttl_secs: Option<u64>,
    #[serde(default)]
    web_mod_check_refresh_secs: Option<u64>,
}

impl WebRuntimeForm {
    fn into_overrides(self) -> WebRuntimeOverrides {
        WebRuntimeOverrides {
            session_ttl_secs: self.web_session_ttl_secs,
            mod_check_refresh_secs: self.web_mod_check_refresh_secs,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level form — flat serde struct (serde_urlencoded does not support
// #[serde(flatten)]; each field lives at the top level, then save() bundles
// them into per-card structs before calling into_overrides()).
// ---------------------------------------------------------------------------

#[derive(Default, Deserialize)]
struct SaveForm {
    #[serde(rename = "_csrf")]
    csrf: String,

    // ---- cooldowns ----
    #[serde(default)]
    cooldown_ai: u64,
    #[serde(default)]
    cooldown_news: u64,
    #[serde(default)]
    cooldown_up: u64,
    #[serde(default)]
    cooldown_feedback: u64,
    #[serde(default)]
    cooldown_doener: u64,

    // ---- pings ----
    #[serde(default)]
    ping_cooldown: u64,
    #[serde(default)]
    ping_public: Option<String>,

    // ---- AI connection card ----
    #[serde(default)]
    ai_connection_backend: Option<String>,
    #[serde(default)]
    ai_connection_base_url: Option<String>,
    #[serde(default)]
    ai_connection_model: Option<String>,
    #[serde(default)]
    ai_connection_timeout: Option<u64>,
    #[serde(default)]
    ai_connection_reasoning_effort: Option<String>,
    #[serde(default)]
    ai_connection_service_tier: Option<String>,

    // ---- AI behavior card ----
    #[serde(default)]
    ai_behavior_max_turn_rounds: Option<usize>,
    #[serde(default)]
    ai_behavior_max_writes_per_turn: Option<usize>,
    #[serde(default)]
    ai_behavior_persona_name: Option<String>,

    // ---- AI history card ----
    #[serde(default)]
    ai_history_length: Option<u64>,
    #[serde(default)]
    ai_history_ai_channel_length: Option<u64>,

    // ---- AI memory card ----
    #[serde(default)]
    ai_memory_soul_bytes: Option<usize>,
    #[serde(default)]
    ai_memory_lore_bytes: Option<usize>,
    #[serde(default)]
    ai_memory_user_bytes: Option<usize>,
    #[serde(default)]
    ai_memory_state_bytes: Option<usize>,
    #[serde(default)]
    ai_memory_inject_byte_budget: Option<usize>,
    #[serde(default)]
    ai_memory_max_state_files: Option<usize>,

    // ---- AI dreamer card ----
    #[serde(default)]
    ai_dreamer_enabled: Option<String>,
    #[serde(default)]
    ai_dreamer_model: Option<String>,
    #[serde(default)]
    ai_dreamer_reasoning_effort: Option<String>,
    #[serde(default)]
    ai_dreamer_service_tier: Option<String>,
    #[serde(default)]
    ai_dreamer_run_at: Option<String>,
    #[serde(default)]
    ai_dreamer_timeout_secs: Option<u64>,
    #[serde(default)]
    ai_dreamer_max_rounds: Option<usize>,

    // ---- AI prefill toggle card ----
    /// Hidden marker so the handler can tell "card visible but unchecked"
    /// from "card not in form".
    #[serde(default)]
    ai_prefill_card_visible: Option<String>,
    #[serde(default)]
    ai_prefill_enabled: Option<String>,
    #[serde(default)]
    ai_prefill_base_url: Option<String>,
    #[serde(default)]
    ai_prefill_threshold: Option<f64>,

    // ---- AI web toggle card ----
    #[serde(default)]
    ai_web_card_visible: Option<String>,
    #[serde(default)]
    ai_web_enabled: Option<String>,
    #[serde(default)]
    ai_web_base_url: Option<String>,
    #[serde(default)]
    ai_web_timeout: Option<u64>,
    #[serde(default)]
    ai_web_max_results: Option<usize>,
    #[serde(default)]
    ai_web_max_rounds: Option<usize>,
    #[serde(default)]
    ai_web_cache_ttl_secs: Option<u64>,
    #[serde(default)]
    ai_web_cache_capacity: Option<usize>,

    // ---- AI emotes toggle card ----
    #[serde(default)]
    ai_emotes_card_visible: Option<String>,
    #[serde(default)]
    ai_emotes_enabled: Option<String>,
    #[serde(default)]
    ai_emotes_include_global: Option<String>,
    #[serde(default)]
    ai_emotes_refresh_interval_secs: Option<u64>,
    #[serde(default)]
    ai_emotes_max_prompt_emotes: Option<usize>,
    #[serde(default)]
    ai_emotes_min_baseline_emotes: Option<usize>,
    #[serde(default)]
    ai_emotes_base_url: Option<String>,

    // ---- AI media card ----
    #[serde(default)]
    ai_media_model: Option<String>,
    #[serde(default)]
    ai_media_timeout: Option<u64>,
    #[serde(default)]
    ai_media_max_image_size: Option<String>,
    #[serde(default)]
    ai_media_max_pdf_size: Option<String>,
    #[serde(default)]
    ai_media_max_audio_size: Option<String>,
    #[serde(default)]
    ai_media_max_video_size: Option<String>,
    #[serde(default)]
    ai_media_max_text_size: Option<String>,

    // ---- Twitch · Permissions card ----
    #[serde(default)]
    twitch_hidden_admins: Option<String>,
    #[serde(default)]
    twitch_viewer_allowlist: Option<String>,

    // ---- Twitch · Channels card ----
    #[serde(default)]
    twitch_expected_latency: Option<u32>,
    #[serde(default)]
    twitch_admin_channel: Option<String>,
    #[serde(default)]
    twitch_ai_channel: Option<String>,

    // ---- Aviationstack card ----
    #[serde(default)]
    aviationstack_card_visible: Option<String>,
    #[serde(default)]
    aviationstack_enabled: Option<String>,
    #[serde(default)]
    aviationstack_base_url: Option<String>,
    #[serde(default)]
    aviationstack_timeout_secs: Option<u64>,

    // ---- Suspend card ----
    #[serde(default)]
    suspend_default_duration_secs: Option<u64>,

    // ---- Web · Sessions card ----
    #[serde(default)]
    web_session_ttl_secs: Option<u64>,
    #[serde(default)]
    web_mod_check_refresh_secs: Option<u64>,
}

/// Identify resolved AI fields whose change cannot take effect without a
/// process restart, so the dashboard can surface a "restart bot to apply"
/// hint in the flash. The rest of the AI knobs are read live via
/// `SettingsHandle` and require no restart.
fn restart_required(before: &AiSettings, after: &AiSettings) -> Vec<String> {
    let mut out = Vec::new();
    if before.connection.backend != after.connection.backend {
        out.push("ai.connection.backend".into());
    }
    if before.connection.base_url != after.connection.base_url {
        out.push("ai.connection.base_url".into());
    }
    if before.prefill.is_none() && after.prefill.is_some() {
        out.push("ai.prefill (enabling requires restart)".into());
    }
    if before.web.is_none() && after.web.is_some() {
        out.push("ai.web (enabling requires restart)".into());
    }
    if before.emotes.is_none() && after.emotes.is_some() {
        out.push("ai.emotes (enabling requires restart)".into());
    }
    if let (Some(a), Some(b)) = (&before.prefill, &after.prefill)
        && a.base_url != b.base_url
    {
        out.push("ai.prefill.base_url".into());
    }
    out
}

async fn save(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<SaveForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }

    let before = (**state.settings.load()).clone();

    // Capture submitted cooldown/ping values for validation-error re-render
    // before constructing the per-card structs (which consume the fields).
    let cooldowns_settings = Cooldowns {
        ai: form.cooldown_ai,
        news: form.cooldown_news,
        up: form.cooldown_up,
        feedback: form.cooldown_feedback,
        doener: form.cooldown_doener,
    };
    let pings_settings = PingsSettings {
        cooldown: form.ping_cooldown,
        public: form.ping_public.is_some(),
    };

    let patch = SettingsOverrides {
        schema_version: twitch_1337_core::settings::SCHEMA_VERSION,
        cooldowns: CooldownsForm {
            cooldown_ai: form.cooldown_ai,
            cooldown_news: form.cooldown_news,
            cooldown_up: form.cooldown_up,
            cooldown_feedback: form.cooldown_feedback,
            cooldown_doener: form.cooldown_doener,
        }
        .into_overrides(),
        pings: PingsForm {
            ping_cooldown: form.ping_cooldown,
            ping_public: form.ping_public,
        }
        .into_overrides(),
        ai: AiOverrides {
            connection: AiConnectionForm {
                ai_connection_backend: form.ai_connection_backend,
                ai_connection_base_url: form.ai_connection_base_url,
                ai_connection_model: form.ai_connection_model,
                ai_connection_timeout: form.ai_connection_timeout,
                ai_connection_reasoning_effort: form.ai_connection_reasoning_effort,
                ai_connection_service_tier: form.ai_connection_service_tier,
            }
            .into_overrides(),
            behavior: AiBehaviorForm {
                ai_behavior_max_turn_rounds: form.ai_behavior_max_turn_rounds,
                ai_behavior_max_writes_per_turn: form.ai_behavior_max_writes_per_turn,
                ai_behavior_persona_name: form.ai_behavior_persona_name,
            }
            .into_overrides(),
            history: AiHistoryForm {
                ai_history_length: form.ai_history_length,
                ai_history_ai_channel_length: form.ai_history_ai_channel_length,
            }
            .into_overrides(),
            memory: AiMemoryForm {
                ai_memory_soul_bytes: form.ai_memory_soul_bytes,
                ai_memory_lore_bytes: form.ai_memory_lore_bytes,
                ai_memory_user_bytes: form.ai_memory_user_bytes,
                ai_memory_state_bytes: form.ai_memory_state_bytes,
                ai_memory_inject_byte_budget: form.ai_memory_inject_byte_budget,
                ai_memory_max_state_files: form.ai_memory_max_state_files,
            }
            .into_overrides(),
            dreamer: AiDreamerForm {
                ai_dreamer_enabled: form.ai_dreamer_enabled,
                ai_dreamer_model: form.ai_dreamer_model,
                ai_dreamer_reasoning_effort: form.ai_dreamer_reasoning_effort,
                ai_dreamer_service_tier: form.ai_dreamer_service_tier,
                ai_dreamer_run_at: form.ai_dreamer_run_at,
                ai_dreamer_timeout_secs: form.ai_dreamer_timeout_secs,
                ai_dreamer_max_rounds: form.ai_dreamer_max_rounds,
            }
            .into_overrides(),
            prefill: AiPrefillForm {
                ai_prefill_card_visible: form.ai_prefill_card_visible,
                ai_prefill_enabled: form.ai_prefill_enabled,
                ai_prefill_base_url: form.ai_prefill_base_url,
                ai_prefill_threshold: form.ai_prefill_threshold,
            }
            .into_overrides(),
            web: AiWebForm {
                ai_web_card_visible: form.ai_web_card_visible,
                ai_web_enabled: form.ai_web_enabled,
                ai_web_base_url: form.ai_web_base_url,
                ai_web_timeout: form.ai_web_timeout,
                ai_web_max_results: form.ai_web_max_results,
                ai_web_max_rounds: form.ai_web_max_rounds,
                ai_web_cache_ttl_secs: form.ai_web_cache_ttl_secs,
                ai_web_cache_capacity: form.ai_web_cache_capacity,
            }
            .into_overrides(),
            emotes: AiEmotesForm {
                ai_emotes_card_visible: form.ai_emotes_card_visible,
                ai_emotes_enabled: form.ai_emotes_enabled,
                ai_emotes_include_global: form.ai_emotes_include_global,
                ai_emotes_refresh_interval_secs: form.ai_emotes_refresh_interval_secs,
                ai_emotes_max_prompt_emotes: form.ai_emotes_max_prompt_emotes,
                ai_emotes_min_baseline_emotes: form.ai_emotes_min_baseline_emotes,
                ai_emotes_base_url: form.ai_emotes_base_url,
            }
            .into_overrides(),
            media: AiMediaForm {
                ai_media_model: form.ai_media_model,
                ai_media_timeout: form.ai_media_timeout,
                ai_media_max_image_size: form.ai_media_max_image_size,
                ai_media_max_pdf_size: form.ai_media_max_pdf_size,
                ai_media_max_audio_size: form.ai_media_max_audio_size,
                ai_media_max_video_size: form.ai_media_max_video_size,
                ai_media_max_text_size: form.ai_media_max_text_size,
            }
            .into_overrides(),
        },
        twitch: {
            let mut t = TwitchPermissionsForm {
                twitch_hidden_admins: form.twitch_hidden_admins,
                twitch_viewer_allowlist: form.twitch_viewer_allowlist,
            }
            .into_overrides();
            let c = TwitchChannelsForm {
                twitch_expected_latency: form.twitch_expected_latency,
                twitch_admin_channel: form.twitch_admin_channel,
                twitch_ai_channel: form.twitch_ai_channel,
            }
            .into_overrides();
            t.expected_latency = c.expected_latency;
            t.admin_channel = c.admin_channel;
            t.ai_channel = c.ai_channel;
            t
        },
        aviationstack: AviationstackForm {
            aviationstack_card_visible: form.aviationstack_card_visible,
            aviationstack_enabled: form.aviationstack_enabled,
            aviationstack_base_url: form.aviationstack_base_url,
            aviationstack_timeout_secs: form.aviationstack_timeout_secs,
        }
        .into_overrides(),
        suspend: SuspendForm {
            suspend_default_duration_secs: form.suspend_default_duration_secs,
        }
        .into_overrides(),
        web: WebRuntimeForm {
            web_session_ttl_secs: form.web_session_ttl_secs,
            web_mod_check_refresh_secs: form.web_mod_check_refresh_secs,
        }
        .into_overrides(),
    };

    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };

    match state.settings_store.apply(patch, actor).await {
        Ok(after) => {
            let restart = restart_required(&before.ai, &after.ai);
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "settings_apply",
                result = "ok",
                restart_required = restart.len(),
            );
            let flash_msg = if restart.is_empty() {
                "Settings saved.".to_string()
            } else {
                format!(
                    "Settings saved. Restart bot to apply: {}",
                    restart.join(", ")
                )
            };
            flash::set(&cookies, &flash_msg);
            Ok(Redirect::to("/settings").into_response())
        }
        Err(SettingsError::Validation(errors)) => {
            tracing::info!(
                target: "twitch_1337_web",
                user_id = %session.user_id,
                action = "settings_apply",
                result = "validation",
                error_count = errors.len(),
            );
            // Preserve the user's submitted (raw) values so they can correct
            // the invalid field without having to retype every other input.
            // Spec §7.2: "previously entered values are preserved".
            let submitted = Settings {
                schema_version: twitch_1337_core::settings::SCHEMA_VERSION,
                cooldowns: cooldowns_settings,
                pings: pings_settings,
                // AI submitted-value preservation is left as a follow-up:
                // it requires the per-card templates (Task 19) to read echoed
                // values rather than the live `current.ai`. Until then, the
                // re-render falls back to compiled defaults for the AI section.
                ai: AiSettings::default(),
                ..Settings::compiled_defaults()
            };
            let defaults = state.settings_store.defaults().clone();
            render_with(
                axum::http::StatusCode::BAD_REQUEST,
                &ShowTpl {
                    csrf: csrf::encode(&session.csrf_value),
                    flash: None,
                    user_login: session.user_login.clone(),
                    user_avatar_url: session.avatar_url.clone(),
                    current_page: crate::nav::SETTINGS,
                    is_mod: session.is_mod(),
                    is_broadcaster: session.is_broadcaster,
                    is_owner: matches!(session.role, Role::Owner),
                    current: submitted,
                    defaults,
                    errors,
                },
            )
        }
        Err(e) => Err(WebError::Internal(eyre::eyre!("settings apply: {e}"))),
    }
}

#[derive(Deserialize)]
struct ResetForm {
    #[serde(rename = "_csrf")]
    csrf: String,
}

async fn reset(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    Path(section): Path<String>,
    axum::Form(form): axum::Form<ResetForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let section = match section.as_str() {
        "cooldowns" => SettingsSection::Cooldowns,
        "pings" => SettingsSection::Pings,
        "twitch_permissions" => SettingsSection::TwitchPermissions,
        "twitch_channels" => SettingsSection::TwitchChannels,
        "aviationstack" => SettingsSection::Aviationstack,
        "suspend" => SettingsSection::Suspend,
        "web_runtime" => SettingsSection::WebRuntime,
        other => {
            return Err(WebError::Validation {
                field: "section".into(),
                msg: format!("unknown section `{other}`"),
            });
        }
    };
    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };
    state.settings_store.reset(section, actor).await?;
    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "settings_reset",
        section = ?section,
        result = "ok",
    );
    flash::set(&cookies, "Reset to defaults.");
    Ok(Redirect::to("/settings").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tri_state_absent_is_none() {
        assert_eq!(tri_state(None), None);
    }

    #[test]
    fn tri_state_empty_clears() {
        assert_eq!(tri_state(Some(String::new())), Some(None));
    }

    #[test]
    fn tri_state_literal_none_is_not_cleared() {
        // tri_state does NOT treat the word "none" specially — it would set it.
        // Segmented selectors must go through segmented_tri_state instead.
        assert_eq!(
            tri_state(Some("none".to_string())),
            Some(Some("none".to_string()))
        );
    }

    #[test]
    fn tri_state_value_sets() {
        assert_eq!(
            tri_state(Some("gpt-4o".to_string())),
            Some(Some("gpt-4o".to_string()))
        );
    }

    #[test]
    fn segmented_tri_state_none_sentinel_clears() {
        assert_eq!(segmented_tri_state(Some("none".to_string())), Some(None));
    }

    #[test]
    fn segmented_tri_state_empty_clears() {
        assert_eq!(segmented_tri_state(Some(String::new())), Some(None));
    }

    #[test]
    fn segmented_tri_state_absent_is_none() {
        assert_eq!(segmented_tri_state(None), None);
    }

    #[test]
    fn segmented_tri_state_whitespace_only_clears() {
        // Preserve old behavior: trim before sentinel check.
        assert_eq!(segmented_tri_state(Some("   ".to_string())), Some(None));
    }

    #[test]
    fn form_parses_service_tier_flex_priority_and_none_sentinel() {
        let conn_ov = AiConnectionForm {
            ai_connection_service_tier: Some("flex".to_string()),
            ..Default::default()
        }
        .into_overrides();
        let dream_ov = AiDreamerForm {
            ai_dreamer_service_tier: Some("priority".to_string()),
            ..Default::default()
        }
        .into_overrides();
        assert_eq!(
            conn_ov.service_tier.as_ref().and_then(|v| v.as_deref()),
            Some("flex")
        );
        assert_eq!(
            dream_ov.service_tier.as_ref().and_then(|v| v.as_deref()),
            Some("priority")
        );

        let conn_ov2 = AiConnectionForm {
            ai_connection_service_tier: Some("none".to_string()),
            ..Default::default()
        }
        .into_overrides();
        let dream_ov2 = AiDreamerForm {
            ai_dreamer_service_tier: Some(String::new()),
            ..Default::default()
        }
        .into_overrides();
        assert!(matches!(conn_ov2.service_tier, Some(None)));
        assert!(matches!(dream_ov2.service_tier, Some(None)));
    }
}
