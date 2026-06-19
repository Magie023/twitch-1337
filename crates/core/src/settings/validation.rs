//! Per-field validation for a resolved `Settings` snapshot.

use super::{AiSettings, FieldError, Settings};

/// Bootstrap-side context required for cross-field validation. `channel`
/// is the IRC channel from config.toml, used to enforce that
/// `twitch.admin_channel` and `twitch.ai_channel` differ from it.
#[derive(Debug, Clone)]
pub struct ValidationContext {
    pub channel: String,
}

/// Push a "must be lo..=hi seconds" `FieldError` when `v` is out of range.
/// Generic over the numeric type so any bounded `..._secs` field can use it.
fn bound<T: PartialOrd + std::fmt::Display>(
    name: &str,
    v: T,
    lo: T,
    hi: T,
    errs: &mut Vec<FieldError>,
) {
    if v < lo || v > hi {
        errs.push(FieldError {
            field: name.to_owned(),
            message: format!("must be {lo}..={hi} seconds (got {v})"),
        });
    }
}

impl Settings {
    pub fn validate(&self, ctx: &ValidationContext) -> Result<(), Vec<FieldError>> {
        let mut errs = Vec::new();
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
            // Per-row validation lives on `Schedule`.
            errs.extend(sc.validate(&prefix));
            // Uniqueness check (cross-row, so it stays here).
            if !sc.name.trim().is_empty() && !seen_names.insert(sc.name.trim()) {
                errs.push(FieldError {
                    field: format!("{prefix}.name"),
                    message: format!("duplicate name {:?}", sc.name.trim()),
                });
            }
        }
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }
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
        // Pinned core is injected before scoring/baseline; if it alone exceeds
        // the window cap there is no room left for any scored or baseline emote.
        // (Names absent from the glossary are dropped with a warning at the
        // provider, not rejected here — see SevenTvEmoteProvider.)
        if em.pinned_emotes.len() > em.max_prompt_emotes {
            err(
                errs,
                "ai.emotes.pinned_emotes",
                format!(
                    "must have <= max_prompt_emotes entries (got {}, max {})",
                    em.pinned_emotes.len(),
                    em.max_prompt_emotes
                ),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn validate_rejects_pinned_emotes_exceeding_max() {
        use crate::settings::ai::AiEmotes;
        let mut s = Settings::compiled_defaults();
        s.ai.emotes = Some(AiEmotes {
            max_prompt_emotes: 2,
            min_baseline_emotes: 0,
            pinned_emotes: vec!["A".into(), "B".into(), "C".into()],
            ..AiEmotes::default()
        });
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("3 pins > max 2 must fail");
        assert!(errs.iter().any(|e| e.field == "ai.emotes.pinned_emotes"));
    }

    #[test]
    fn validate_accepts_pinned_emotes_within_max() {
        use crate::settings::ai::AiEmotes;
        let mut s = Settings::compiled_defaults();
        s.ai.emotes = Some(AiEmotes {
            max_prompt_emotes: 20,
            pinned_emotes: vec!["PepeLa".into(), "okjj".into()],
            ..AiEmotes::default()
        });
        s.validate(&ValidationContext {
            channel: "test".into(),
        })
        .expect("2 pins <= max 20 must pass");
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
    fn validate_rejects_duplicate_schedule_name() {
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![
            Schedule {
                name: "a".into(),
                message: "hi".into(),
                trigger: Trigger::Interval {
                    every: std::time::Duration::from_secs(3600),
                    days: WeekdaySet::default(),
                    active_from: None,
                    active_to: None,
                },
                start_date: None,
                end_date: None,
                enabled: true,
            },
            Schedule {
                name: "a".into(),
                message: "hi".into(),
                trigger: Trigger::Interval {
                    every: std::time::Duration::from_secs(3600),
                    days: WeekdaySet::default(),
                    active_from: None,
                    active_to: None,
                },
                start_date: None,
                end_date: None,
                enabled: true,
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
    fn validate_rejects_single_active_time_field() {
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![Schedule {
            name: "x".into(),
            message: "hi".into(),
            trigger: Trigger::Interval {
                every: std::time::Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: Some(chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
                active_to: None,
            },
            start_date: None,
            end_date: None,
            enabled: true,
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("orphan active_from must fail");
        assert!(
            errs.iter()
                .any(|e| e.field == "schedules[0].trigger.active_to")
        );
    }

    #[test]
    fn validate_accepts_disabled_schedule() {
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![Schedule {
            name: "x".into(),
            message: "hi".into(),
            trigger: Trigger::Interval {
                every: std::time::Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: None,
                active_to: None,
            },
            start_date: None,
            end_date: None,
            enabled: false,
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
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![Schedule {
            name: "x".into(),
            message: "hi".into(),
            trigger: Trigger::Interval {
                every: std::time::Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: None,
                active_to: None,
            },
            start_date: Some(chrono::NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()),
            end_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            enabled: true,
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
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![Schedule {
            name: "morning/evening".into(),
            message: "hi".into(),
            trigger: Trigger::Interval {
                every: std::time::Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: None,
                active_to: None,
            },
            start_date: None,
            end_date: None,
            enabled: true,
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
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![Schedule {
            name: "foo&bar".into(),
            message: "m".into(),
            trigger: Trigger::Interval {
                every: std::time::Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: None,
                active_to: None,
            },
            start_date: None,
            end_date: None,
            enabled: true,
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
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![Schedule {
            name: "foo bar".into(),
            message: "m".into(),
            trigger: Trigger::Interval {
                every: std::time::Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: None,
                active_to: None,
            },
            start_date: None,
            end_date: None,
            enabled: true,
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
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let mut s = Settings::compiled_defaults();
        s.schedules = vec![Schedule {
            name: "foo\nbar".into(),
            message: "m".into(),
            trigger: Trigger::Interval {
                every: std::time::Duration::from_secs(3600),
                days: WeekdaySet::default(),
                active_from: None,
                active_to: None,
            },
            start_date: None,
            end_date: None,
            enabled: true,
        }];
        let errs = s
            .validate(&ValidationContext {
                channel: "test".into(),
            })
            .expect_err("should fail");
        assert!(errs.iter().any(|e| e.field == "schedules[0].name"));
    }
}
