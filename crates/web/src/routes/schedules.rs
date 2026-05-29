//! `/schedules` CRUD handlers (mod-gated).
//!
//! The list page renders a card-grid of all schedules plus an add/edit form.
//! Add: `?new=true`. Edit: `?edit=<name>`. Validation errors re-render the
//! list with per-row error attribution.

use std::collections::BTreeSet;
use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Extension, Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{NaiveDate, NaiveTime, Weekday};
use serde::Deserialize;
use tower_cookies::Cookies;
use twitch_1337_core::schedule::{Schedule, Trigger, WeekdaySet};
use twitch_1337_core::settings::{Actor, SettingsError};

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
        .route("/schedules/{name}/toggle", post(toggle))
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ListQuery {
    /// When `Some(name)`, the matching row renders in edit mode.
    edit: Option<String>,
    /// When `Some(true)`, the "new schedule" form is shown.
    new: Option<bool>,
}

struct CardView {
    name: String,
    message: String,
    enabled: bool,
    trigger_label: String,
    /// Human-rendered active-window label, e.g. "09:00–17:00".
    schedule_window_label: Option<String>,
    last_fired_human: Option<String>,
    fires_today: u32,
    next_fire_human: Option<String>,
}

struct NextFire {
    name: String,
    time: String,
}

#[derive(Default, Debug, Clone)]
struct FormState {
    name: String,
    message: String,
    /// "interval" | "calendar"
    kind: String,
    interval_every: String,
    interval_active_from: String,
    interval_active_to: String,
    interval_days: BTreeSet<String>,
    calendar_at: String,
    calendar_days: BTreeSet<String>,
    start_date: String,
    end_date: String,
    enabled: bool,
}

#[derive(Template)]
#[template(path = "schedules/index.html")]
struct ListTpl {
    rows: Vec<CardView>,
    edit_form: Option<FormState>,
    new_form: Option<FormState>,
    row_errors: Vec<RowError>,
    global_errors: Vec<(String, String)>,
    flash: Option<String>,
    csrf: String,
    user_login: String,
    user_avatar_url: Option<String>,
    current_page: &'static str,
    is_mod: bool,
    is_broadcaster: bool,
    is_owner: bool,
    total: usize,
    active_count: usize,
    fired_today: u32,
    next_fire: Option<NextFire>,
}

#[derive(Debug, Deserialize)]
struct ScheduleForm {
    _csrf: String,
    name: String,
    message: String,
    /// "interval" or "calendar"
    kind: String,
    // Interval fields
    #[serde(default)]
    interval_every: String, // "hh:mm"
    #[serde(default)]
    interval_active_from: String,
    #[serde(default)]
    interval_active_to: String,
    #[serde(default)]
    interval_days: Vec<String>, // "Mon".."Sun"
    // Calendar fields
    #[serde(default)]
    calendar_at: String,
    #[serde(default)]
    calendar_days: Vec<String>,
    // Common date range
    #[serde(default)]
    start_date: String, // YYYY-MM-DD
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    enabled: Option<String>,
}

impl ScheduleForm {
    fn try_into_schedule(self) -> Result<Schedule, Vec<twitch_1337_core::settings::FieldError>> {
        let mut errs = Vec::new();
        let prefix = "form";
        let trigger = match self.kind.as_str() {
            "interval" => {
                let secs = parse_hhmm_to_secs(&self.interval_every).unwrap_or(0);
                let days = parse_weekdays(&self.interval_days);
                let active_from = parse_hhmm(&self.interval_active_from);
                let active_to = parse_hhmm(&self.interval_active_to);
                Trigger::Interval {
                    every: Duration::from_secs(secs),
                    days,
                    active_from,
                    active_to,
                }
            }
            "calendar" => {
                let at = match parse_hhmm(&self.calendar_at) {
                    Some(v) => v,
                    None => {
                        errs.push(twitch_1337_core::settings::FieldError {
                            field: format!("{prefix}.calendar_at"),
                            message: format!("must be HH:MM (got {:?})", self.calendar_at),
                        });
                        NaiveTime::from_hms_opt(0, 0, 0).unwrap()
                    }
                };
                let days = parse_weekdays(&self.calendar_days);
                Trigger::Calendar { days, at }
            }
            other => {
                errs.push(twitch_1337_core::settings::FieldError {
                    field: format!("{prefix}.kind"),
                    message: format!("unknown trigger kind {other:?}"),
                });
                Trigger::Calendar {
                    days: WeekdaySet::default(),
                    at: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
                }
            }
        };
        if !errs.is_empty() {
            return Err(errs);
        }
        Ok(Schedule {
            name: self.name.trim().to_owned(),
            message: self.message.trim().to_owned(),
            trigger,
            start_date: parse_date(&self.start_date),
            end_date: parse_date(&self.end_date),
            enabled: self.enabled.is_some(),
        })
    }
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    NaiveTime::parse_from_str(s, "%H:%M").ok()
}

fn parse_hhmm_to_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (h, m) = s.split_once(':')?;
    let h: u64 = h.parse().ok()?;
    let m: u64 = m.parse().ok()?;
    Some(h * 3600 + m * 60)
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn parse_weekdays(values: &[String]) -> WeekdaySet {
    let mut out = WeekdaySet::default();
    for v in values {
        if let Ok(d) = v.parse::<Weekday>() {
            out.0.insert(d);
        }
    }
    out
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
    let active_count = rows.iter().filter(|r| r.enabled).count();
    let telemetry = state.telemetry.snapshot().await;
    let fired_today: u32 = telemetry.values().map(|r| r.fires_today).sum();
    let now_utc = chrono::Utc::now();
    let now_berlin = now_utc.with_timezone(&chrono_tz::Europe::Berlin);
    let next_fire = rows
        .iter()
        .filter(|r| r.enabled)
        .filter_map(|r| r.next_active_fire(now_berlin).map(|t| (r.name.clone(), t)))
        .min_by_key(|(_, t)| *t)
        .map(|(name, t)| NextFire {
            name,
            time: t.format("%H:%M").to_string(),
        });
    let edit_form = q.edit.as_ref().and_then(|name| {
        rows.iter()
            .find(|r| r.name == *name)
            .map(form_state_from_schedule)
    });
    let new_form = if q.new.unwrap_or(false) {
        Some(FormState::default())
    } else {
        None
    };
    let cards: Vec<CardView> = rows
        .iter()
        .map(|s| card_view(s, telemetry.get(&s.name), now_utc, now_berlin))
        .collect();
    let total = cards.len();
    render(&ListTpl {
        rows: cards,
        edit_form,
        new_form,
        row_errors: Vec::new(),
        global_errors: Vec::new(),
        flash: flash::take(&cookies),
        csrf: csrf::encode(&session.csrf_value),
        user_login: session.user_login.clone(),
        user_avatar_url: session.avatar_url.clone(),
        current_page: crate::nav::SCHEDULES,
        is_mod: session.is_mod(),
        is_broadcaster: session.is_broadcaster,
        is_owner: matches!(session.role, crate::auth::Role::Owner),
        total,
        active_count,
        fired_today,
        next_fire,
    })
}

fn card_view(
    s: &Schedule,
    tele: Option<&twitch_1337_core::schedule::ScheduleRuntime>,
    now_utc: chrono::DateTime<chrono::Utc>,
    now_berlin: chrono::DateTime<chrono_tz::Tz>,
) -> CardView {
    let (trigger_label, window_label) = trigger_human(&s.trigger);
    let last_fired_human = tele.and_then(|r| r.last_fired_at).map(|t| {
        let delta = now_utc.signed_duration_since(t);
        humanize_delta(delta)
    });
    let next_fire_human = if s.enabled {
        s.next_active_fire(now_berlin)
            .map(|t| t.format("%a %H:%M").to_string())
    } else {
        None
    };
    CardView {
        name: s.name.clone(),
        message: s.message.clone(),
        enabled: s.enabled,
        trigger_label,
        schedule_window_label: window_label,
        last_fired_human,
        fires_today: tele.map(|r| r.fires_today).unwrap_or(0),
        next_fire_human,
    }
}

fn trigger_human(t: &Trigger) -> (String, Option<String>) {
    match t {
        Trigger::Interval {
            every,
            days,
            active_from,
            active_to,
        } => {
            let secs = every.as_secs();
            let mut main = if secs % 3600 == 0 {
                format!("every {} h", secs / 3600)
            } else if secs >= 3600 {
                format!("every {}h{}m", secs / 3600, (secs / 60) % 60)
            } else {
                format!("every {} min", secs / 60)
            };
            if !days.is_empty() {
                main.push_str(&format!(" · {}", weekdays_short(days)));
            }
            let window = match (active_from, active_to) {
                (Some(f), Some(t2)) => {
                    Some(format!("{}–{}", f.format("%H:%M"), t2.format("%H:%M")))
                }
                _ => None,
            };
            (main, window)
        }
        Trigger::Calendar { days, at } => {
            let main = if days.is_empty() {
                format!("daily at {}", at.format("%H:%M"))
            } else {
                format!("{} at {}", weekdays_short(days), at.format("%H:%M"))
            };
            (main, None)
        }
    }
}

fn weekdays_short(days: &WeekdaySet) -> String {
    let mut wds: Vec<Weekday> = days.0.iter().copied().collect();
    wds.sort_by_key(Weekday::num_days_from_monday);
    wds.iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("·")
}

fn humanize_delta(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

fn form_state_from_schedule(s: &Schedule) -> FormState {
    let mut fs = FormState {
        name: s.name.clone(),
        message: s.message.clone(),
        enabled: s.enabled,
        start_date: s
            .start_date
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        end_date: s
            .end_date
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        ..FormState::default()
    };
    match &s.trigger {
        Trigger::Interval {
            every,
            days,
            active_from,
            active_to,
        } => {
            fs.kind = "interval".into();
            let total = every.as_secs();
            fs.interval_every = format!("{:02}:{:02}", total / 3600, (total / 60) % 60);
            fs.interval_active_from = active_from
                .map(|t| t.format("%H:%M").to_string())
                .unwrap_or_default();
            fs.interval_active_to = active_to
                .map(|t| t.format("%H:%M").to_string())
                .unwrap_or_default();
            fs.interval_days = days
                .0
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
        }
        Trigger::Calendar { days, at } => {
            fs.kind = "calendar".into();
            fs.calendar_at = at.format("%H:%M").to_string();
            fs.calendar_days = days
                .0
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
        }
    }
    fs
}

fn form_state_from_form(form: &ScheduleForm) -> FormState {
    FormState {
        name: form.name.trim().to_owned(),
        message: form.message.trim().to_owned(),
        kind: form.kind.clone(),
        interval_every: form.interval_every.clone(),
        interval_active_from: form.interval_active_from.clone(),
        interval_active_to: form.interval_active_to.clone(),
        interval_days: form.interval_days.iter().cloned().collect(),
        calendar_at: form.calendar_at.clone(),
        calendar_days: form.calendar_days.iter().cloned().collect(),
        start_date: form.start_date.clone(),
        end_date: form.end_date.clone(),
        enabled: form.enabled.is_some(),
    }
}

async fn create(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    cookies: Cookies,
    axum_extra::extract::Form(form): axum_extra::extract::Form<ScheduleForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let submitted = form_state_from_form(&form);
    let new_row = match form.try_into_schedule() {
        Ok(r) => r,
        Err(errs) => {
            return render_validation(
                &session,
                state.settings.load().schedules.clone(),
                errs,
                None,
                Some(submitted),
            );
        }
    };
    let new_row_for_mutate = new_row.clone();
    apply_or_rerender(
        &state,
        &session,
        cookies,
        Box::new(move |o| {
            let mut next = o.schedules.clone().unwrap_or_default();
            next.push(new_row_for_mutate);
            o.schedules = Some(next);
        }),
        None,
        Some(submitted),
    )
    .await
}

async fn update(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum_extra::extract::Form(form): axum_extra::extract::Form<ScheduleForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let submitted = form_state_from_form(&form);
    let new_row = match form.try_into_schedule() {
        Ok(r) => r,
        Err(errs) => {
            return render_validation(
                &session,
                state.settings.load().schedules.clone(),
                errs,
                Some(name),
                Some(submitted),
            );
        }
    };
    let new_name = new_row.name.clone();
    let name_for_mutate = name.clone();
    let new_row_for_mutate = new_row.clone();
    apply_or_rerender(
        &state,
        &session,
        cookies,
        Box::new(move |o| {
            let mut next = o.schedules.clone().unwrap_or_default();
            if let Some(idx) = next.iter().position(|s| s.name == name_for_mutate) {
                next[idx] = new_row_for_mutate;
            }
            o.schedules = Some(next);
        }),
        Some(new_name),
        Some(submitted),
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
    let name_for_mutate = name.clone();
    apply_or_rerender(
        &state,
        &session,
        cookies,
        Box::new(move |o| {
            let next: Vec<Schedule> = o
                .schedules
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| s.name != name_for_mutate)
                .collect();
            o.schedules = Some(next);
        }),
        None,
        None,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct ToggleForm {
    _csrf: String,
}

async fn toggle(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Path(name): Path<String>,
    cookies: Cookies,
    axum::Form(form): axum::Form<ToggleForm>,
) -> Result<Response, WebError> {
    if !csrf::verify(&form._csrf, &session.csrf_value) {
        return Err(WebError::CsrfMismatch);
    }
    let actor = Actor {
        user_id: session.user_id.clone(),
        user_login: session.user_login.clone(),
    };
    let name_for = name.clone();
    state
        .settings_store
        .apply_with(
            Box::new(
                move |o: &mut twitch_1337_core::settings::overrides::SettingsOverrides| {
                    let mut next = o.schedules.clone().unwrap_or_default();
                    if let Some(s) = next.iter_mut().find(|s| s.name == name_for) {
                        s.enabled = !s.enabled;
                    }
                    o.schedules = Some(next);
                },
            ),
            actor,
        )
        .await
        .map_err(|e| WebError::Internal(eyre::eyre!("settings apply: {e}")))?;
    flash::set(&cookies, &format!("Toggled {name}."));
    Ok(Redirect::to("/schedules").into_response())
}

type Mutator =
    Box<dyn FnOnce(&mut twitch_1337_core::settings::overrides::SettingsOverrides) + Send>;

async fn apply_or_rerender(
    state: &WebState,
    session: &Session,
    cookies: Cookies,
    mutate: Mutator,
    edit_on_error: Option<String>,
    submitted: Option<FormState>,
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
            // Render the *persisted* rows as cards — never the rejected
            // attempt. A card for a row that was never saved vanishes the
            // moment the user clicks edit/toggle/delete on it (no such
            // schedule exists), trapping them. The attempt is instead
            // restored into the editable form via `submitted` so they can
            // fix and resubmit.
            render_validation(
                session,
                state.settings.load().schedules.clone(),
                errs,
                edit_on_error,
                submitted,
            )
        }
        Err(e) => Err(WebError::Internal(eyre::eyre!("settings apply: {e}"))),
    }
}

fn render_validation(
    session: &Session,
    rows: Vec<Schedule>,
    errs: Vec<twitch_1337_core::settings::FieldError>,
    edit_on_error: Option<String>,
    submitted: Option<FormState>,
) -> Result<Response, WebError> {
    let mut row_errors: Vec<RowError> = Vec::new();
    let mut global_errors: Vec<(String, String)> = Vec::new();
    for e in errs {
        if let Some(rest) = e.field.strip_prefix("schedules[")
            && let Some(end) = rest.find(']')
        {
            let idx_str = &rest[..end];
            if let Ok(idx) = idx_str.parse::<usize>() {
                let field_name = rest.get(end + 2..).unwrap_or("").to_owned();
                if let Some(row) = rows.get(idx) {
                    row_errors.push(RowError {
                        row_index: idx,
                        row_name: row.name.clone(),
                        field: field_name,
                        message: e.message,
                    });
                    continue;
                }
                // Index past the persisted rows = the just-submitted row that
                // failed store validation and was never saved (e.g. a
                // duplicate name on create). It has no card, so attribute it to
                // the form rather than a phantom row that doesn't exist.
                global_errors.push((field_name, e.message));
                continue;
            }
        }
        global_errors.push((e.field, e.message));
    }
    // Rebuild card grid from the passed-in rows so the user can see existing
    // schedules alongside the error.
    let now_utc = chrono::Utc::now();
    let now_berlin = now_utc.with_timezone(&chrono_tz::Europe::Berlin);
    let active_count = rows.iter().filter(|r| r.enabled).count();
    let total = rows.len();
    let cards: Vec<CardView> = rows
        .iter()
        .map(|s| card_view(s, None, now_utc, now_berlin))
        .collect();

    // When re-rendering on validation failure, restore the user's submitted
    // form state (if available) for the edit or new form so they don't lose
    // their input.
    let (edit_form, new_form) = match (edit_on_error, submitted) {
        (Some(_), Some(fs)) => (Some(fs), None),
        (Some(name), None) => (
            Some(FormState {
                name,
                ..FormState::default()
            }),
            None,
        ),
        (None, Some(fs)) => (None, Some(fs)),
        (None, None) => (None, None),
    };

    let resp = ListTpl {
        rows: cards,
        edit_form,
        new_form,
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
        total,
        active_count,
        fired_today: 0,
        next_fire: None,
    };
    render(&resp)
}

#[cfg(test)]
mod try_into_schedule_tests {
    use super::{ScheduleForm, parse_date, parse_hhmm, parse_hhmm_to_secs, parse_weekdays};
    use twitch_1337_core::schedule::Trigger;

    fn base_interval_form() -> ScheduleForm {
        ScheduleForm {
            _csrf: "x".into(),
            name: "  noon  ".into(),
            message: "  hello world  ".into(),
            kind: "interval".into(),
            interval_every: "01:00".into(),
            interval_active_from: "".into(),
            interval_active_to: "".into(),
            interval_days: vec![],
            calendar_at: "".into(),
            calendar_days: vec![],
            start_date: "".into(),
            end_date: "".into(),
            enabled: Some("true".into()),
        }
    }

    #[test]
    fn trims_text_fields() {
        let s = base_interval_form().try_into_schedule().unwrap();
        assert_eq!(s.name, "noon");
        assert_eq!(s.message, "hello world");
    }

    #[test]
    fn interval_every_parsed_from_hhmm() {
        let s = base_interval_form().try_into_schedule().unwrap();
        if let Trigger::Interval { every, .. } = s.trigger {
            assert_eq!(every.as_secs(), 3600);
        } else {
            panic!("expected Interval");
        }
    }

    #[test]
    fn interval_zero_passes_through_for_downstream_validation() {
        // The form parser must NOT clamp; Schedule::validate is responsible
        // for rejecting intervals below 60s. Posting "00:00" should produce
        // a Duration of 0 without any silent correction by the form layer.
        let mut form = base_interval_form();
        form.interval_every = "00:00".into(); // 0 secs — must not be silently clamped
        let s = form.try_into_schedule().unwrap();
        if let Trigger::Interval { every, .. } = s.trigger {
            assert_eq!(
                every.as_secs(),
                0,
                "form parser must not clamp; validate() handles this"
            );
        } else {
            panic!("expected Interval");
        }
    }

    #[test]
    fn calendar_at_required() {
        let form = ScheduleForm {
            _csrf: "x".into(),
            name: "test".into(),
            message: "hi".into(),
            kind: "calendar".into(),
            interval_every: "".into(),
            interval_active_from: "".into(),
            interval_active_to: "".into(),
            interval_days: vec![],
            calendar_at: "".into(), // missing
            calendar_days: vec![],
            start_date: "".into(),
            end_date: "".into(),
            enabled: None,
        };
        let errs = form.try_into_schedule().unwrap_err();
        assert!(errs.iter().any(|e| e.field.contains("calendar_at")));
    }

    #[test]
    fn unknown_kind_returns_error() {
        let form = ScheduleForm {
            _csrf: "x".into(),
            name: "test".into(),
            message: "hi".into(),
            kind: "bogus".into(),
            interval_every: "".into(),
            interval_active_from: "".into(),
            interval_active_to: "".into(),
            interval_days: vec![],
            calendar_at: "".into(),
            calendar_days: vec![],
            start_date: "".into(),
            end_date: "".into(),
            enabled: None,
        };
        let errs = form.try_into_schedule().unwrap_err();
        assert!(errs.iter().any(|e| e.field.contains("kind")));
    }

    #[test]
    fn parse_hhmm_helper() {
        use chrono::NaiveTime;
        assert_eq!(
            parse_hhmm("08:30"),
            Some(NaiveTime::from_hms_opt(8, 30, 0).unwrap())
        );
        assert_eq!(parse_hhmm(""), None);
        assert_eq!(parse_hhmm("  "), None);
        assert_eq!(parse_hhmm("invalid"), None);
    }

    #[test]
    fn parse_hhmm_to_secs_helper() {
        assert_eq!(parse_hhmm_to_secs("01:30"), Some(5400));
        assert_eq!(parse_hhmm_to_secs(""), None);
    }

    #[test]
    fn parse_date_helper() {
        use chrono::NaiveDate;
        assert_eq!(
            parse_date("2026-06-01"),
            Some(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap())
        );
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn parse_weekdays_helper() {
        let days = parse_weekdays(&["Mon".to_owned(), "Wed".to_owned()]);
        use chrono::Weekday;
        assert!(days.0.contains(&Weekday::Mon));
        assert!(days.0.contains(&Weekday::Wed));
        assert!(!days.0.contains(&Weekday::Fri));
    }
}

#[cfg(test)]
mod weekday_roundtrip_tests {
    #[test]
    fn weekday_string_roundtrip_matches_template() {
        // `form_state_from_schedule` serialises weekdays via `.to_string()`.
        // The form parser deserialises them via `.parse::<Weekday>()`.
        // Both ends must agree on the short-form strings the template renders.
        let cases = [
            (chrono::Weekday::Mon, "Mon"),
            (chrono::Weekday::Tue, "Tue"),
            (chrono::Weekday::Wed, "Wed"),
            (chrono::Weekday::Thu, "Thu"),
            (chrono::Weekday::Fri, "Fri"),
            (chrono::Weekday::Sat, "Sat"),
            (chrono::Weekday::Sun, "Sun"),
        ];
        for (day, expected) in cases {
            let s = day.to_string();
            assert_eq!(s, expected, "Display for {day:?} should be short form");
            let back: chrono::Weekday = s.parse().expect("must round-trip");
            assert_eq!(back, day, "parse should recover the same weekday");
        }
    }
}

#[cfg(test)]
mod field_path_parse_tests {
    // We can't call render_validation directly without a WebState; instead
    // test the parsing logic by reproducing the slice arithmetic. This pins
    // the bounds-safe `.get(end + 2..).unwrap_or("")` behavior so a future
    // refactor doesn't reintroduce a panicking unchecked slice.
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
        let got = extract_field_name("schedules[0]");
        assert_eq!(got, Some((0, "".to_owned())));
    }

    #[test]
    fn malformed_path_returns_none() {
        assert!(extract_field_name("schedules.name").is_none());
        assert!(extract_field_name("schedules[abc].name").is_none());
    }
}
