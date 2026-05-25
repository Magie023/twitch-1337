//! Dashboard-managed runtime settings.
//!
//! `Settings` is the fully-resolved snapshot read by command handlers via
//! a `SettingsHandle = Arc<ArcSwap<Settings>>`. Sparse `SettingsOverrides`
//! (see `overrides.rs`) live on disk at `$DATA_DIR/settings.ron`; missing
//! fields fall through to `compiled_defaults()`.
//!
//! Writes go through `SettingsStore::apply` (see `store.rs`) which
//! validates, atomically persists, swaps the handle, and appends an audit
//! entry.

pub mod ai;
pub mod audit;
pub mod aviationstack;
pub mod migrate;
pub mod overrides;
pub mod schedules;
pub mod store;
pub mod suspend;
pub mod twitch;
pub mod web;

pub use ai::{
    AiBackendKind, AiBehavior, AiConnection, AiDreamer, AiEmotes, AiHistory, AiMedia, AiMemory,
    AiPrefill, AiSettings, AiWeb,
};
#[cfg(any(test, feature = "testing"))]
pub use audit::MemoryAuditLog;
pub use audit::{AuditChange, AuditEntry, AuditError, AuditLog, FileAuditLog};
pub use aviationstack::AviationstackSettings;
pub use overrides::{AiOverrides, CooldownsOverrides, PingsOverrides, SettingsOverrides};
pub use schedules::ScheduleSettings;
pub use store::{Actor, SettingsStore};
pub use suspend::SuspendSettings;
pub use twitch::TwitchRuntime;
pub use web::WebRuntime;

use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type SettingsHandle = Arc<ArcSwap<Settings>>;

pub const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub schema_version: u32,
    pub cooldowns: Cooldowns,
    pub pings: PingsSettings,
    pub ai: AiSettings,
    pub twitch: TwitchRuntime,
    pub aviationstack: AviationstackSettings,
    pub suspend: SuspendSettings,
    pub web: WebRuntime,
    pub schedules: Vec<ScheduleSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cooldowns {
    pub ai: u64,
    pub news: u64,
    pub up: u64,
    pub feedback: u64,
    pub doener: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PingsSettings {
    pub cooldown: u64,
    pub public: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Cooldowns,
    Pings,
    AiConnection,
    AiBehavior,
    AiHistory,
    AiMemory,
    AiDreamer,
    AiPrefill,
    AiWeb,
    AiEmotes,
    AiMedia,
    TwitchPermissions,
    TwitchChannels,
    Aviationstack,
    Suspend,
    WebRuntime,
    Schedules,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

/// Bootstrap-side context required for cross-field validation. `channel`
/// is the IRC channel from config.toml, used to enforce that
/// `twitch.admin_channel` and `twitch.ai_channel` differ from it.
#[derive(Debug, Clone)]
pub struct ValidationContext {
    pub channel: String,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("validation failed")]
    Validation(Vec<FieldError>),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ron: {0}")]
    Ron(#[from] ron::error::SpannedError),
    #[error("persist: {0}")]
    Persist(#[from] crate::util::persist::AtomicPersistError),
}

impl From<Vec<FieldError>> for SettingsError {
    fn from(errs: Vec<FieldError>) -> Self {
        Self::Validation(errs)
    }
}

impl Settings {
    pub fn compiled_defaults() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            cooldowns: Cooldowns {
                ai: 30,
                news: 60,
                up: 30,
                feedback: 300,
                doener: 30,
            },
            pings: PingsSettings {
                cooldown: 300,
                public: false,
            },
            ai: AiSettings::default(),
            twitch: TwitchRuntime::default(),
            aviationstack: AviationstackSettings::default(),
            suspend: SuspendSettings::default(),
            web: WebRuntime::default(),
            schedules: Vec::new(),
        }
    }

    pub fn validate(&self, ctx: &ValidationContext) -> Result<(), Vec<FieldError>> {
        let mut errs = Vec::new();
        fn bound(name: &str, v: u64, lo: u64, hi: u64, errs: &mut Vec<FieldError>) {
            if v < lo || v > hi {
                errs.push(FieldError {
                    field: name.to_owned(),
                    message: format!("must be {lo}..={hi} seconds (got {v})"),
                });
            }
        }
        bound("cooldowns.ai", self.cooldowns.ai, 1, 3600, &mut errs);
        bound("cooldowns.news", self.cooldowns.news, 1, 3600, &mut errs);
        bound("cooldowns.up", self.cooldowns.up, 1, 3600, &mut errs);
        bound(
            "cooldowns.feedback",
            self.cooldowns.feedback,
            1,
            3600,
            &mut errs,
        );
        bound(
            "cooldowns.doener",
            self.cooldowns.doener,
            1,
            3600,
            &mut errs,
        );
        bound("pings.cooldown", self.pings.cooldown, 1, 86_400, &mut errs);
        validate_ai(&self.ai, &mut errs);
        if self.twitch.expected_latency > 1000 {
            errs.push(FieldError {
                field: "twitch.expected_latency".into(),
                message: format!("must be <= 1000ms (got {})", self.twitch.expected_latency),
            });
        }
        for (field, val) in [
            ("twitch.admin_channel", self.twitch.admin_channel.as_deref()),
            ("twitch.ai_channel", self.twitch.ai_channel.as_deref()),
        ] {
            if let Some(v) = val {
                let t = v.trim();
                if t.is_empty() {
                    errs.push(FieldError {
                        field: field.into(),
                        message: "must not be blank when set".into(),
                    });
                } else if t == ctx.channel {
                    errs.push(FieldError {
                        field: field.into(),
                        message: format!("must differ from twitch.channel ({:?})", ctx.channel),
                    });
                }
            }
        }
        if let (Some(ad), Some(ai)) = (
            self.twitch.admin_channel.as_deref(),
            self.twitch.ai_channel.as_deref(),
        ) && !ad.trim().is_empty()
            && !ai.trim().is_empty()
            && ad.trim() == ai.trim()
        {
            errs.push(FieldError {
                field: "twitch.ai_channel".into(),
                message: "must differ from twitch.admin_channel".into(),
            });
        }
        for (idx, id) in self.twitch.hidden_admins.iter().enumerate() {
            if id.trim().is_empty() {
                errs.push(FieldError {
                    field: format!("twitch.hidden_admins[{idx}]"),
                    message: "must not be blank".into(),
                });
            }
        }
        for (idx, id) in self.twitch.viewer_allowlist.iter().enumerate() {
            if id.trim().is_empty() {
                errs.push(FieldError {
                    field: format!("twitch.viewer_allowlist[{idx}]"),
                    message: "must not be blank".into(),
                });
            }
        }
        if !(1..=604_800).contains(&self.suspend.default_duration_secs) {
            errs.push(FieldError {
                field: "suspend.default_duration_secs".into(),
                message: format!(
                    "must be 1..=604800 (got {})",
                    self.suspend.default_duration_secs
                ),
            });
        }
        if self.aviationstack.enabled {
            if reqwest::Url::parse(&self.aviationstack.base_url).is_err() {
                errs.push(FieldError {
                    field: "aviationstack.base_url".into(),
                    message: format!(
                        "must be a valid URL (got {:?})",
                        self.aviationstack.base_url
                    ),
                });
            }
            if self.aviationstack.timeout_secs == 0 {
                errs.push(FieldError {
                    field: "aviationstack.timeout_secs".into(),
                    message: "must be > 0".into(),
                });
            }
        }
        if !(3600..=2_592_000).contains(&self.web.session_ttl_secs) {
            errs.push(FieldError {
                field: "web.session_ttl_secs".into(),
                message: format!("must be 3600..=2592000 (got {})", self.web.session_ttl_secs),
            });
        }
        if !(30..=3600).contains(&self.web.mod_check_refresh_secs) {
            errs.push(FieldError {
                field: "web.mod_check_refresh_secs".into(),
                message: format!(
                    "must be 30..=3600 (got {})",
                    self.web.mod_check_refresh_secs
                ),
            });
        }
        // Schedules
        let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (idx, sc) in self.schedules.iter().enumerate() {
            let prefix = format!("schedules[{idx}]");
            if sc.name.trim().is_empty() {
                errs.push(FieldError {
                    field: format!("{prefix}.name"),
                    message: "must not be blank".into(),
                });
            } else if !seen_names.insert(sc.name.trim()) {
                errs.push(FieldError {
                    field: format!("{prefix}.name"),
                    message: format!("duplicate name {:?}", sc.name.trim()),
                });
            }
            if !sc.name.trim().is_empty() {
                let bad: Vec<char> = sc
                    .name
                    .chars()
                    .filter(|c| {
                        // URL-reserved or query-decoded chars that break the
                        // dashboard's /schedules/<name>/edit + ?edit=<name>
                        // routes, plus ASCII control chars (log injection,
                        // IRC framing). PR #227 review F6 + F9.
                        matches!(
                            c,
                            '/' | '?' | '#' | '%' | '&' | '+' | '=' | ' ' | '\'' | '"'
                        ) || c.is_control()
                    })
                    .collect();
                if !bad.is_empty() {
                    errs.push(FieldError {
                        field: format!("{prefix}.name"),
                        message: format!(
                            "must not contain URL-reserved, whitespace, quote, or control characters {bad:?}"
                        ),
                    });
                }
            }
            if sc.message.trim().is_empty() {
                errs.push(FieldError {
                    field: format!("{prefix}.message"),
                    message: "must not be blank".into(),
                });
            }
            match crate::database::Schedule::parse_interval(&sc.interval) {
                Ok(d) if d.num_seconds() <= 0 => {
                    errs.push(FieldError {
                        field: format!("{prefix}.interval"),
                        message: format!("must be > 0 (got {:?})", sc.interval),
                    });
                }
                Ok(_) => {}
                Err(e) => errs.push(FieldError {
                    field: format!("{prefix}.interval"),
                    message: format!("invalid: {e}"),
                }),
            }
            for (field, val) in [
                ("start_date", sc.start_date.as_deref()),
                ("end_date", sc.end_date.as_deref()),
            ] {
                if let Some(v) = val
                    && chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S").is_err()
                {
                    errs.push(FieldError {
                        field: format!("{prefix}.{field}"),
                        message: format!("must be YYYY-MM-DDTHH:MM:SS (got {v:?})"),
                    });
                }
            }
            for (field, val) in [
                ("active_time_start", sc.active_time_start.as_deref()),
                ("active_time_end", sc.active_time_end.as_deref()),
            ] {
                if let Some(v) = val
                    && chrono::NaiveTime::parse_from_str(v, "%H:%M").is_err()
                {
                    errs.push(FieldError {
                        field: format!("{prefix}.{field}"),
                        message: format!("must be HH:MM (got {v:?})"),
                    });
                }
            }
            match (
                sc.active_time_start.as_deref(),
                sc.active_time_end.as_deref(),
            ) {
                (Some(_), None) => errs.push(FieldError {
                    field: format!("{prefix}.active_time_end"),
                    message: "must be set when active_time_start is set".into(),
                }),
                (None, Some(_)) => errs.push(FieldError {
                    field: format!("{prefix}.active_time_start"),
                    message: "must be set when active_time_end is set".into(),
                }),
                _ => {}
            }
            if let (Some(sd), Some(ed)) = (sc.start_date.as_deref(), sc.end_date.as_deref())
                && let (Ok(s), Ok(e)) = (
                    chrono::NaiveDateTime::parse_from_str(sd, "%Y-%m-%dT%H:%M:%S"),
                    chrono::NaiveDateTime::parse_from_str(ed, "%Y-%m-%dT%H:%M:%S"),
                )
                && e <= s
            {
                errs.push(FieldError {
                    field: format!("{prefix}.end_date"),
                    message: format!("must be after start_date (start={sd}, end={ed})"),
                });
            }
        }
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }

    pub fn resolve(defaults: &Settings, overrides: &overrides::SettingsOverrides) -> Settings {
        Settings {
            schema_version: SCHEMA_VERSION,
            cooldowns: Cooldowns {
                ai: overrides.cooldowns.ai.unwrap_or(defaults.cooldowns.ai),
                news: overrides.cooldowns.news.unwrap_or(defaults.cooldowns.news),
                up: overrides.cooldowns.up.unwrap_or(defaults.cooldowns.up),
                feedback: overrides
                    .cooldowns
                    .feedback
                    .unwrap_or(defaults.cooldowns.feedback),
                doener: overrides
                    .cooldowns
                    .doener
                    .unwrap_or(defaults.cooldowns.doener),
            },
            pings: PingsSettings {
                cooldown: overrides.pings.cooldown.unwrap_or(defaults.pings.cooldown),
                public: overrides.pings.public.unwrap_or(defaults.pings.public),
            },
            ai: resolve_ai(&defaults.ai, &overrides.ai),
            twitch: TwitchRuntime {
                expected_latency: overrides
                    .twitch
                    .expected_latency
                    .unwrap_or(defaults.twitch.expected_latency),
                hidden_admins: overrides
                    .twitch
                    .hidden_admins
                    .clone()
                    .unwrap_or_else(|| defaults.twitch.hidden_admins.clone()),
                viewer_allowlist: overrides
                    .twitch
                    .viewer_allowlist
                    .clone()
                    .unwrap_or_else(|| defaults.twitch.viewer_allowlist.clone()),
                admin_channel: match &overrides.twitch.admin_channel {
                    Some(v) => v.clone(),
                    None => defaults.twitch.admin_channel.clone(),
                },
                ai_channel: match &overrides.twitch.ai_channel {
                    Some(v) => v.clone(),
                    None => defaults.twitch.ai_channel.clone(),
                },
            },
            aviationstack: AviationstackSettings {
                enabled: overrides
                    .aviationstack
                    .enabled
                    .unwrap_or(defaults.aviationstack.enabled),
                base_url: overrides
                    .aviationstack
                    .base_url
                    .clone()
                    .unwrap_or_else(|| defaults.aviationstack.base_url.clone()),
                timeout_secs: overrides
                    .aviationstack
                    .timeout_secs
                    .unwrap_or(defaults.aviationstack.timeout_secs),
            },
            suspend: SuspendSettings {
                default_duration_secs: overrides
                    .suspend
                    .default_duration_secs
                    .unwrap_or(defaults.suspend.default_duration_secs),
            },
            web: WebRuntime {
                session_ttl_secs: overrides
                    .web
                    .session_ttl_secs
                    .unwrap_or(defaults.web.session_ttl_secs),
                mod_check_refresh_secs: overrides
                    .web
                    .mod_check_refresh_secs
                    .unwrap_or(defaults.web.mod_check_refresh_secs),
            },
            schedules: overrides
                .schedules
                .clone()
                .unwrap_or_else(|| defaults.schedules.clone()),
        }
    }
}

fn resolve_ai(defaults: &AiSettings, o: &overrides::AiOverrides) -> AiSettings {
    use ai::{AiBehavior, AiConnection, AiDreamer, AiHistory, AiMedia, AiMemory};
    AiSettings {
        connection: AiConnection {
            backend: o.connection.backend.unwrap_or(defaults.connection.backend),
            base_url: match &o.connection.base_url {
                Some(v) => v.clone(),
                None => defaults.connection.base_url.clone(),
            },
            model: o
                .connection
                .model
                .clone()
                .unwrap_or_else(|| defaults.connection.model.clone()),
            timeout: o.connection.timeout.unwrap_or(defaults.connection.timeout),
            reasoning_effort: match &o.connection.reasoning_effort {
                Some(v) => v.clone(),
                None => defaults.connection.reasoning_effort.clone(),
            },
            service_tier: match &o.connection.service_tier {
                Some(v) => v.clone(),
                None => defaults.connection.service_tier.clone(),
            },
        },
        behavior: AiBehavior {
            max_turn_rounds: o
                .behavior
                .max_turn_rounds
                .unwrap_or(defaults.behavior.max_turn_rounds),
            max_writes_per_turn: o
                .behavior
                .max_writes_per_turn
                .unwrap_or(defaults.behavior.max_writes_per_turn),
            persona_name: o
                .behavior
                .persona_name
                .clone()
                .unwrap_or_else(|| defaults.behavior.persona_name.clone()),
        },
        history: AiHistory {
            length: o.history.length.unwrap_or(defaults.history.length),
            ai_channel_length: o
                .history
                .ai_channel_length
                .unwrap_or(defaults.history.ai_channel_length),
        },
        memory: AiMemory {
            soul_bytes: o.memory.soul_bytes.unwrap_or(defaults.memory.soul_bytes),
            lore_bytes: o.memory.lore_bytes.unwrap_or(defaults.memory.lore_bytes),
            user_bytes: o.memory.user_bytes.unwrap_or(defaults.memory.user_bytes),
            state_bytes: o.memory.state_bytes.unwrap_or(defaults.memory.state_bytes),
            inject_byte_budget: o
                .memory
                .inject_byte_budget
                .unwrap_or(defaults.memory.inject_byte_budget),
            max_state_files: o
                .memory
                .max_state_files
                .unwrap_or(defaults.memory.max_state_files),
        },
        dreamer: AiDreamer {
            enabled: o.dreamer.enabled.unwrap_or(defaults.dreamer.enabled),
            model: match &o.dreamer.model {
                Some(v) => v.clone(),
                None => defaults.dreamer.model.clone(),
            },
            reasoning_effort: match &o.dreamer.reasoning_effort {
                Some(v) => v.clone(),
                None => defaults.dreamer.reasoning_effort.clone(),
            },
            service_tier: match &o.dreamer.service_tier {
                Some(v) => v.clone(),
                None => defaults.dreamer.service_tier.clone(),
            },
            run_at: o
                .dreamer
                .run_at
                .clone()
                .unwrap_or_else(|| defaults.dreamer.run_at.clone()),
            timeout_secs: o
                .dreamer
                .timeout_secs
                .unwrap_or(defaults.dreamer.timeout_secs),
            max_rounds: o.dreamer.max_rounds.unwrap_or(defaults.dreamer.max_rounds),
        },
        prefill: resolve_prefill(defaults.prefill.as_ref(), &o.prefill),
        web: resolve_web(defaults.web.as_ref(), &o.web),
        emotes: resolve_emotes(defaults.emotes.as_ref(), &o.emotes),
        media: AiMedia {
            model: o
                .media
                .model
                .clone()
                .unwrap_or_else(|| defaults.media.model.clone()),
            timeout: o.media.timeout.unwrap_or(defaults.media.timeout),
            max_image_size: o
                .media
                .max_image_size
                .unwrap_or(defaults.media.max_image_size),
            max_pdf_size: o.media.max_pdf_size.unwrap_or(defaults.media.max_pdf_size),
            max_audio_size: o
                .media
                .max_audio_size
                .unwrap_or(defaults.media.max_audio_size),
            max_video_size: o
                .media
                .max_video_size
                .unwrap_or(defaults.media.max_video_size),
            max_text_size: o
                .media
                .max_text_size
                .unwrap_or(defaults.media.max_text_size),
        },
    }
}

fn resolve_prefill(
    defaults: Option<&AiPrefill>,
    o: &overrides::AiPrefillOverrides,
) -> Option<AiPrefill> {
    let enabled = o.enabled.unwrap_or_else(|| defaults.is_some());
    if !enabled {
        return None;
    }
    let base = defaults.cloned().unwrap_or_default();
    Some(AiPrefill {
        base_url: o.base_url.clone().unwrap_or(base.base_url),
        threshold: o.threshold.unwrap_or(base.threshold),
    })
}

fn resolve_web(defaults: Option<&AiWeb>, o: &overrides::AiWebOverrides) -> Option<AiWeb> {
    let enabled = o.enabled.unwrap_or_else(|| defaults.is_some());
    if !enabled {
        return None;
    }
    let base = defaults.cloned().unwrap_or_default();
    Some(AiWeb {
        base_url: o.base_url.clone().unwrap_or(base.base_url),
        timeout: o.timeout.unwrap_or(base.timeout),
        max_results: o.max_results.unwrap_or(base.max_results),
        max_rounds: o.max_rounds.unwrap_or(base.max_rounds),
        cache_ttl_secs: o.cache_ttl_secs.unwrap_or(base.cache_ttl_secs),
        cache_capacity: o.cache_capacity.unwrap_or(base.cache_capacity),
    })
}

fn resolve_emotes(
    defaults: Option<&AiEmotes>,
    o: &overrides::AiEmotesOverrides,
) -> Option<AiEmotes> {
    let enabled = o.enabled.unwrap_or_else(|| defaults.is_some());
    if !enabled {
        return None;
    }
    let base = defaults.cloned().unwrap_or_default();
    Some(AiEmotes {
        include_global: o.include_global.unwrap_or(base.include_global),
        refresh_interval_secs: o
            .refresh_interval_secs
            .unwrap_or(base.refresh_interval_secs),
        max_prompt_emotes: o.max_prompt_emotes.unwrap_or(base.max_prompt_emotes),
        min_baseline_emotes: o.min_baseline_emotes.unwrap_or(base.min_baseline_emotes),
        base_url: match &o.base_url {
            Some(v) => v.clone(),
            None => base.base_url,
        },
    })
}

fn validate_ai(ai: &AiSettings, errs: &mut Vec<FieldError>) {
    fn err(errs: &mut Vec<FieldError>, field: &str, msg: String) {
        errs.push(FieldError {
            field: field.into(),
            message: msg,
        });
    }

    if !(1..=20).contains(&ai.behavior.max_turn_rounds) {
        err(
            errs,
            "ai.behavior.max_turn_rounds",
            format!("must be 1..=20 (got {})", ai.behavior.max_turn_rounds),
        );
    }
    if !(1..=64).contains(&ai.behavior.max_writes_per_turn) {
        err(
            errs,
            "ai.behavior.max_writes_per_turn",
            format!("must be 1..=64 (got {})", ai.behavior.max_writes_per_turn),
        );
    }
    if ai.behavior.persona_name.trim().is_empty() {
        err(errs, "ai.behavior.persona_name", "must not be empty".into());
    }
    if ai.history.length > crate::ai::chat_history::MAX_HISTORY_LENGTH {
        err(
            errs,
            "ai.history.length",
            format!("must be <= {}", crate::ai::chat_history::MAX_HISTORY_LENGTH),
        );
    }
    if ai.history.ai_channel_length > crate::ai::chat_history::MAX_HISTORY_LENGTH {
        err(
            errs,
            "ai.history.ai_channel_length",
            format!("must be <= {}", crate::ai::chat_history::MAX_HISTORY_LENGTH),
        );
    }
    if ai.memory.inject_byte_budget < ai.memory.soul_bytes + ai.memory.lore_bytes {
        err(
            errs,
            "ai.memory.inject_byte_budget",
            "must be >= soul_bytes + lore_bytes".into(),
        );
    }
    if !(1..=200).contains(&ai.dreamer.max_rounds) {
        err(
            errs,
            "ai.dreamer.max_rounds",
            format!("must be 1..=200 (got {})", ai.dreamer.max_rounds),
        );
    }
    if ai.dreamer.timeout_secs == 0 {
        err(errs, "ai.dreamer.timeout_secs", "must be > 0".into());
    }
    if chrono::NaiveTime::parse_from_str(&ai.dreamer.run_at, "%H:%M").is_err() {
        err(
            errs,
            "ai.dreamer.run_at",
            format!("must be HH:MM (got {:?})", ai.dreamer.run_at),
        );
    }
    for (field, val) in [
        (
            "ai.connection.reasoning_effort",
            ai.connection.reasoning_effort.as_deref(),
        ),
        (
            "ai.dreamer.reasoning_effort",
            ai.dreamer.reasoning_effort.as_deref(),
        ),
    ] {
        if let Some(v) = val
            && v.trim().is_empty()
        {
            err(errs, field, "must be non-empty when set".into());
        }
    }
    for (field, val) in [
        (
            "ai.connection.service_tier",
            ai.connection.service_tier.as_deref(),
        ),
        (
            "ai.dreamer.service_tier",
            ai.dreamer.service_tier.as_deref(),
        ),
    ] {
        if let Some(v) = val {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                err(errs, field, "must be non-empty when set".into());
            } else if !matches!(trimmed, "flex" | "priority") {
                err(
                    errs,
                    field,
                    format!("must be one of 'flex' | 'priority' (got {v:?})"),
                );
            }
        }
    }
    if let Some(url) = ai.connection.base_url.as_deref()
        && reqwest::Url::parse(url).is_err()
    {
        err(
            errs,
            "ai.connection.base_url",
            format!("must be a valid URL (got {url:?})"),
        );
    }
    if ai.connection.timeout == 0 {
        err(errs, "ai.connection.timeout", "must be > 0".into());
    }
    if let Some(prefill) = &ai.prefill {
        if reqwest::Url::parse(&prefill.base_url).is_err() {
            err(
                errs,
                "ai.prefill.base_url",
                format!("must be a valid URL (got {:?})", prefill.base_url),
            );
        }
        if !(0.0..=1.0).contains(&prefill.threshold) {
            err(
                errs,
                "ai.prefill.threshold",
                format!("must be 0.0..=1.0 (got {})", prefill.threshold),
            );
        }
        if ai.history.length == 0 {
            err(errs, "ai.prefill", "requires ai.history.length > 0".into());
        }
    }
    if let Some(web) = &ai.web {
        if reqwest::Url::parse(&web.base_url).is_err() {
            err(
                errs,
                "ai.web.base_url",
                format!("must be a valid URL (got {:?})", web.base_url),
            );
        }
        if !(1..=10).contains(&web.max_results) {
            err(
                errs,
                "ai.web.max_results",
                format!("must be 1..=10 (got {})", web.max_results),
            );
        }
        if !(1..=6).contains(&web.max_rounds) {
            err(
                errs,
                "ai.web.max_rounds",
                format!("must be 1..=6 (got {})", web.max_rounds),
            );
        }
        if web.cache_capacity == 0 {
            err(errs, "ai.web.cache_capacity", "must be > 0".into());
        }
    }
    if let Some(em) = &ai.emotes {
        if em.refresh_interval_secs == 0 {
            err(
                errs,
                "ai.emotes.refresh_interval_secs",
                "must be > 0".into(),
            );
        }
        if !(1..=200).contains(&em.max_prompt_emotes) {
            err(
                errs,
                "ai.emotes.max_prompt_emotes",
                format!("must be 1..=200 (got {})", em.max_prompt_emotes),
            );
        }
        if em.min_baseline_emotes > em.max_prompt_emotes {
            err(
                errs,
                "ai.emotes.min_baseline_emotes",
                "must be <= max_prompt_emotes".into(),
            );
        }
        if let Some(url) = em.base_url.as_deref()
            && url.trim().is_empty()
        {
            err(
                errs,
                "ai.emotes.base_url",
                "must be non-empty when set".into(),
            );
        }
    }
}

#[cfg(any(test, feature = "testing"))]
pub fn test_handle() -> SettingsHandle {
    Arc::new(ArcSwap::from_pointee(Settings::compiled_defaults()))
}

#[cfg(test)]
mod resolve_tests {
    use super::overrides::{CooldownsOverrides, PingsOverrides, SettingsOverrides};
    use super::*;

    #[test]
    fn empty_overrides_equal_defaults() {
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides::default();
        assert_eq!(Settings::resolve(&defaults, &overrides), defaults);
    }

    #[test]
    fn cooldown_override_wins_per_field() {
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(15),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let resolved = Settings::resolve(&defaults, &overrides);
        assert_eq!(resolved.cooldowns.ai, 15);
        assert_eq!(resolved.cooldowns.news, defaults.cooldowns.news);
        assert_eq!(resolved.pings, defaults.pings);
    }

    #[test]
    fn pings_public_override_flips_bool() {
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            pings: PingsOverrides {
                public: Some(true),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let resolved = Settings::resolve(&defaults, &overrides);
        assert!(resolved.pings.public);
        assert_eq!(resolved.pings.cooldown, defaults.pings.cooldown);
    }

    #[test]
    fn pings_cooldown_override_leaves_public_at_default() {
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            pings: PingsOverrides {
                cooldown: Some(600),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let resolved = Settings::resolve(&defaults, &overrides);
        assert_eq!(resolved.pings.cooldown, 600);
        assert_eq!(resolved.pings.public, defaults.pings.public);
    }

    #[test]
    fn validate_collects_multiple_errors() {
        let mut s = Settings::compiled_defaults();
        s.cooldowns.ai = 0;
        s.pings.cooldown = 0;
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("both bounds violated");
        let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"cooldowns.ai"));
        assert!(fields.contains(&"pings.cooldown"));
    }

    #[test]
    fn validate_accepts_compiled_defaults() {
        Settings::compiled_defaults()
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect("compiled defaults pass validate()");
    }

    #[test]
    fn compiled_defaults_v3_layout() {
        let s = Settings::compiled_defaults();
        assert_eq!(s.schema_version, 3);
        assert_eq!(s.ai, AiSettings::default());
        assert_eq!(s.twitch, TwitchRuntime::default());
        assert_eq!(s.aviationstack, AviationstackSettings::default());
        assert_eq!(s.suspend, SuspendSettings::default());
        assert_eq!(s.web, WebRuntime::default());
        assert!(s.schedules.is_empty());
    }

    #[test]
    fn ai_connection_model_override_wins() {
        use crate::settings::overrides::{AiConnectionOverrides, AiOverrides};

        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            ai: AiOverrides {
                connection: AiConnectionOverrides {
                    model: Some("gpt-5".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let resolved = Settings::resolve(&defaults, &overrides);
        assert_eq!(resolved.ai.connection.model, "gpt-5");
        assert_eq!(
            resolved.ai.connection.timeout,
            defaults.ai.connection.timeout
        );
    }

    #[test]
    fn validate_rejects_empty_persona_name() {
        let mut s = Settings::compiled_defaults();
        s.ai.behavior.persona_name = "   ".into();
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(errs.iter().any(|e| e.field == "ai.behavior.persona_name"));
    }

    #[test]
    fn validate_rejects_max_turn_rounds_out_of_range() {
        let mut s = Settings::compiled_defaults();
        s.ai.behavior.max_turn_rounds = 0;
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(
            errs.iter()
                .any(|e| e.field == "ai.behavior.max_turn_rounds")
        );
    }

    #[test]
    fn validate_rejects_inject_budget_below_soul_plus_lore() {
        let mut s = Settings::compiled_defaults();
        s.ai.memory.soul_bytes = 4096;
        s.ai.memory.lore_bytes = 12288;
        s.ai.memory.inject_byte_budget = 1024;
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(
            errs.iter()
                .any(|e| e.field == "ai.memory.inject_byte_budget")
        );
    }

    #[test]
    fn validate_rejects_malformed_dreamer_run_at() {
        let mut s = Settings::compiled_defaults();
        s.ai.dreamer.run_at = "not-a-time".into();
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(errs.iter().any(|e| e.field == "ai.dreamer.run_at"));
    }

    #[test]
    fn validate_rejects_invalid_connection_base_url() {
        let mut s = Settings::compiled_defaults();
        s.ai.connection.base_url = Some("not a url".into());
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(errs.iter().any(|e| e.field == "ai.connection.base_url"));
    }

    #[test]
    fn service_tier_override_resolves_for_connection_and_dreamer() {
        let mut overrides = overrides::SettingsOverrides::default();
        overrides.ai.connection.service_tier = Some(Some("flex".to_string()));
        overrides.ai.dreamer.service_tier = Some(Some("priority".to_string()));
        let s = Settings::resolve(&Settings::compiled_defaults(), &overrides);
        assert_eq!(s.ai.connection.service_tier.as_deref(), Some("flex"));
        assert_eq!(s.ai.dreamer.service_tier.as_deref(), Some("priority"));
    }

    #[test]
    fn service_tier_explicit_clear_resolves_to_none() {
        // Set a non-None default to prove `Some(None)` clears it.
        let mut defaults = Settings::compiled_defaults();
        defaults.ai.connection.service_tier = Some("flex".to_string());
        let mut overrides = overrides::SettingsOverrides::default();
        overrides.ai.connection.service_tier = Some(None);
        let s = Settings::resolve(&defaults, &overrides);
        assert!(s.ai.connection.service_tier.is_none());
    }

    #[test]
    fn validate_rejects_unknown_service_tier_value() {
        let mut s = Settings::compiled_defaults();
        s.ai.connection.service_tier = Some("turbo".to_string());
        let err = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("'turbo' must be rejected");
        assert!(
            err.iter().any(|e| e.field == "ai.connection.service_tier"),
            "expected field error for ai.connection.service_tier, got {err:?}"
        );
    }

    #[test]
    fn twitch_override_resolves() {
        use crate::settings::overrides::TwitchOverrides;
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            twitch: TwitchOverrides {
                expected_latency: Some(250),
                hidden_admins: Some(vec!["111".into(), "222".into()]),
                admin_channel: Some(Some("admins".into())),
                ai_channel: Some(None), // explicit clear
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let r = Settings::resolve(&defaults, &overrides);
        assert_eq!(r.twitch.expected_latency, 250);
        assert_eq!(r.twitch.hidden_admins, vec!["111", "222"]);
        assert_eq!(r.twitch.admin_channel.as_deref(), Some("admins"));
        assert!(r.twitch.ai_channel.is_none());
    }

    #[test]
    fn aviationstack_suspend_web_resolve() {
        use crate::settings::overrides::{
            AviationstackOverrides, SuspendOverrides, WebRuntimeOverrides,
        };
        let defaults = Settings::compiled_defaults();
        let overrides = SettingsOverrides {
            aviationstack: AviationstackOverrides {
                enabled: Some(true),
                timeout_secs: Some(7),
                ..Default::default()
            },
            suspend: SuspendOverrides {
                default_duration_secs: Some(900),
            },
            web: WebRuntimeOverrides {
                session_ttl_secs: Some(3600 * 12),
                mod_check_refresh_secs: Some(120),
            },
            ..SettingsOverrides::default()
        };
        let r = Settings::resolve(&defaults, &overrides);
        assert!(r.aviationstack.enabled);
        assert_eq!(r.aviationstack.timeout_secs, 7);
        assert_eq!(r.aviationstack.base_url, defaults.aviationstack.base_url);
        assert_eq!(r.suspend.default_duration_secs, 900);
        assert_eq!(r.web.session_ttl_secs, 3600 * 12);
        assert_eq!(r.web.mod_check_refresh_secs, 120);
    }

    #[test]
    fn validate_rejects_expected_latency_over_1000() {
        let mut s = Settings::compiled_defaults();
        s.twitch.expected_latency = 1500;
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(errs.iter().any(|e| e.field == "twitch.expected_latency"));
    }

    #[test]
    fn validate_rejects_admin_channel_equal_to_bootstrap_channel() {
        let mut s = Settings::compiled_defaults();
        s.twitch.admin_channel = Some("test".into());
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(errs.iter().any(|e| e.field == "twitch.admin_channel"));
    }

    #[test]
    fn validate_rejects_ai_channel_equal_to_admin_channel() {
        let mut s = Settings::compiled_defaults();
        s.twitch.admin_channel = Some("admins".into());
        s.twitch.ai_channel = Some("admins".into());
        let errs = s
            .validate(&ValidationContext {
                channel: "main".into(),
            })
            .expect_err("must fail");
        assert!(errs.iter().any(|e| e.field == "twitch.ai_channel"));
    }

    #[test]
    fn validate_rejects_suspend_out_of_range() {
        let mut s = Settings::compiled_defaults();
        s.suspend.default_duration_secs = 0;
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(
            errs.iter()
                .any(|e| e.field == "suspend.default_duration_secs")
        );
    }

    #[test]
    fn validate_rejects_invalid_aviationstack_url() {
        let mut s = Settings::compiled_defaults();
        s.aviationstack.enabled = true;
        s.aviationstack.base_url = "not a url".into();
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(errs.iter().any(|e| e.field == "aviationstack.base_url"));
    }

    #[test]
    fn validate_disabled_aviationstack_ignores_invalid_base_url() {
        let mut s = Settings::compiled_defaults();
        s.aviationstack.enabled = false;
        s.aviationstack.base_url = "not a url".into();
        s.aviationstack.timeout_secs = 0;
        // Neither base_url nor timeout_secs should produce errors when disabled.
        s.validate(&ValidationContext {
            channel: "test".into(),
        })
        .expect("disabled aviationstack must not fail URL/timeout checks");
    }

    #[test]
    fn validate_both_channels_blank_produces_two_errors_not_three() {
        let mut s = Settings::compiled_defaults();
        s.twitch.admin_channel = Some("".into());
        s.twitch.ai_channel = Some("".into());
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("blank channels must fail");
        // Exactly one error per blank channel, no spurious "must differ" entry.
        assert!(
            errs.iter().any(|e| e.field == "twitch.admin_channel"),
            "expected error for blank admin_channel; got {errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.field == "twitch.ai_channel"),
            "expected error for blank ai_channel; got {errs:?}"
        );
        let differ_errors = errs
            .iter()
            .filter(|e| e.message.contains("must differ from twitch.admin_channel"))
            .count();
        assert_eq!(
            differ_errors, 0,
            "spurious 'must differ' error must not fire when both channels are blank"
        );
    }

    #[test]
    fn validate_rejects_web_session_ttl_out_of_range() {
        let mut s = Settings::compiled_defaults();
        s.web.session_ttl_secs = 60; // below 1h
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("must fail");
        assert!(errs.iter().any(|e| e.field == "web.session_ttl_secs"));
    }

    #[test]
    fn schedules_default_is_empty_vec() {
        let s = Settings::compiled_defaults();
        assert!(s.schedules.is_empty());
    }

    #[test]
    fn schedules_override_wholesale_replaces() {
        use crate::settings::ScheduleSettings;
        let defaults = Settings::compiled_defaults();
        let overrides = overrides::SettingsOverrides {
            schedules: Some(vec![ScheduleSettings {
                name: "noon".into(),
                message: "hi".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            }]),
            ..overrides::SettingsOverrides::default()
        };
        let r = Settings::resolve(&defaults, &overrides);
        assert_eq!(r.schedules.len(), 1);
        assert_eq!(r.schedules[0].name, "noon");
    }

    #[test]
    fn schedules_none_override_resolves_to_defaults() {
        let defaults = Settings::compiled_defaults();
        let overrides = overrides::SettingsOverrides::default();
        let r = Settings::resolve(&defaults, &overrides);
        assert!(r.schedules.is_empty());
    }

    #[test]
    fn validate_rejects_duplicate_schedule_name() {
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![
            ScheduleSettings {
                name: "a".into(),
                message: "hi".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
            ScheduleSettings {
                name: "a".into(),
                message: "hi".into(),
                interval: "01:00".into(),
                enabled: true,
                ..Default::default()
            },
        ];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("duplicate name must fail");
        assert!(errs.iter().any(|e| e.field.starts_with("schedules[")));
    }

    #[test]
    fn validate_rejects_bad_interval() {
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![ScheduleSettings {
            name: "x".into(),
            message: "hi".into(),
            interval: "not-a-duration".into(),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("bad interval must fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].interval"));
    }

    #[test]
    fn validate_rejects_single_active_time_field() {
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![ScheduleSettings {
            name: "x".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            active_time_start: Some("09:00".into()),
            active_time_end: None,
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("orphan active_time_start must fail");
        assert!(
            errs.iter()
                .any(|e| e.field == "schedules[0].active_time_end")
        );
    }

    #[test]
    fn validate_accepts_disabled_schedule_even_if_malformed_dates() {
        // Disabled rows still need a parseable interval (cheapest invariant
        // to keep the dashboard's add-form honest), but optional date strings
        // are not validated.
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![ScheduleSettings {
            name: "x".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: false,
            ..Default::default()
        }];
        s.validate(&ValidationContext {
            channel: "test".into(),
        })
        .expect("disabled schedule with valid required fields must pass");
    }

    #[test]
    fn validate_empty_schedules_is_ok() {
        let s = Settings::compiled_defaults();
        s.validate(&ValidationContext {
            channel: "test".into(),
        })
        .expect("empty schedules must pass");
    }

    #[test]
    fn validate_rejects_end_date_before_start_date() {
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![ScheduleSettings {
            name: "x".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            start_date: Some("2027-01-01T00:00:00".into()),
            end_date: Some("2026-01-01T00:00:00".into()),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("inverted date range must fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].end_date"));
    }

    #[test]
    fn validate_rejects_schedule_name_with_slash() {
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![ScheduleSettings {
            name: "morning/evening".into(),
            message: "hi".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("slash in name must fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }

    #[test]
    fn validate_rejects_schedule_name_with_ampersand() {
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![ScheduleSettings {
            name: "foo&bar".into(),
            message: "m".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("should fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }

    #[test]
    fn validate_rejects_schedule_name_with_space() {
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![ScheduleSettings {
            name: "foo bar".into(),
            message: "m".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("should fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }

    #[test]
    fn validate_rejects_schedule_name_with_control_char() {
        use crate::settings::ScheduleSettings;
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![ScheduleSettings {
            name: "foo\nbar".into(),
            message: "m".into(),
            interval: "01:00".into(),
            enabled: true,
            ..Default::default()
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("should fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }
}
