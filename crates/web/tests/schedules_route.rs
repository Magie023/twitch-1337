//! Integration tests for /schedules CRUD (mod-gated).

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_core::schedule::{Schedule, Trigger, WeekdaySet};
use twitch_1337_core::settings::{Actor, overrides::SettingsOverrides};
use twitch_1337_web::build_router;

mod helpers;
use helpers::{
    FakeHelix, build_state_with_all_dirs, cookie_header, insert_session, install_crypto,
};

fn empty_helix() -> Arc<FakeHelix> {
    Arc::new(FakeHelix {
        moderators: vec!["999".into()],
        users: Default::default(),
    })
}

async fn body_string(res: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Apply schedules via the store so we can seed pre-test state without
/// going through the dashboard.
async fn seed_schedules(state: &twitch_1337_web::WebState, rows: Vec<Schedule>) {
    state
        .settings_store
        .apply(
            SettingsOverrides {
                schedules: Some(rows),
                ..Default::default()
            },
            Actor {
                user_id: "test".into(),
                user_login: "test".into(),
            },
        )
        .await
        .expect("seed schedules");
}

fn interval_schedule(name: &str, message: &str) -> Schedule {
    Schedule {
        name: name.into(),
        message: message.into(),
        trigger: Trigger::Interval {
            every: Duration::from_secs(3600),
            days: WeekdaySet::default(),
            active_from: None,
            active_to: None,
        },
        start_date: None,
        end_date: None,
        enabled: true,
    }
}

/// Build a URL-encoded form body for a new/updated interval schedule.
fn interval_form(csrf: &str, name: &str, message: &str) -> String {
    format!(
        "_csrf={csrf}&name={name}&message={message}&kind=interval&interval_every=01%3A00&interval_active_from=&interval_active_to=&start_date=&end_date=&enabled=true",
        csrf = urlencoding::encode(csrf),
    )
}

#[tokio::test]
async fn add_schedule_persists_and_renders() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = interval_form(&bare_csrf, "noon", "midday");
    let req = Request::builder()
        .method("POST")
        .uri("/schedules/add")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let listed = {
        let req = Request::builder()
            .uri("/schedules")
            .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        body_string(res).await
    };
    assert!(listed.contains("noon"), "row must appear in list");
    let v = state.settings.load().schedules.clone();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].name, "noon");
}

/// A calendar schedule with exactly ONE weekday selected sends a single
/// `calendar_days=Thu` field. Plain `axum::Form` (serde_urlencoded) rejected
/// that as "invalid type: string, expected a sequence"; the `axum_extra` Form
/// extractor parses a lone occurrence into a 1-element Vec.
#[tokio::test]
async fn add_calendar_schedule_with_single_weekday() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = format!(
        "_csrf={csrf}&name=thurs&message=hi&kind=calendar&calendar_at=13%3A37&calendar_days=Thu&start_date=&end_date=&enabled=true",
        csrf = urlencoding::encode(&bare_csrf),
    );
    let req = Request::builder()
        .method("POST")
        .uri("/schedules/add")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let v = state.settings.load().schedules.clone();
    assert_eq!(v.len(), 1);
    match &v[0].trigger {
        Trigger::Calendar { days, .. } => {
            assert!(days.contains(chrono::Weekday::Thu), "Thu must be set");
            assert!(!days.contains(chrono::Weekday::Mon), "only Thu selected");
        }
        other => panic!("expected calendar trigger, got {other:?}"),
    }
}

#[tokio::test]
async fn edit_schedule_renames() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    seed_schedules(&state, vec![interval_schedule("old", "hi")]).await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = interval_form(&bare_csrf, "new", "hi");
    let req = Request::builder()
        .method("POST")
        .uri("/schedules/old/edit")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let v = state.settings.load().schedules.clone();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].name, "new");
}

#[tokio::test]
async fn edit_duplicate_name_returns_error_without_persisting() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    seed_schedules(
        &state,
        vec![interval_schedule("a", "hi"), interval_schedule("b", "hi")],
    )
    .await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = interval_form(&bare_csrf, "a", "hi");
    let req = Request::builder()
        .method("POST")
        .uri("/schedules/b/edit")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    // Validation error renders the list with errors; status is 200, not 303.
    assert_eq!(res.status().as_u16(), 200);
    let v = state.settings.load().schedules.clone();
    assert_eq!(v.len(), 2, "no row may be renamed on validation failure");
    assert!(v.iter().any(|s| s.name == "a"));
    assert!(v.iter().any(|s| s.name == "b"));
}

/// A create that fails store-level validation (duplicate name) must restore
/// the editable form with the user's input — NOT render an interactive card
/// for the rejected, never-persisted row (clicking such a card targeted a
/// nonexistent schedule and made the whole thing vanish).
#[tokio::test]
async fn create_duplicate_name_restores_form_without_phantom_card() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    seed_schedules(&state, vec![interval_schedule("dup", "original")]).await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = interval_form(&bare_csrf, "dup", "keepme");
    let req = Request::builder()
        .method("POST")
        .uri("/schedules/add")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status().as_u16(),
        200,
        "validation error re-renders, not 303"
    );

    let v = state.settings.load().schedules.clone();
    assert_eq!(v.len(), 1, "rejected row must not persist");

    let html = body_string(res).await;
    // Form restored with the submitted message so the user can fix the name.
    assert!(
        html.contains("action=\"/schedules/add\""),
        "new-schedule form must be restored"
    );
    assert!(html.contains("keepme"), "submitted input must be preserved");
    // Exactly one card for "dup" — the persisted one. A phantom card for the
    // rejected attempt would push this to two.
    assert_eq!(
        html.matches("/schedules/dup/toggle").count(),
        1,
        "no phantom card for the unsaved row"
    );
    // The error still surfaces, but attributed to the form (global), not a
    // phantom "row 1" that exceeds the single persisted card.
    assert!(html.contains("Validation failed"), "error must be shown");
    assert!(
        !html.contains("row 1"),
        "no phantom row reference for the unsaved attempt"
    );
}

#[tokio::test]
async fn delete_schedule_removes_row() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    seed_schedules(&state, vec![interval_schedule("gone", "x")]).await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = format!("_csrf={csrf}", csrf = urlencoding::encode(&bare_csrf));
    let req = Request::builder()
        .method("POST")
        .uri("/schedules/gone/delete")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let v = state.settings.load().schedules.clone();
    assert!(v.is_empty());
}

#[tokio::test]
async fn edit_query_param_renders_inline_form() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    seed_schedules(&state, vec![interval_schedule("alpha", "hi")]).await;
    let (sid, csrf_cookie, _bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let req = Request::builder()
        .uri("/schedules?edit=alpha")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let body = body_string(res).await;
    assert!(
        body.contains("/schedules/alpha/edit"),
        "edit form action must be present for the matching row; body was: {body}"
    );
}

#[tokio::test]
async fn edit_validation_failure_reopens_form_with_new_name() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    seed_schedules(
        &state,
        vec![
            interval_schedule("keep", "hi"),
            interval_schedule("b", "hi"),
        ],
    )
    .await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    // Try to rename "b" to "keep" — duplicate name, validation fails.
    let body = interval_form(&bare_csrf, "keep", "hi");
    let req = Request::builder()
        .method("POST")
        .uri("/schedules/b/edit")
        .header(header::COOKIE, cookie_header(&sid, &csrf_cookie))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let body = body_string(res).await;
    // The inline edit form should reopen for the NEW name ("keep"), not the
    // URL path's old name ("b"). The rendered template iterates the in-memory
    // `next` vec — `b` has been replaced with `keep` (validation failure does
    // not roll the vec back since it's a local).
    assert!(
        body.contains("/schedules/keep/edit"),
        "edit form for new name 'keep' must reopen on validation failure"
    );
}
