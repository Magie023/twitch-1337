//! Re-export of the schedule data model owned by `crate::schedule`.
//!
//! Lives here so that `Settings` and `SettingsOverrides` only depend on
//! the type aliases — the schedule module is the source of truth for
//! shape, validation, and engine logic.

pub use crate::schedule::Schedule;
