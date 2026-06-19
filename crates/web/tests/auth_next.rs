//! Post-login `?next=` deep-link tests.
//!
//! Covers:
//! - `is_safe_redirect` validator semantics (scheme/host/CRLF/length).
//! - `require_mod` capturing the requested path into `?next=` on
//!   `WebError::Unauthenticated`.
//! - `/login` silently dropping unsafe `?next=` values rather than
//!   stashing them in the `tw1337_next` cookie.
//! - OAuth callback consuming a signed `tw1337_next` cookie and clearing it
//!   after redirect (wiremock round-trip).

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt as _;
use twitch_1337_web::build_router;
use twitch_1337_web::error::is_safe_redirect;
use twitch_1337_web::helix::HelixClient;
use wiremock::matchers::{header as wm_header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod helpers;
use helpers::{
    FakeHelix, build_state, build_state_with_oauth_upstream, install_crypto, sign_for_tests,
};

fn fake_helix() -> Arc<dyn HelixClient> {
    Arc::new(FakeHelix {
        moderators: vec![],
        users: HashMap::new(),
    })
}

#[test]
fn safe_redirect_rejects_scheme_and_host() {
    assert!(is_safe_redirect("/pings"));
    assert!(is_safe_redirect("/memory/state/notes"));
    assert!(!is_safe_redirect("//evil.example/x"));
    assert!(!is_safe_redirect("https://evil.example/"));
    assert!(!is_safe_redirect("javascript:alert(1)"));
    assert!(!is_safe_redirect("/path\r\nSet-Cookie: x=1"));
    assert!(!is_safe_redirect("/\\evil.example/x"));
    assert!(!is_safe_redirect("/foo\\bar"));
    assert!(!is_safe_redirect(&"/".repeat(257)));
}

#[tokio::test]
async fn unauth_request_redirects_to_login_with_next() {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);

    let req = Request::builder()
        .uri("/memory/state/notes")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(
        location, "/login?next=%2Fmemory%2Fstate%2Fnotes",
        "expected exact encoded next path, got {location}"
    );
}

#[tokio::test]
async fn login_with_unsafe_next_drops_it_silently() {
    install_crypto();
    let state = build_state(fake_helix()).await;
    let app = build_router(state);

    let req = Request::builder()
        .uri("/login?next=https://evil.example/x")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    // Login still redirects to twitch authorize; the unsafe next must NOT
    // appear as a Set-Cookie value.
    let set_cookie = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !set_cookie.contains("tw1337_next="),
        "unsafe next must not be stashed; saw: {set_cookie}"
    );
}

const TEST_USER_ID: &str = "12345";
const TEST_OAUTH_STATE: &str = "test-oauth-state";
const TEST_AUTH_CODE: &str = "test-auth-code";
const TEST_ACCESS_TOKEN: &str = "test-access-token";
const NEXT_PATH: &str = "/memory/state/notes";

async fn mount_oauth_callback_stubs(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": TEST_ACCESS_TOKEN,
            "token_type": "bearer",
            "scope": ["user:read:email", "user:read:moderated_channels"],
        })))
        .expect(1)
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/helix/users"))
        .and(wm_header(
            "authorization",
            format!("Bearer {TEST_ACCESS_TOKEN}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "id": TEST_USER_ID,
                "login": "alice",
                "display_name": "Alice",
            }],
        })))
        .expect(1)
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path("/helix/moderation/channels"))
        .and(query_param("user_id", TEST_USER_ID))
        .and(query_param("first", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "broadcaster_id": "100",
                "broadcaster_login": "testchannel",
                "broadcaster_name": "testchannel",
            }],
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn callback_consumes_signed_next_and_clears_cookie() {
    install_crypto();
    let upstream = MockServer::start().await;
    mount_oauth_callback_stubs(&upstream).await;

    let state = build_state_with_oauth_upstream(fake_helix(), &upstream.uri()).await;
    let signed_next = sign_for_tests(&state, "tw1337_next", NEXT_PATH);
    let app = build_router(state);

    let req = Request::builder()
        .uri(format!(
            "/auth/callback?code={TEST_AUTH_CODE}&state={TEST_OAUTH_STATE}"
        ))
        .header(
            header::COOKIE,
            format!("tw1337_oauth_state={TEST_OAUTH_STATE}; tw1337_next={signed_next}"),
        )
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(
        location, NEXT_PATH,
        "callback must redirect to stashed next path"
    );

    let set_cookies: Vec<_> = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap_or(""))
        .collect();
    let next_removal = set_cookies
        .iter()
        .find(|c| c.starts_with("tw1337_next="))
        .expect("callback must clear tw1337_next via Set-Cookie");
    assert!(
        next_removal.contains("Max-Age=0"),
        "tw1337_next removal must expire the cookie; saw: {next_removal}"
    );
}
