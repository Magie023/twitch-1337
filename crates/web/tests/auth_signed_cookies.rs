//! Smoke tests: tampered signed cookies are rejected by the signed extractor.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use tower_cookies::{Cookie, Cookies};
use twitch_1337_web::build_router;
use twitch_1337_web::error::is_safe_redirect;

mod helpers;
use helpers::{FakeHelix, build_state, insert_session, install_crypto, sign_for_tests};

#[tokio::test]
async fn tampered_sid_redirects_to_login() {
    install_crypto();
    let state = build_state(std::sync::Arc::new(FakeHelix {
        moderators: vec!["12345".into()],
        users: std::collections::HashMap::new(),
    }))
    .await;
    let (signed_sid, _signed_csrf, _bare_csrf) = insert_session(&state, "12345", "alice");

    // tower-cookies signs as `BASE64(hmac_tag) || original_value`. Flipping
    // a char inside the HMAC tag prefix (first 44 chars) corrupts only the
    // signature; the embedded sid stays intact. A bare cookies.get(...)
    // would still find the live sid in SessionTable — only the signed
    // extractor's HMAC verification rejects this. That's what we want to
    // pin down.
    let mut tampered = signed_sid.clone();
    // SAFETY: base64 chars are ASCII; flipping A↔B preserves UTF-8.
    let bytes = unsafe { tampered.as_bytes_mut() };
    bytes[0] = if bytes[0] == b'A' { b'B' } else { b'A' };

    let app = build_router(state);
    let req = Request::builder()
        .uri("/pings")
        .header(header::COOKIE, format!("tw1337_sid={tampered}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "tampered sid must redirect to /login (Unauthenticated)"
    );
    let location = res
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    // ?next= will be added in Task 5; for now, must at least begin with /login.
    assert!(location.starts_with("/login"), "got {location}");
}

#[tokio::test]
async fn untampered_sid_passes_through() {
    install_crypto();
    let state = build_state(std::sync::Arc::new(FakeHelix {
        moderators: vec!["12345".into()],
        users: std::collections::HashMap::new(),
    }))
    .await;
    let (signed_sid, _signed_csrf, _bare_csrf) = insert_session(&state, "12345", "alice");

    let app = build_router(state);
    let req = Request::builder()
        .uri("/pings")
        .header(header::COOKIE, format!("tw1337_sid={signed_sid}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn tampered_next_cookie_falls_back_to_root() {
    install_crypto();
    let state = build_state(std::sync::Arc::new(FakeHelix {
        moderators: vec!["12345".into()],
        users: std::collections::HashMap::new(),
    }))
    .await;

    // Same signing + tampering technique as `tampered_sid_redirects_to_login`.
    // Callback reads `tw1337_next` via the signed extractor; a corrupted HMAC
    // must look absent so the post-login redirect falls back to `/`.
    let signed_next = sign_for_tests(&state, "tw1337_next", "/memory/state/notes");
    let mut tampered = signed_next.clone();
    // SAFETY: base64 chars are ASCII; flipping A↔B preserves UTF-8.
    let bytes = unsafe { tampered.as_bytes_mut() };
    bytes[0] = if bytes[0] == b'A' { b'B' } else { b'A' };

    let cookies = Cookies::default();
    cookies.add(Cookie::build(("tw1337_next", tampered)).path("/").build());
    let next_path = cookies
        .signed(&state.signed_key)
        .get("tw1337_next")
        .map(|c| c.value().to_owned())
        .filter(|p| is_safe_redirect(p))
        .unwrap_or_else(|| "/".to_owned());
    assert_eq!(
        next_path, "/",
        "tampered next cookie must fall back to / (signed extractor rejects it)"
    );

    // Cross-site injection of an unsigned cookie must likewise fail verification.
    let cookies = Cookies::default();
    cookies.add(
        Cookie::build(("tw1337_next", "/memory/state/notes"))
            .path("/")
            .build(),
    );
    assert!(
        cookies
            .signed(&state.signed_key)
            .get("tw1337_next")
            .is_none(),
        "unsigned next cookie must not pass the signed extractor"
    );
}

#[tokio::test]
async fn auth_start_sets_signed_next_cookie() {
    install_crypto();
    let state = build_state(std::sync::Arc::new(FakeHelix {
        moderators: vec![],
        users: std::collections::HashMap::new(),
    }))
    .await;
    let app = build_router(state);

    let req = Request::builder()
        .uri("/auth/start?next=%2Fpings")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let set_cookie = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap_or(""))
        .find(|v| v.starts_with("tw1337_next="))
        .expect("auth/start must set tw1337_next when ?next= is safe");
    let value = set_cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("tw1337_next=")
        .unwrap();
    assert_ne!(value, "/pings", "next cookie must be signed, not bare path");
}
