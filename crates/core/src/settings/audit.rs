//! Append-only JSON-lines audit log for settings changes.

#[cfg(any(test, feature = "testing"))]
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{Schedule, Settings};

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub ts: DateTime<chrono_tz::Tz>,
    pub actor_id: String,
    pub actor_login: String,
    pub changes: Vec<AuditChange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditChange {
    pub key: String,
    /// Old value, JSON-encoded. `null` if the field had no override (was at default).
    pub old: serde_json::Value,
    /// New value, JSON-encoded.
    pub new: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
}

pub trait AuditLog: Send + Sync {
    fn append(&self, entry: &AuditEntry) -> Result<(), AuditError>;
}

pub struct FileAuditLog {
    path: std::path::PathBuf,
}

impl FileAuditLog {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl AuditLog for FileAuditLog {
    fn append(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        use std::io::Write as _;
        let line = serde_json::to_string(entry)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        f.sync_all()?;
        Ok(())
    }
}

#[cfg(any(test, feature = "testing"))]
pub struct MemoryAuditLog {
    entries: Mutex<Vec<AuditEntry>>,
}

#[cfg(any(test, feature = "testing"))]
impl MemoryAuditLog {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    pub fn snapshot(&self) -> Vec<AuditEntry> {
        self.entries.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "testing"))]
impl Default for MemoryAuditLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl AuditLog for MemoryAuditLog {
    fn append(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        self.entries.lock().unwrap().push(entry.clone());
        Ok(())
    }
}

pub fn berlin_now(now_utc: DateTime<Utc>) -> DateTime<chrono_tz::Tz> {
    now_utc.with_timezone(&chrono_tz::Europe::Berlin)
}

/// Produce the per-field `AuditChange` list between two resolved snapshots.
/// Only fields that actually differ are emitted; toggle-cards (prefill/web/
/// emotes) diff the whole block on a None↔Some transition and leaf-by-leaf
/// when both are present. Schedules are diffed by name across the vec.
pub(super) fn diff_changes(prior: &Settings, next: &Settings) -> Vec<AuditChange> {
    let mut out = Vec::new();
    macro_rules! cmp {
        ($key:literal, $prior:expr, $next:expr) => {
            if $prior != $next {
                out.push(AuditChange {
                    key: $key.into(),
                    old: serde_json::to_value($prior).expect(concat!("serialize prior ", $key)),
                    new: serde_json::to_value($next).expect(concat!("serialize next ", $key)),
                });
            }
        };
    }
    cmp!("cooldowns.ai", prior.cooldowns.ai, next.cooldowns.ai);
    cmp!("cooldowns.news", prior.cooldowns.news, next.cooldowns.news);
    cmp!("cooldowns.up", prior.cooldowns.up, next.cooldowns.up);
    cmp!(
        "cooldowns.feedback",
        prior.cooldowns.feedback,
        next.cooldowns.feedback
    );
    cmp!(
        "cooldowns.doener",
        prior.cooldowns.doener,
        next.cooldowns.doener
    );
    cmp!("pings.cooldown", prior.pings.cooldown, next.pings.cooldown);
    cmp!("pings.public", prior.pings.public, next.pings.public);
    // AI connection
    cmp!(
        "ai.connection.backend",
        prior.ai.connection.backend,
        next.ai.connection.backend
    );
    cmp!(
        "ai.connection.base_url",
        prior.ai.connection.base_url.as_deref(),
        next.ai.connection.base_url.as_deref()
    );
    cmp!(
        "ai.connection.model",
        prior.ai.connection.model.as_str(),
        next.ai.connection.model.as_str()
    );
    cmp!(
        "ai.connection.timeout",
        prior.ai.connection.timeout,
        next.ai.connection.timeout
    );
    cmp!(
        "ai.connection.reasoning_effort",
        prior.ai.connection.reasoning_effort.as_deref(),
        next.ai.connection.reasoning_effort.as_deref()
    );
    cmp!(
        "ai.connection.service_tier",
        prior.ai.connection.service_tier.as_deref(),
        next.ai.connection.service_tier.as_deref()
    );
    // AI behavior
    cmp!(
        "ai.behavior.max_turn_rounds",
        prior.ai.behavior.max_turn_rounds,
        next.ai.behavior.max_turn_rounds
    );
    cmp!(
        "ai.behavior.max_writes_per_turn",
        prior.ai.behavior.max_writes_per_turn,
        next.ai.behavior.max_writes_per_turn
    );
    cmp!(
        "ai.behavior.persona_name",
        prior.ai.behavior.persona_name.as_str(),
        next.ai.behavior.persona_name.as_str()
    );
    // AI history
    cmp!(
        "ai.history.length",
        prior.ai.history.length,
        next.ai.history.length
    );
    cmp!(
        "ai.history.ai_channel_length",
        prior.ai.history.ai_channel_length,
        next.ai.history.ai_channel_length
    );
    // AI memory
    cmp!(
        "ai.memory.soul_bytes",
        prior.ai.memory.soul_bytes,
        next.ai.memory.soul_bytes
    );
    cmp!(
        "ai.memory.lore_bytes",
        prior.ai.memory.lore_bytes,
        next.ai.memory.lore_bytes
    );
    cmp!(
        "ai.memory.user_bytes",
        prior.ai.memory.user_bytes,
        next.ai.memory.user_bytes
    );
    cmp!(
        "ai.memory.state_bytes",
        prior.ai.memory.state_bytes,
        next.ai.memory.state_bytes
    );
    cmp!(
        "ai.memory.inject_byte_budget",
        prior.ai.memory.inject_byte_budget,
        next.ai.memory.inject_byte_budget
    );
    cmp!(
        "ai.memory.max_state_files",
        prior.ai.memory.max_state_files,
        next.ai.memory.max_state_files
    );
    // AI dreamer
    cmp!(
        "ai.dreamer.enabled",
        prior.ai.dreamer.enabled,
        next.ai.dreamer.enabled
    );
    cmp!(
        "ai.dreamer.model",
        prior.ai.dreamer.model.as_deref(),
        next.ai.dreamer.model.as_deref()
    );
    cmp!(
        "ai.dreamer.reasoning_effort",
        prior.ai.dreamer.reasoning_effort.as_deref(),
        next.ai.dreamer.reasoning_effort.as_deref()
    );
    cmp!(
        "ai.dreamer.service_tier",
        prior.ai.dreamer.service_tier.as_deref(),
        next.ai.dreamer.service_tier.as_deref()
    );
    cmp!(
        "ai.dreamer.run_at",
        prior.ai.dreamer.run_at.as_str(),
        next.ai.dreamer.run_at.as_str()
    );
    cmp!(
        "ai.dreamer.timeout_secs",
        prior.ai.dreamer.timeout_secs,
        next.ai.dreamer.timeout_secs
    );
    cmp!(
        "ai.dreamer.max_rounds",
        prior.ai.dreamer.max_rounds,
        next.ai.dreamer.max_rounds
    );
    // AI prefill (toggle-card: diff the whole block as a single key on None<->Some
    // transitions, then leaf-by-leaf when both are Some)
    match (&prior.ai.prefill, &next.ai.prefill) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => {
            out.push(AuditChange {
                key: "ai.prefill".into(),
                old: serde_json::to_value(&prior.ai.prefill).expect("serialize prior ai.prefill"),
                new: serde_json::to_value(&next.ai.prefill).expect("serialize next ai.prefill"),
            });
        }
        (Some(p), Some(n)) => {
            cmp!(
                "ai.prefill.base_url",
                p.base_url.as_str(),
                n.base_url.as_str()
            );
            if p.threshold.to_bits() != n.threshold.to_bits() {
                out.push(AuditChange {
                    key: "ai.prefill.threshold".into(),
                    old: serde_json::to_value(p.threshold)
                        .expect("serialize prior ai.prefill.threshold"),
                    new: serde_json::to_value(n.threshold)
                        .expect("serialize next ai.prefill.threshold"),
                });
            }
        }
    }
    // AI web
    match (&prior.ai.web, &next.ai.web) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => {
            out.push(AuditChange {
                key: "ai.web".into(),
                old: serde_json::to_value(&prior.ai.web).expect("serialize prior ai.web"),
                new: serde_json::to_value(&next.ai.web).expect("serialize next ai.web"),
            });
        }
        (Some(p), Some(n)) => {
            cmp!("ai.web.base_url", p.base_url.as_str(), n.base_url.as_str());
            cmp!("ai.web.timeout", p.timeout, n.timeout);
            cmp!("ai.web.max_results", p.max_results, n.max_results);
            cmp!("ai.web.max_rounds", p.max_rounds, n.max_rounds);
            cmp!("ai.web.cache_ttl_secs", p.cache_ttl_secs, n.cache_ttl_secs);
            cmp!("ai.web.cache_capacity", p.cache_capacity, n.cache_capacity);
        }
    }
    // AI emotes
    match (&prior.ai.emotes, &next.ai.emotes) {
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => {
            out.push(AuditChange {
                key: "ai.emotes".into(),
                old: serde_json::to_value(&prior.ai.emotes).expect("serialize prior ai.emotes"),
                new: serde_json::to_value(&next.ai.emotes).expect("serialize next ai.emotes"),
            });
        }
        (Some(p), Some(n)) => {
            cmp!(
                "ai.emotes.include_global",
                p.include_global,
                n.include_global
            );
            cmp!(
                "ai.emotes.refresh_interval_secs",
                p.refresh_interval_secs,
                n.refresh_interval_secs
            );
            cmp!(
                "ai.emotes.max_prompt_emotes",
                p.max_prompt_emotes,
                n.max_prompt_emotes
            );
            cmp!(
                "ai.emotes.min_baseline_emotes",
                p.min_baseline_emotes,
                n.min_baseline_emotes
            );
            cmp!(
                "ai.emotes.pinned_emotes",
                p.pinned_emotes.as_slice(),
                n.pinned_emotes.as_slice()
            );
            cmp!(
                "ai.emotes.base_url",
                p.base_url.as_deref(),
                n.base_url.as_deref()
            );
        }
    }
    // AI media
    cmp!(
        "ai.media.model",
        prior.ai.media.model.as_str(),
        next.ai.media.model.as_str()
    );
    cmp!(
        "ai.media.timeout",
        prior.ai.media.timeout,
        next.ai.media.timeout
    );
    cmp!(
        "ai.media.max_image_size",
        prior.ai.media.max_image_size,
        next.ai.media.max_image_size
    );
    cmp!(
        "ai.media.max_pdf_size",
        prior.ai.media.max_pdf_size,
        next.ai.media.max_pdf_size
    );
    cmp!(
        "ai.media.max_audio_size",
        prior.ai.media.max_audio_size,
        next.ai.media.max_audio_size
    );
    cmp!(
        "ai.media.max_video_size",
        prior.ai.media.max_video_size,
        next.ai.media.max_video_size
    );
    cmp!(
        "ai.media.max_text_size",
        prior.ai.media.max_text_size,
        next.ai.media.max_text_size
    );
    // Twitch runtime
    cmp!(
        "twitch.expected_latency",
        prior.twitch.expected_latency,
        next.twitch.expected_latency
    );
    cmp!(
        "twitch.hidden_admins",
        prior.twitch.hidden_admins.as_slice(),
        next.twitch.hidden_admins.as_slice()
    );
    cmp!(
        "twitch.viewer_allowlist",
        prior.twitch.viewer_allowlist.as_slice(),
        next.twitch.viewer_allowlist.as_slice()
    );
    cmp!(
        "twitch.admin_channel",
        prior.twitch.admin_channel.as_deref(),
        next.twitch.admin_channel.as_deref()
    );
    cmp!(
        "twitch.ai_channel",
        prior.twitch.ai_channel.as_deref(),
        next.twitch.ai_channel.as_deref()
    );
    // Aviationstack
    cmp!(
        "aviationstack.enabled",
        prior.aviationstack.enabled,
        next.aviationstack.enabled
    );
    cmp!(
        "aviationstack.base_url",
        prior.aviationstack.base_url.as_str(),
        next.aviationstack.base_url.as_str()
    );
    cmp!(
        "aviationstack.timeout_secs",
        prior.aviationstack.timeout_secs,
        next.aviationstack.timeout_secs
    );
    // Suspend
    cmp!(
        "suspend.default_duration_secs",
        prior.suspend.default_duration_secs,
        next.suspend.default_duration_secs
    );
    // Web runtime
    cmp!(
        "web.session_ttl_secs",
        prior.web.session_ttl_secs,
        next.web.session_ttl_secs
    );
    cmp!(
        "web.mod_check_refresh_secs",
        prior.web.mod_check_refresh_secs,
        next.web.mod_check_refresh_secs
    );
    // Schedules — diff by name across vec
    {
        use std::collections::{BTreeMap, BTreeSet};
        let prior_map: BTreeMap<&str, &Schedule> = prior
            .schedules
            .iter()
            .map(|s| (s.name.as_str(), s))
            .collect();
        let next_map: BTreeMap<&str, &Schedule> = next
            .schedules
            .iter()
            .map(|s| (s.name.as_str(), s))
            .collect();
        let all_names: BTreeSet<&str> = prior_map
            .keys()
            .copied()
            .chain(next_map.keys().copied())
            .collect();
        for name in all_names {
            let p = prior_map.get(name).copied();
            let n = next_map.get(name).copied();
            if p != n {
                out.push(AuditChange {
                    key: format!("schedules.{name}"),
                    old: p
                        .map(|s| serde_json::to_value(s).expect("serialize prior schedule"))
                        .unwrap_or(serde_json::Value::Null),
                    new: n
                        .map(|s| serde_json::to_value(s).expect("serialize next schedule"))
                        .unwrap_or(serde_json::Value::Null),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> AuditEntry {
        AuditEntry {
            ts: berlin_now("2026-05-12T13:37:00Z".parse::<DateTime<Utc>>().unwrap()),
            actor_id: "12345678".into(),
            actor_login: "chronophylos".into(),
            changes: vec![AuditChange {
                key: "cooldowns.ai".into(),
                old: serde_json::Value::Number(30.into()),
                new: serde_json::Value::Number(15.into()),
            }],
        }
    }

    #[test]
    fn memory_log_records_entries() {
        let log = MemoryAuditLog::new();
        let e = sample_entry();
        log.append(&e).expect("append");
        log.append(&e).expect("append twice");
        assert_eq!(log.snapshot().len(), 2);
    }

    #[test]
    fn file_log_appends_one_json_line_per_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.log");
        let log = FileAuditLog::new(&path);
        let e = sample_entry();
        log.append(&e).expect("first");
        log.append(&e).expect("second");
        let body = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let parsed: serde_json::Value = serde_json::from_str(line).expect("valid json");
            assert_eq!(parsed["actor_id"], "12345678");
            assert_eq!(parsed["changes"][0]["key"], "cooldowns.ai");
        }
    }

    #[test]
    fn file_log_survives_truncation_between_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.log");
        let log = FileAuditLog::new(&path);
        log.append(&sample_entry()).expect("first");
        std::fs::remove_file(&path).expect("remove");
        log.append(&sample_entry()).expect("second after unlink");
        assert!(path.exists());
        let lines = std::fs::read_to_string(&path)
            .expect("read")
            .lines()
            .count();
        assert_eq!(lines, 1);
    }

    fn make_schedule(name: &str, message: &str) -> Schedule {
        use crate::schedule::{Trigger, WeekdaySet};
        Schedule {
            name: name.into(),
            message: message.into(),
            trigger: Trigger::Interval {
                every: std::time::Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: None,
                active_to: None,
            },
            start_date: None,
            end_date: None,
            enabled: true,
        }
    }

    #[test]
    fn diff_emits_service_tier_changes() {
        let prior = Settings::compiled_defaults();
        let mut next = Settings::compiled_defaults();
        next.ai.connection.service_tier = Some("flex".to_string());
        next.ai.dreamer.service_tier = Some("priority".to_string());
        let changes = diff_changes(&prior, &next);
        let keys: Vec<&str> = changes.iter().map(|c| c.key.as_str()).collect();
        assert!(keys.contains(&"ai.connection.service_tier"), "got {keys:?}");
        assert!(keys.contains(&"ai.dreamer.service_tier"), "got {keys:?}");
    }

    #[test]
    fn diff_emits_emotes_pinned_change() {
        use crate::settings::ai::AiEmotes;
        let mut prior = Settings::compiled_defaults();
        let mut next = Settings::compiled_defaults();
        prior.ai.emotes = Some(AiEmotes::default());
        next.ai.emotes = Some(AiEmotes {
            pinned_emotes: vec!["PepeLa".into(), "okjj".into(), "Clueless".into()],
            ..AiEmotes::default()
        });
        let changes = diff_changes(&prior, &next);
        let keys: Vec<&str> = changes.iter().map(|c| c.key.as_str()).collect();
        assert!(keys.contains(&"ai.emotes.pinned_emotes"), "got {keys:?}");
    }

    #[test]
    fn diff_emits_twitch_section_changes() {
        let prior = Settings::compiled_defaults();
        let mut next = Settings::compiled_defaults();
        next.twitch.expected_latency = 200;
        next.twitch.hidden_admins = vec!["111".into()];
        next.twitch.admin_channel = Some("admins".into());
        let changes = diff_changes(&prior, &next);
        let keys: Vec<&str> = changes.iter().map(|c| c.key.as_str()).collect();
        assert!(
            keys.iter().any(|k| k.starts_with("twitch.")),
            "diff must include at least one twitch.* entry; got {keys:?}"
        );
        assert!(keys.contains(&"twitch.expected_latency"), "got {keys:?}");
        assert!(keys.contains(&"twitch.hidden_admins"), "got {keys:?}");
        assert!(keys.contains(&"twitch.admin_channel"), "got {keys:?}");
    }

    #[test]
    fn diff_emits_schedule_added_removed_modified() {
        let mut prior = Settings::compiled_defaults();
        let mut next = Settings::compiled_defaults();
        prior.schedules = vec![make_schedule("keep", "old"), make_schedule("drop", "x")];
        next.schedules = vec![
            make_schedule("keep", "new"), // modified
            make_schedule("add", "y"),
        ];
        let changes = diff_changes(&prior, &next);
        let keys: Vec<&str> = changes.iter().map(|c| c.key.as_str()).collect();
        assert!(keys.contains(&"schedules.keep"), "got {keys:?}");
        assert!(keys.contains(&"schedules.drop"), "got {keys:?}");
        assert!(keys.contains(&"schedules.add"), "got {keys:?}");
    }
}
