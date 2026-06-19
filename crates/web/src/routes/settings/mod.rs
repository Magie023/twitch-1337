//! Owner-only settings page: live cooldowns + pings runtime knobs.
//!
//! The page reads `state.settings` for the current effective values and
//! `state.settings_store.defaults()` to show the compile-time fallbacks
//! beside each input. Saves go through `SettingsStore::apply`, which
//! validates, atomically persists, swaps the shared handle, and records an
//! audit entry. Reset clears one section (`cooldowns` or `pings`) back to
//! its defaults.
//!
//! Form deserialization + field conversion live in `forms`; the page-render
//! template lives in `view`. This module keeps the router and the apply
//! handlers (`save`, `reset`).

mod forms;
mod view;

use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::settings::{
    Actor, AiSettings, Cooldowns, PingsSettings, Settings, SettingsError, SettingsSection,
};

use crate::auth::Role;
use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::routes::render_with;
use crate::state::WebState;
use view::{ShowTpl, show};

pub fn owner_router() -> Router<WebState> {
    Router::new()
        .route("/settings", get(show).post(save))
        .route("/settings/reset/{section}", post(reset))
}

// ---------------------------------------------------------------------------
// Top-level form — flat serde struct (serde_urlencoded does not support
// #[serde(flatten)]; each field lives at the top level, then `forms` bundles
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
    ai_emotes_pinned_emotes: Option<String>,
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
    // before handing the form to the patch builder (which consumes it).
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

    let patch = forms::overrides_from_save_form(form);

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
