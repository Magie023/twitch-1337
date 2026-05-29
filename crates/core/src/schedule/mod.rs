//! Schedule data model and runtime engine.
//!
//! `Schedule` rows live in `settings.ron`; `ScheduleRuntime` lives in a
//! sibling `schedule_runtime.ron` (telemetry only, not config).

#[allow(clippy::module_inception)] // module `schedule` inside module `schedule`
pub mod schedule;
pub mod telemetry;
pub mod trigger;
pub mod weekday;

pub use schedule::Schedule;
pub use telemetry::{ScheduleRuntime, TelemetryStore};
pub use trigger::Trigger;
pub use weekday::WeekdaySet;
