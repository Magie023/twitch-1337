//! `/flights` — read-only live snapshot of currently tracked aircraft.

use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio::sync::oneshot;
use tower_cookies::Cookies;
use twitch_1337_core::aviation::tracker::{TrackedFlightView, TrackerCommand};

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::nav;
use crate::routes::render;
use crate::state::WebState;

#[derive(Template)]
#[template(path = "flights/list.html")]
struct ListTpl {
    flights: Vec<TrackedFlightView>,
    aviation_disabled: bool,
    tracker_busy: bool,
    user_login: String,
    user_avatar_url: Option<String>,
    csrf: String,
    flash: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
}

pub fn viewer_router() -> Router<WebState> {
    Router::new().route("/flights", get(list))
}

pub fn mod_router() -> Router<WebState> {
    Router::new().route("/flights/delete", post(delete))
}

const TRACKER_TIMEOUT: Duration = Duration::from_millis(500);

enum TrackerResult<R> {
    Ok(R),
    Timeout,
    Unavailable,
}

/// Round-trips one tracker command and returns the reply, or a status when
/// aviation is disabled, the channel is closed, or the reply times out.
async fn tracker_request<R>(
    state: &WebState,
    build_cmd: impl FnOnce(oneshot::Sender<R>) -> TrackerCommand,
) -> TrackerResult<R> {
    let Some(tx) = state.tracker_tx.as_ref() else {
        return TrackerResult::Unavailable;
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    if tx.send(build_cmd(reply_tx)).await.is_err() {
        return TrackerResult::Unavailable;
    }
    match tokio::time::timeout(TRACKER_TIMEOUT, reply_rx).await {
        Ok(Ok(value)) => TrackerResult::Ok(value),
        Ok(Err(_)) => TrackerResult::Unavailable,
        Err(_) => TrackerResult::Timeout,
    }
}

async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let aviation_disabled = state.tracker_tx.is_none();
    let (flights, tracker_busy) =
        match tracker_request(&state, |reply| TrackerCommand::Snapshot { reply }).await {
            TrackerResult::Ok(flights) => (flights, false),
            TrackerResult::Timeout => (Vec::new(), true),
            TrackerResult::Unavailable => (Vec::new(), false),
        };
    let csrf = csrf::encode(&session.csrf_value);
    let is_mod = session.is_mod();
    let is_owner = matches!(session.role, crate::auth::Role::Owner);
    render(&ListTpl {
        flights,
        aviation_disabled,
        tracker_busy,
        user_avatar_url: session.avatar_url.clone(),
        user_login: session.user_login,
        csrf,
        flash: flash::take(&cookies),
        current_page: nav::FLIGHTS,
        is_mod,
        is_broadcaster: session.is_broadcaster,
        is_owner,
    })
}

#[derive(Deserialize)]
struct DeleteForm {
    #[serde(rename = "_csrf")]
    csrf: String,
    identifier: String,
}

async fn delete(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<DeleteForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form.csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let identifier = form.identifier.trim().to_owned();
    if identifier.is_empty() {
        return Err(WebError::Validation {
            field: "identifier".into(),
            msg: "required".into(),
        });
    }
    let delete_result = tracker_request(&state, |reply| TrackerCommand::DeleteFromWeb {
        identifier: identifier.clone(),
        requested_by: session.user_login.clone(),
        is_mod: true,
        reply,
    })
    .await;
    tracing::info!(
        target: "twitch_1337_web",
        user_id = %session.user_id,
        action = "flight_delete",
        target_id = %identifier,
        result = match &delete_result {
            TrackerResult::Ok(Some(_)) => "ok",
            TrackerResult::Ok(None) => "not_found",
            TrackerResult::Timeout => "timeout",
            TrackerResult::Unavailable => "unavailable",
        },
    );
    let msg = match delete_result {
        TrackerResult::Ok(Some(label)) => format!("Untracked `{label}`."),
        TrackerResult::Ok(None) => format!("`{identifier}` not found."),
        TrackerResult::Timeout => {
            "Flight tracker busy — refresh to check whether the delete applied.".to_owned()
        }
        TrackerResult::Unavailable => "Aviation tracking disabled.".to_owned(),
    };
    flash::set(&cookies, &msg);
    Ok(Redirect::to("/flights").into_response())
}
