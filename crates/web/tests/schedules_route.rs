//! Integration tests for /schedules CRUD (mod-gated).

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;
use twitch_1337_core::settings::{Actor, ScheduleSettings, overrides::SettingsOverrides};
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
async fn seed_schedules(state: &twitch_1337_web::WebState, rows: Vec<ScheduleSettings>) {
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

#[tokio::test]
async fn add_schedule_persists_and_renders() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = format!(
        "_csrf={csrf}&name=noon&message=midday&interval=01%3A00&start_date=&end_date=&active_time_start=&active_time_end=&enabled=true",
        csrf = urlencoding::encode(&bare_csrf),
    );
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

#[tokio::test]
async fn edit_schedule_renames() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    seed_schedules(
        &state,
        vec![ScheduleSettings {
            name: "old".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }],
    )
    .await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = format!(
        "_csrf={csrf}&name=new&message=hi&interval=01%3A00&start_date=&end_date=&active_time_start=&active_time_end=&enabled=true",
        csrf = urlencoding::encode(&bare_csrf),
    );
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
        vec![
            ScheduleSettings {
                name: "a".into(),
                message: "hi".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
            ScheduleSettings {
                name: "b".into(),
                message: "hi".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
        ],
    )
    .await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    let body = format!(
        "_csrf={csrf}&name=a&message=hi&interval=01%3A00&start_date=&end_date=&active_time_start=&active_time_end=&enabled=true",
        csrf = urlencoding::encode(&bare_csrf),
    );
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

#[tokio::test]
async fn delete_schedule_removes_row() {
    install_crypto();
    let (state, _td_p, _td_m, _td_s) = build_state_with_all_dirs(empty_helix()).await;
    seed_schedules(
        &state,
        vec![ScheduleSettings {
            name: "gone".into(),
            message: "x".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }],
    )
    .await;
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
    seed_schedules(
        &state,
        vec![ScheduleSettings {
            name: "alpha".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }],
    )
    .await;
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
            ScheduleSettings {
                name: "keep".into(),
                message: "hi".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
            ScheduleSettings {
                name: "b".into(),
                message: "hi".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
        ],
    )
    .await;
    let (sid, csrf_cookie, bare_csrf) = insert_session(&state, "999", "mod");
    let app = build_router(state.clone());

    // Try to rename "b" to "keep" — duplicate name, validation fails.
    let body = format!(
        "_csrf={csrf}&name=keep&message=hi&interval=01%3A00&start_date=&end_date=&active_time_start=&active_time_end=&enabled=true",
        csrf = urlencoding::encode(&bare_csrf),
    );
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
