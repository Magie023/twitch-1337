//! `/schedules` CRUD handlers (mod-gated).
//!
//! The list page renders all schedules + an add form. Each row has its
//! own inline edit form, toggled via `?edit=<name>`. Validation errors
//! re-render the list with per-row error attribution.

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::settings::{Actor, ScheduleSettings, SettingsError};

use crate::auth::csrf;
use crate::auth::session::Session;
use crate::error::WebError;
use crate::flash;
use crate::routes::render;
use crate::state::WebState;

#[derive(Debug, Clone)]
pub(crate) struct RowError {
    pub row_index: usize,
    pub row_name: String,
    pub field: String,
    pub message: String,
}

pub fn router() -> Router<WebState> {
    Router::new()
        .route("/schedules", get(list))
        .route("/schedules/add", post(create))
        .route("/schedules/{name}/edit", post(update))
        .route("/schedules/{name}/delete", post(delete))
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ListQuery {
    /// When `Some(name)`, the matching row renders in edit mode.
    edit: Option<String>,
}

#[derive(Template)]
#[template(path = "schedules/index.html")]
struct ListTpl {
    rows: Vec<ScheduleSettings>,
    edit_name: Option<String>,
    /// Per-row validation errors. Indexed by row position in `rows` so blank-
    /// name rows attribute correctly (PR #227 review F7).
    row_errors: Vec<RowError>,
    /// Non-row-attributable validation errors (raw field path + message).
    global_errors: Vec<(String, String)>,
    flash: Option<String>,
    csrf: String,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
}

#[derive(Debug, Deserialize)]
struct ScheduleForm {
    _csrf: String,
    name: String,
    message: String,
    interval: String,
    #[serde(default)]
    start_date: String,
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    active_time_start: String,
    #[serde(default)]
    active_time_end: String,
    #[serde(default)]
    enabled: Option<String>, // checkbox: "true" or absent
}

fn opt(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_owned())
    }
}

impl ScheduleForm {
    fn into_settings(self) -> ScheduleSettings {
        ScheduleSettings {
            name: self.name.trim().to_owned(),
            message: self.message.trim().to_owned(),
            interval: self.interval.trim().to_owned(),
            start_date: opt(self.start_date),
            end_date: opt(self.end_date),
            active_time_start: opt(self.active_time_start),
            active_time_end: opt(self.active_time_end),
            enabled: self.enabled.is_some(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DeleteForm {
    _csrf: String,
}

async fn list(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Query(q): Query<ListQuery>,
    cookies: Cookies,
) -> Result<Response, WebError> {
    let rows = state.settings.load().schedules.clone();
    let flash_msg = flash::take(&cookies);
    render(&ListTpl {
        rows,
        edit_name: q.edit,
        row_errors: Vec::new(),
        global_errors: Vec::new(),
        flash: flash_msg,
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::SCHEDULES,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
    })
}

async fn create(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum::Form(form): axum::Form<ScheduleForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let new_row = form.into_settings();
    let mut intended_rows = state.settings.load().schedules.clone();
    intended_rows.push(new_row.clone());
    let new_row_for_mutate = new_row.clone();
    apply_or_rerender(
        &state,
        &session,
        cookies,
        intended_rows,
        Box::new(move |o| {
            let mut next = o.schedules.clone().unwrap_or_default();
            next.push(new_row_for_mutate);
            o.schedules = Some(next);
        }),
        None,
    )
    .await
}

async fn update(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<ScheduleForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let new_row = form.into_settings();
    let new_name = new_row.name.clone();
    let mut intended_rows = state.settings.load().schedules.clone();
    if let Some(idx) = intended_rows.iter().position(|s| s.name == name) {
        intended_rows[idx] = new_row.clone();
    }
    let name_for_mutate = name.clone();
    let new_row_for_mutate = new_row.clone();
    apply_or_rerender(
        &state,
        &session,
        cookies,
        intended_rows,
        Box::new(move |o| {
            let mut next = o.schedules.clone().unwrap_or_default();
            if let Some(idx) = next.iter().position(|s| s.name == name_for_mutate) {
                next[idx] = new_row_for_mutate;
            }
            o.schedules = Some(next);
        }),
        Some(new_name),
    )
    .await
}

async fn delete(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<DeleteForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let intended_rows: Vec<ScheduleSettings> = state
        .settings
        .load()
        .schedules
        .iter()
        .filter(|s| s.name != name)
        .cloned()
        .collect();
    let name_for_mutate = name.clone();
    apply_or_rerender(
        &state,
        &session,
        cookies,
        intended_rows,
        Box::new(move |o| {
            let next: Vec<ScheduleSettings> = o
                .schedules
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| s.name != name_for_mutate)
                .collect();
            o.schedules = Some(next);
        }),
        None,
    )
    .await
}

type Mutator =
    Box<dyn FnOnce(&mut twitch_1337_core::settings::overrides::SettingsOverrides) + Send>;

async fn apply_or_rerender(
    state: &WebState,
    session: &Session,
    cookies: Cookies,
    intended_rows: Vec<ScheduleSettings>,
    mutate: Mutator,
    edit_on_error: Option<String>,
) -> Result<Response, WebError> {
    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };
    match state.settings_store.apply_with(mutate, actor).await {
        Ok(_) => {
            flash::set(&cookies, "Schedules saved.");
            Ok(Redirect::to("/schedules").into_response())
        }
        Err(SettingsError::Validation(errs)) => {
            // intended_rows is the user's attempted submission; renders the
            // failed attempt rather than the persisted pre-submit state so
            // error attribution by index resolves correctly.
            render_validation(session, intended_rows, errs, edit_on_error)
        }
        Err(e) => Err(WebError::Internal(eyre::eyre!("settings apply: {e}"))),
    }
}

fn render_validation(
    session: &Session,
    rows: Vec<ScheduleSettings>,
    errs: Vec<twitch_1337_core::settings::FieldError>,
    edit_on_error: Option<String>,
) -> Result<Response, WebError> {
    let mut row_errors: Vec<RowError> = Vec::new();
    let mut global_errors: Vec<(String, String)> = Vec::new();
    for e in errs {
        if let Some(rest) = e.field.strip_prefix("schedules[")
            && let Some(end) = rest.find(']')
        {
            let idx_str = &rest[..end];
            if let Ok(idx) = idx_str.parse::<usize>() {
                let row_name = rows.get(idx).map(|r| r.name.clone()).unwrap_or_default();
                let field_name = rest.get(end + 2..).unwrap_or("").to_owned();
                row_errors.push(RowError {
                    row_index: idx,
                    row_name,
                    field: field_name,
                    message: e.message,
                });
                continue;
            }
        }
        global_errors.push((e.field, e.message));
    }
    let resp = ListTpl {
        rows,
        edit_name: edit_on_error,
        row_errors,
        global_errors,
        flash: None,
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::SCHEDULES,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
    };
    render(&resp)
}

#[cfg(test)]
mod into_settings_tests {
    use super::ScheduleForm;

    #[test]
    fn trims_all_text_fields() {
        let form = ScheduleForm {
            _csrf: "x".into(),
            name: "  noon  ".into(),
            message: "  hello world  ".into(),
            interval: "  01:00  ".into(),
            start_date: "".into(),
            end_date: "".into(),
            active_time_start: "".into(),
            active_time_end: "".into(),
            enabled: Some("true".into()),
        };
        let s = form.into_settings();
        assert_eq!(s.name, "noon");
        assert_eq!(s.message, "hello world");
        assert_eq!(s.interval, "01:00");
    }
}

#[cfg(test)]
mod field_path_parse_tests {
    // We can't call render_validation directly without a WebState; instead
    // test the parsing logic by reproducing the slice arithmetic. This pins
    // the bounds-safe `.get(end + 2..).unwrap_or("")` behavior so a future
    // refactor doesn't reintroduce a panicking unchecked slice (PR #227
    // review F11).
    fn extract_field_name(field: &str) -> Option<(usize, String)> {
        let rest = field.strip_prefix("schedules[")?;
        let end = rest.find(']')?;
        let idx: usize = rest[..end].parse().ok()?;
        let field_name = rest.get(end + 2..).unwrap_or("").to_owned();
        Some((idx, field_name))
    }

    #[test]
    fn well_formed_field_path_parses() {
        let got = extract_field_name("schedules[3].name");
        assert_eq!(got, Some((3, "name".to_owned())));
    }

    #[test]
    fn bare_indexed_path_does_not_panic() {
        // No .field suffix — pre-fix this panicked on the slice. With
        // .get().unwrap_or("") it produces an empty field name.
        let got = extract_field_name("schedules[0]");
        assert_eq!(got, Some((0, "".to_owned())));
    }

    #[test]
    fn malformed_path_returns_none() {
        assert!(extract_field_name("schedules.name").is_none());
        assert!(extract_field_name("schedules[abc].name").is_none());
    }
}
