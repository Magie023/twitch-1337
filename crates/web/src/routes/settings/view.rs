//! The settings page render concern: the `ShowTpl` template struct and the
//! `GET /settings` handler. The save handler reuses `ShowTpl` to re-render the
//! page with field errors, so the struct and its fields are visible to the
//! parent module.

use askama::Template;
use axum::extract::{Extension, State};
use axum::response::Response;
use tower_cookies::Cookies;
use twitch_1337_core::settings::{FieldError, Settings};

use crate::auth::Role;
use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::routes::render;
use crate::state::WebState;

#[derive(Template)]
#[template(path = "settings/index.html")]
pub(super) struct ShowTpl {
    pub(super) csrf: String,
    pub(super) flash: Option<String>,
    pub(super) user_login: String,
    pub(super) user_avatar_url: Option<String>,
    pub(super) current_page: &'static str,
    pub(super) is_mod: bool,
    pub(super) is_broadcaster: bool,
    pub(super) is_owner: bool,
    pub(super) current: Settings,
    pub(super) defaults: Settings,
    pub(super) errors: Vec<FieldError>,
}

pub(super) async fn show(
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
