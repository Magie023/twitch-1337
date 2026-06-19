//! Dashboard-managed runtime settings.
//!
//! `Settings` is the fully-resolved snapshot read by command handlers via
//! a `SettingsHandle = Arc<ArcSwap<Settings>>`. Sparse `SettingsOverrides`
//! (see `overrides.rs`) live on disk at `$DATA_DIR/settings.ron`; missing
//! fields fall through to `compiled_defaults()`.
//!
//! Writes go through `SettingsStore::apply` (see `store.rs`) which
//! validates, atomically persists, swaps the handle, and appends an audit
//! entry. Override → effective resolution lives in `resolve.rs`; per-field
//! validation lives in `validation.rs`.

pub mod ai;
pub mod audit;
pub mod aviationstack;
pub mod migrate;
pub mod overrides;
mod resolve;
pub mod schedules;
pub mod store;
pub mod suspend;
pub mod twitch;
mod validation;
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
pub use schedules::Schedule;
pub use store::{Actor, SettingsStore};
pub use suspend::SuspendSettings;
pub use twitch::TwitchRuntime;
pub use validation::ValidationContext;
pub use web::WebRuntime;

use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type SettingsHandle = Arc<ArcSwap<Settings>>;

pub const SCHEMA_VERSION: u32 = 4;

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
    pub schedules: Vec<Schedule>,
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
}

#[cfg(any(test, feature = "testing"))]
pub fn test_handle() -> SettingsHandle {
    Arc::new(ArcSwap::from_pointee(Settings::compiled_defaults()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_defaults_v4_layout() {
        let s = Settings::compiled_defaults();
        assert_eq!(s.schema_version, 4);
        assert_eq!(s.ai, AiSettings::default());
        assert_eq!(s.twitch, TwitchRuntime::default());
        assert_eq!(s.aviationstack, AviationstackSettings::default());
        assert_eq!(s.suspend, SuspendSettings::default());
        assert_eq!(s.web, WebRuntime::default());
        assert!(s.schedules.is_empty());
    }

    #[test]
    fn schedules_default_is_empty_vec() {
        let s = Settings::compiled_defaults();
        assert!(s.schedules.is_empty());
    }
}
