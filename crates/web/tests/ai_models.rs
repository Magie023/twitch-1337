//! Integration tests for `GET /settings/ai/models`.
//!
//! Each test spins up a wiremock upstream, builds a `WebState` with the
//! matching AI bootstrap / connection settings, and drives `build_router`
//! end-to-end so the owner middleware + cache are exercised.

mod helpers;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use secrecy::SecretString;
use tower::ServiceExt as _;
use twitch_1337_core::config::AiBootstrap;
use twitch_1337_core::settings::AiBackendKind;
use twitch_1337_core::settings::overrides::{
    AiConnectionOverrides, AiOverrides, SettingsOverrides,
};
use twitch_1337_web::auth::Role;
use twitch_1337_web::build_router;
use twitch_1337_web::routes::ai_models::ModelListCache;
use wiremock::matchers::{header as wm_header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use helpers::{
    FakeHelix, build_state_with_all_dirs, cookie_header, insert_session_as, install_crypto,
    set_owner,
};

fn empty_helix() -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec![],
        users: Default::default(),
    })
}

/// Build a request body as JSON for the given path, authenticated as the owner.
async fn owner_get(state: &twitch_1337_web::WebState, uri: &str) -> axum::http::Response<Body> {
    let (sid, csrf, _bare) = insert_session_as(state, "1", "owner", Role::Owner);
    let app = build_router(state.clone());
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .header(header::COOKIE, cookie_header(&sid, &csrf))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn body_json(res: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).expect("response must be valid JSON")
}

/// Apply AI connection overrides (backend + base_url) and optionally set the
/// bootstrap api_key on the state. Returns the modified state.
async fn configure_state(
    state: &mut twitch_1337_web::WebState,
    backend: AiBackendKind,
    base_url: &str,
    api_key: &str,
) {
    let patch = SettingsOverrides {
        ai: AiOverrides {
            connection: AiConnectionOverrides {
                backend: Some(backend),
                base_url: Some(Some(base_url.to_owned())),
                ..AiConnectionOverrides::default()
            },
            ..AiOverrides::default()
        },
        ..SettingsOverrides::default()
    };
    let actor = twitch_1337_core::settings::Actor {
        user_id: "test".into(),
        user_login: "test".into(),
    };
    state
        .settings_store
        .apply(patch, actor)
        .await
        .expect("apply AI settings override");

    state.ai_bootstrap = Some(Arc::new(AiBootstrap {
        api_key: SecretString::new(api_key.to_owned().into()),
    }));
    // Fresh cache per test so tests are isolated.
    state.model_cache = Arc::new(ModelListCache::default());
    set_owner(state, Some("1"));
}

#[tokio::test]
async fn openai_models_proxy_returns_normalized_list() {
    install_crypto();
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(wm_header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-5", "object": "model"},
                {"id": "gpt-5-mini", "object": "model"},
            ],
        })))
        .mount(&upstream)
        .await;

    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    configure_state(
        &mut state,
        AiBackendKind::OpenAi,
        &upstream.uri(),
        "test-key",
    )
    .await;

    let res = owner_get(&state, "/settings/ai/models?scope=connection").await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_json(res).await;
    let ids: Vec<&str> = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["gpt-5", "gpt-5-mini"]);
    assert!(body["error"].is_null(), "error must be null on success");
}

#[tokio::test]
async fn ollama_models_proxy_returns_normalized_list() {
    install_crypto();
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "models": [
                {"name": "gemma3:4b"},
                {"name": "llama3.2:3b"},
            ],
        })))
        .mount(&upstream)
        .await;

    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    configure_state(&mut state, AiBackendKind::Ollama, &upstream.uri(), "").await;

    let res = owner_get(&state, "/settings/ai/models?scope=connection").await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_json(res).await;
    let ids: Vec<&str> = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["gemma3:4b", "llama3.2:3b"]);
    assert!(body["error"].is_null());
}

#[tokio::test]
async fn upstream_failure_returns_empty_list_and_error() {
    install_crypto();
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&upstream)
        .await;

    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    configure_state(&mut state, AiBackendKind::OpenAi, &upstream.uri(), "k").await;

    let res = owner_get(&state, "/settings/ai/models?scope=connection").await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_json(res).await;
    assert!(
        body["models"].as_array().unwrap().is_empty(),
        "models must be empty on upstream failure",
    );
    assert!(
        body["error"].is_string(),
        "error must be a string on upstream failure",
    );
}

#[tokio::test]
async fn cache_hit_skips_second_upstream_call() {
    install_crypto();
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
        .expect(1)
        .mount(&upstream)
        .await;

    let (mut state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    configure_state(&mut state, AiBackendKind::OpenAi, &upstream.uri(), "k").await;

    let _ = owner_get(&state, "/settings/ai/models?scope=connection").await;
    let _ = owner_get(&state, "/settings/ai/models?scope=connection").await;
    // wiremock asserts .expect(1) on Drop — if the upstream was hit twice, the
    // test binary panics after this point.
}

#[tokio::test]
async fn non_owner_is_rejected() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    set_owner(&state, Some("999"));

    let (sid, csrf, _bare) = insert_session_as(&state, "1", "someone", Role::Mod);
    let app = build_router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/settings/ai/models")
                .header(header::COOKIE, cookie_header(&sid, &csrf))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}
