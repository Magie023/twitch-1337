//! Integration tests for the owner-only `/settings` page.
//!
//! Drives `build_router` so the owner middleware + handler chain are
//! exercised end-to-end. The fixture seeds the session table with an
//! `owner_id` that matches the inserted Owner session so the periodic
//! role recheck inside `require_owner` admits cleanly.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_web::auth::Role;
use twitch_1337_web::build_router;

mod helpers;
use helpers::{
    FakeHelix, build_state_with_all_dirs, cookie_header, insert_session_as, install_crypto,
};

fn empty_helix() -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec![],
        users: Default::default(),
    })
}

async fn body_string(res: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn owner_can_save_cooldown_and_handle_reflects_change() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let app = build_router(state.clone());
    // Construct a fully-populated form. The save handler treats every field
    // as authoritative, so we send the current value for everything except
    // the one knob we're changing.
    let defaults = state.settings_store.defaults().clone();
    let body = format!(
        "_csrf={csrf}&cooldown_ai=15&cooldown_news={n}&cooldown_up={u}&cooldown_feedback={f}&cooldown_doener={d}&ping_cooldown={p}",
        csrf = urlencoding::encode(&bare_csrf),
        n = defaults.cooldowns.news,
        u = defaults.cooldowns.up,
        f = defaults.cooldowns.feedback,
        d = defaults.cooldowns.doener,
        p = defaults.pings.cooldown,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "owner save should redirect (303)",
    );
    assert_eq!(
        res.headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/settings"),
    );
    assert_eq!(
        state.settings.load().cooldowns.ai,
        15,
        "live handle must reflect saved cooldown",
    );
}

#[tokio::test]
async fn non_owner_get_settings_returns_403() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    // Owner is someone else; the mod session must NOT be admitted as owner.
    state.owner_id = Some(Arc::from("999"));
    let (sid, csrf_cookie, _bare) = insert_session_as(&state, "42", "modder", Role::Mod);

    let app = build_router(state);
    let req = Request::builder()
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "Mod session must not satisfy require_owner on /settings",
    );
}

#[tokio::test]
async fn validation_error_renders_form_with_errors() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let before = state.settings.load().cooldowns.ai;
    let app = build_router(state.clone());
    let defaults = state.settings_store.defaults().clone();
    // The submitted news value (900) is valid and *different* from the
    // default (60), so the re-rendered form must echo it back to verify
    // we don't discard the user's other typed input on a validation error.
    assert_ne!(
        defaults.cooldowns.news, 900,
        "fixture assumes the default news cooldown is not already 900",
    );
    // cooldown_ai = 0 violates the 1..=3600 bound.
    let body = format!(
        "_csrf={csrf}&cooldown_ai=0&cooldown_news=900&cooldown_up={u}&cooldown_feedback={f}&cooldown_doener={d}&ping_cooldown={p}",
        csrf = urlencoding::encode(&bare_csrf),
        u = defaults.cooldowns.up,
        f = defaults.cooldowns.feedback,
        d = defaults.cooldowns.doener,
        p = defaults.pings.cooldown,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "validation failure must render form with 400",
    );
    let html = body_string(res).await;
    assert!(
        html.contains("cooldowns.ai"),
        "response must surface the failing field; got: {html}"
    );
    // Submitted-value preservation: the valid sibling field (news=900) must
    // be echoed back into the form, not replaced by the stored default.
    assert!(
        html.contains("value=\"900\""),
        "submitted cooldown_news must be preserved on validation error; got: {html}"
    );
    assert!(
        !html.contains(&format!(
            "name=\"cooldown_news\" min=\"1\" max=\"3600\" value=\"{}\"",
            defaults.cooldowns.news
        )),
        "the stored default for news ({}) must NOT replace the submitted 900",
        defaults.cooldowns.news,
    );
    assert_eq!(
        state.settings.load().cooldowns.ai,
        before,
        "validation rejection must not mutate the live handle",
    );
}

/// Build a form body containing every required cooldown/ping field at their
/// default values plus any extra (form_field, value) pairs the test wants to
/// submit. Returned bodies are ready for `application/x-www-form-urlencoded`.
fn save_form_body(
    bare_csrf: &str,
    defaults: &twitch_1337_core::settings::Settings,
    extra: &[(&str, &str)],
) -> String {
    let mut body = format!(
        "_csrf={csrf}&cooldown_ai={a}&cooldown_news={n}&cooldown_up={u}&cooldown_feedback={f}&cooldown_doener={d}&ping_cooldown={p}",
        csrf = urlencoding::encode(bare_csrf),
        a = defaults.cooldowns.ai,
        n = defaults.cooldowns.news,
        u = defaults.cooldowns.up,
        f = defaults.cooldowns.feedback,
        d = defaults.cooldowns.doener,
        p = defaults.pings.cooldown,
    );
    for (k, v) in extra {
        body.push('&');
        body.push_str(k);
        body.push('=');
        body.push_str(&urlencoding::encode(v));
    }
    body
}

/// Pluck the `tw1337_flash` cookie value out of `Set-Cookie` headers.
/// Returns `None` if no flash cookie was emitted on the response.
fn read_flash(res: &axum::http::Response<Body>) -> Option<String> {
    for c in res.headers().get_all(header::SET_COOKIE) {
        let s = c.to_str().ok()?;
        if let Some(rest) = s.strip_prefix("tw1337_flash=") {
            let value = rest.split(';').next()?;
            return Some(
                urlencoding::decode(value)
                    .map(std::borrow::Cow::into_owned)
                    .unwrap_or_else(|_| value.to_owned()),
            );
        }
    }
    None
}

#[tokio::test]
async fn post_settings_applies_ai_connection_model() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let app = build_router(state.clone());
    let defaults = state.settings_store.defaults().clone();
    let body = save_form_body(&bare_csrf, &defaults, &[("ai_connection_model", "gpt-5")]);
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "AI model save should redirect (303)",
    );
    // The connection model is read live via the settings handle — no restart.
    let flash = read_flash(&res).expect("save must emit a flash cookie");
    assert!(
        flash.starts_with("Settings saved."),
        "flash should start with the saved confirmation; got {flash:?}",
    );
    assert!(
        !flash.contains("Restart bot"),
        "model change is live; restart hint must NOT appear; got {flash:?}",
    );
    assert_eq!(
        state.settings.load().ai.connection.model,
        "gpt-5",
        "live handle must reflect saved AI model",
    );
}

#[tokio::test]
async fn post_settings_flags_backend_restart_required() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let app = build_router(state.clone());
    let defaults = state.settings_store.defaults().clone();
    let body = save_form_body(
        &bare_csrf,
        &defaults,
        &[("ai_connection_backend", "ollama")],
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let flash = read_flash(&res).expect("save must emit a flash cookie");
    assert!(
        flash.contains("Restart bot to apply"),
        "backend swap must surface the restart hint; got {flash:?}",
    );
    assert!(
        flash.contains("ai.connection.backend"),
        "restart hint must name the field; got {flash:?}",
    );
    assert!(
        matches!(
            state.settings.load().ai.connection.backend,
            twitch_1337_core::settings::AiBackendKind::Ollama,
        ),
        "the value still applies — the restart hint is advisory",
    );
}

#[tokio::test]
async fn post_settings_enabling_prefill_card_flags_restart() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    // Prefill is disabled by default; rendering the toggle card and ticking
    // the box enables it — that transition requires a restart.
    assert!(
        state.settings.load().ai.prefill.is_none(),
        "fixture assumes prefill defaults to disabled",
    );

    let app = build_router(state.clone());
    let defaults = state.settings_store.defaults().clone();
    let body = save_form_body(
        &bare_csrf,
        &defaults,
        &[
            ("ai_prefill_card_visible", "1"),
            ("ai_prefill_enabled", "1"),
            ("ai_prefill_base_url", "https://logs.zonian.dev"),
            ("ai_prefill_threshold", "0.5"),
        ],
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let flash = read_flash(&res).expect("save must emit a flash cookie");
    assert!(
        flash.contains("ai.prefill"),
        "enabling prefill must mention the field in the restart hint; got {flash:?}",
    );
    assert!(
        state.settings.load().ai.prefill.is_some(),
        "prefill must be enabled after apply",
    );
}

#[tokio::test]
async fn post_settings_card_invisible_leaves_toggle_alone() {
    // Regression for the *_card_visible hidden-input pattern: a save that
    // does not render a toggle card must NOT inadvertently disable it.
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    // First, enable prefill via a "card visible" save so subsequent saves
    // without the hidden input must preserve that state.
    let defaults = state.settings_store.defaults().clone();
    let app = build_router(state.clone());
    let body = save_form_body(
        &bare_csrf,
        &defaults,
        &[
            ("ai_prefill_card_visible", "1"),
            ("ai_prefill_enabled", "1"),
            ("ai_prefill_base_url", "https://logs.zonian.dev"),
            ("ai_prefill_threshold", "0.5"),
        ],
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert!(state.settings.load().ai.prefill.is_some());

    // Now save again without rendering the card at all — prefill must stay on.
    let app = build_router(state.clone());
    let body = save_form_body(&bare_csrf, &defaults, &[]);
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert!(
        state.settings.load().ai.prefill.is_some(),
        "a save without the card_visible marker must NOT clear an enabled card",
    );
}

#[tokio::test]
async fn settings_page_renders_all_ai_cards() {
    // Smoke test: GETting /settings as the owner must render every AI card
    // we wired into `index.html`. Catches missing/typo'd `id="…"` anchors
    // before they break the sidebar nav and the dirty-counters JS.
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, _bare) = insert_session_as(&state, "123", "owner", Role::Owner);

    let app = build_router(state);
    let req = Request::builder()
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "owner GET /settings must 200");
    let html = body_string(res).await;
    for id in [
        "sec-ai-connection",
        "sec-ai-behavior",
        "sec-ai-history",
        "sec-ai-memory",
        "sec-ai-dreamer",
        "sec-ai-prefill",
        "sec-ai-web",
        "sec-ai-emotes",
        "sec-ai-media",
    ] {
        assert!(
            html.contains(&format!("id=\"{id}\"")),
            "settings page is missing AI card anchor {id}"
        );
    }
}

#[tokio::test]
async fn settings_page_shows_openrouter_tiers_for_default_base_url() {
    // The OpenAI-compatible client defaults an empty base URL to OpenRouter.
    // The dashboard must expose OpenRouter-only controls for that effective
    // runtime configuration, not only after an explicit URL override.
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, _bare) = insert_session_as(&state, "123", "owner", Role::Owner);

    assert!(
        state.settings.load().ai.connection.base_url.is_none(),
        "fixture must exercise the provider-default base URL path"
    );

    let app = build_router(state);
    let req = Request::builder()
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "owner GET /settings must 200");
    let html = body_string(res).await;

    assert!(
        html.contains("name=\"ai_connection_service_tier\""),
        "connection service_tier control must render for default OpenRouter base URL"
    );
    assert!(
        html.contains("name=\"ai_dreamer_service_tier\""),
        "dreamer service_tier control must render for default OpenRouter base URL"
    );
}

#[tokio::test]
async fn reset_cooldowns_clears_section_overrides() {
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let defaults = state.settings_store.defaults().clone();
    // First push a non-default value via /settings POST.
    let app = build_router(state.clone());
    let body = format!(
        "_csrf={csrf}&cooldown_ai=15&cooldown_news={n}&cooldown_up={u}&cooldown_feedback={f}&cooldown_doener={d}&ping_cooldown={p}",
        csrf = urlencoding::encode(&bare_csrf),
        n = defaults.cooldowns.news,
        u = defaults.cooldowns.up,
        f = defaults.cooldowns.feedback,
        d = defaults.cooldowns.doener,
        p = defaults.pings.cooldown,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(state.settings.load().cooldowns.ai, 15);

    // Then reset just the cooldowns section.
    let app = build_router(state.clone());
    let body = format!("_csrf={csrf}", csrf = urlencoding::encode(&bare_csrf));
    let req = Request::builder()
        .method("POST")
        .uri("/settings/reset/cooldowns")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "reset should redirect on success",
    );
    assert_eq!(
        state.settings.load().cooldowns.ai,
        defaults.cooldowns.ai,
        "reset must restore the compiled default",
    );
}

#[tokio::test]
async fn dreamer_checkbox_absent_sets_enabled_false() {
    // The dreamer card always renders (no *_card_visible pattern), so
    // omitting ai_dreamer_enabled from the POST means "disabled".
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let app = build_router(state.clone());
    let defaults = state.settings_store.defaults().clone();
    // Send the form without ai_dreamer_enabled.
    let body = save_form_body(&bare_csrf, &defaults, &[]);
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert!(
        !state.settings.load().ai.dreamer.enabled,
        "missing ai_dreamer_enabled checkbox must set dreamer.enabled = false",
    );
}

#[tokio::test]
async fn emotes_card_include_global_absent_without_card_visible() {
    // When ai_emotes_card_visible is not submitted (card not rendered),
    // include_global override must be None — i.e. no change to the existing value.
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    // First, enable emotes so there's an existing include_global value to preserve.
    let defaults = state.settings_store.defaults().clone();
    let app = build_router(state.clone());
    let body = save_form_body(
        &bare_csrf,
        &defaults,
        &[
            ("ai_emotes_card_visible", "1"),
            ("ai_emotes_enabled", "1"),
            ("ai_emotes_include_global", "1"),
        ],
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let include_global_before = state
        .settings
        .load()
        .ai
        .emotes
        .as_ref()
        .map(|e| e.include_global);
    assert_eq!(include_global_before, Some(true));

    // Now submit a form without the card_visible marker: include_global must stay.
    let app = build_router(state.clone());
    let body = save_form_body(&bare_csrf, &defaults, &[]);
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let include_global_after = state
        .settings
        .load()
        .ai
        .emotes
        .as_ref()
        .map(|e| e.include_global);
    assert_eq!(
        include_global_after,
        Some(true),
        "include_global must not change when card_visible was absent",
    );
}

#[tokio::test]
async fn media_size_malformed_string_is_ignored() {
    // A malformed byte-size string (e.g. "not-a-size") must not crash the
    // handler; the field parses to None so the previous value is preserved.
    install_crypto();
    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    state.owner_id = Some(Arc::from("123"));
    let (sid, csrf_cookie, bare_csrf) = insert_session_as(&state, "123", "owner", Role::Owner);

    let defaults = state.settings_store.defaults().clone();
    let app = build_router(state.clone());
    // Submit a valid save first so we have a known max_image_size.
    let body = save_form_body(
        &bare_csrf,
        &defaults,
        &[("ai_media_max_image_size", "5 MB")],
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    // Now submit a malformed size: handler must succeed (not 500).
    let app = build_router(state.clone());
    let body = save_form_body(
        &bare_csrf,
        &defaults,
        &[("ai_media_max_image_size", "not-a-size")],
    );
    let req = Request::builder()
        .method("POST")
        .uri("/settings")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert!(
        res.status().is_redirection() || res.status().is_client_error(),
        "malformed size must not cause a 500; got {}",
        res.status(),
    );
}
