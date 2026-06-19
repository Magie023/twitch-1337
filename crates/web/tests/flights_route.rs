//! Integration tests for `/flights` read-only route.
//!
//! The route is not yet mounted in `build_router` (Task 14 handles that),
//! so we assemble a minimal `Router<WebState>` locally that merges
//! `routes::flights::router()` behind a `require_mod` layer. This keeps
//! the test self-contained and independent of Task 14.

mod helpers;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use helpers::{FakeHelix, build_state_with_dirs, cookie_header, insert_session, install_crypto};
use tokio::sync::mpsc;
use tower::ServiceExt as _;
use twitch_1337_core::aviation::tracker::{TrackedFlightView, TrackerCommand};
use twitch_1337_web::auth::require_mod;
use twitch_1337_web::routes::flights;

fn mod_helix() -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec!["42".into()],
        users: Default::default(),
    })
}

fn app(state: twitch_1337_web::WebState) -> Router {
    Router::new()
        .merge(flights::viewer_router())
        .merge(flights::mod_router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_mod,
        ))
        .with_state(state)
        .layer(tower_cookies::CookieManagerLayer::new())
}

async fn body_string(res: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(res.into_body(), 128 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn flights_empty_state_when_no_tracker() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;
    // tracker_tx is None by default in test helpers
    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/flights")
        .method(Method::GET)
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("disabled"),
        "should show aviation-disabled placeholder; got {html}"
    );
}

#[tokio::test]
async fn flights_empty_state_when_tracker_returns_no_flights() {
    install_crypto();
    let (mut state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;
    let (tx, mut rx) = mpsc::channel::<TrackerCommand>(8);
    state.tracker_tx = Some(Arc::new(tx));

    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if let TrackerCommand::Snapshot { reply } = cmd {
                let _ = reply.send(Vec::new());
                break;
            }
        }
    });

    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/flights")
        .method(Method::GET)
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("No flights are being tracked right now"),
        "should show empty tracked-flights placeholder; got {html}"
    );
}

#[tokio::test]
async fn flights_renders_snapshot_from_tracker() {
    install_crypto();
    let (mut state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;
    let (tx, mut rx) = mpsc::channel::<TrackerCommand>(8);
    state.tracker_tx = Some(Arc::new(tx));

    // Spawn a tiny task that answers the next Snapshot command.
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if let TrackerCommand::Snapshot { reply } = cmd {
                let _ = reply.send(vec![TrackedFlightView {
                    identifier: "DLH123".into(),
                    callsign: Some("DLH123".into()),
                    hex: Some("3C65A1".into()),
                    owner_login: "alice".into(),
                    phase: "Cruise".into(),
                    route: Some("FRA → JFK".into()),
                    target_confirmation: "ConfirmedByCallsign".into(),
                    hex_source: Some("Adsb".into()),
                    altitude_ft: Some(34000),
                    ground_speed_kts: Some(450.0),
                    last_seen_secs_ago: Some(12),
                }]);
                break;
            }
        }
    });

    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/flights")
        .method(Method::GET)
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("DLH123"),
        "should render callsign; got {html}"
    );
    assert!(
        html.contains("alice"),
        "should render owner login; got {html}"
    );
    assert!(html.contains("3C65A1"), "should render hex; got {html}");
    assert!(
        html.contains("FRA → JFK"),
        "should render route; got {html}"
    );
    assert!(
        html.contains("ConfirmedByCallsign"),
        "should render target confirmation; got {html}"
    );
    assert!(
        html.contains("Adsb"),
        "should render hex source; got {html}"
    );
}

#[tokio::test]
async fn flights_shows_busy_placeholder_when_snapshot_times_out() {
    install_crypto();
    let (mut state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;
    let (tx, _rx) = mpsc::channel::<TrackerCommand>(8);
    state.tracker_tx = Some(Arc::new(tx));

    let (sid, csrf, _bare) = insert_session(&state, "42", "admin");
    let req = Request::builder()
        .uri("/flights")
        .method(Method::GET)
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .body(Body::empty())
        .unwrap();
    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body_string(res).await;
    assert!(
        html.contains("snapshot timed out"),
        "should distinguish timeout from empty list; got {html}"
    );
    assert!(
        !html.contains("No flights are being tracked right now"),
        "timeout must not masquerade as empty list; got {html}"
    );
}

#[tokio::test]
async fn flights_unauthenticated_redirects() {
    install_crypto();
    let (state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;

    let req = Request::builder()
        .uri("/flights")
        .body(Body::empty())
        .unwrap();

    let res = app(state).oneshot(req).await.unwrap();
    assert!(
        res.status() == StatusCode::SEE_OTHER || res.status() == StatusCode::UNAUTHORIZED,
        "unauthenticated request must not get 200; got {}",
        res.status()
    );
}

#[tokio::test]
async fn flights_delete_posts_tracker_command_and_redirects_with_flash() {
    install_crypto();
    let (mut state, _td_pings, _td_mem) = build_state_with_dirs(mod_helix()).await;
    let (tx, mut rx) = mpsc::channel::<TrackerCommand>(8);
    state.tracker_tx = Some(Arc::new(tx));

    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if let TrackerCommand::DeleteFromWeb { identifier, reply } = cmd {
                assert_eq!(identifier, "DLH123");
                let _ = reply.send(Some("DLH123".to_owned()));
                break;
            }
        }
    });

    let (sid, csrf, bare_csrf) = insert_session(&state, "42", "admin");
    let body = format!(
        "_csrf={csrf}&identifier=DLH123",
        csrf = urlencoding::encode(&bare_csrf)
    );
    let req = Request::builder()
        .uri("/flights/delete")
        .method(Method::POST)
        .header(header::COOKIE, cookie_header(&sid, &csrf))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app(state).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers().get(header::LOCATION).unwrap(), "/flights");
    let set_cookie = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        set_cookie.contains("tw1337_flash"),
        "delete should set a flash cookie; got {set_cookie}"
    );
}
