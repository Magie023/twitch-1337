//! Sparse override patch-merge: fold a submitted `SettingsOverrides` patch into
//! the on-disk override tree, field by field. `Some` in the patch wins; `None`
//! leaves the existing value untouched (except list/option fields, which the
//! patch replaces wholesale when present).

use crate::settings::overrides::SettingsOverrides;

pub(super) fn merge_into(into: &mut SettingsOverrides, patch: &SettingsOverrides) {
    if let Some(v) = patch.cooldowns.ai {
        into.cooldowns.ai = Some(v);
    }
    if let Some(v) = patch.cooldowns.news {
        into.cooldowns.news = Some(v);
    }
    if let Some(v) = patch.cooldowns.up {
        into.cooldowns.up = Some(v);
    }
    if let Some(v) = patch.cooldowns.feedback {
        into.cooldowns.feedback = Some(v);
    }
    if let Some(v) = patch.cooldowns.doener {
        into.cooldowns.doener = Some(v);
    }
    if let Some(v) = patch.pings.cooldown {
        into.pings.cooldown = Some(v);
    }
    if let Some(v) = patch.pings.public {
        into.pings.public = Some(v);
    }
    // AI connection
    if let Some(v) = patch.ai.connection.backend {
        into.ai.connection.backend = Some(v);
    }
    if patch.ai.connection.base_url.is_some() {
        into.ai.connection.base_url = patch.ai.connection.base_url.clone();
    }
    if let Some(v) = patch.ai.connection.model.as_ref() {
        into.ai.connection.model = Some(v.clone());
    }
    if let Some(v) = patch.ai.connection.timeout {
        into.ai.connection.timeout = Some(v);
    }
    if patch.ai.connection.reasoning_effort.is_some() {
        into.ai.connection.reasoning_effort = patch.ai.connection.reasoning_effort.clone();
    }
    if patch.ai.connection.service_tier.is_some() {
        into.ai.connection.service_tier = patch.ai.connection.service_tier.clone();
    }
    // AI behavior
    if let Some(v) = patch.ai.behavior.max_turn_rounds {
        into.ai.behavior.max_turn_rounds = Some(v);
    }
    if let Some(v) = patch.ai.behavior.max_writes_per_turn {
        into.ai.behavior.max_writes_per_turn = Some(v);
    }
    if let Some(v) = &patch.ai.behavior.persona_name {
        into.ai.behavior.persona_name = Some(v.clone());
    }
    // AI history
    if let Some(v) = patch.ai.history.length {
        into.ai.history.length = Some(v);
    }
    if let Some(v) = patch.ai.history.ai_channel_length {
        into.ai.history.ai_channel_length = Some(v);
    }
    // AI memory
    if let Some(v) = patch.ai.memory.soul_bytes {
        into.ai.memory.soul_bytes = Some(v);
    }
    if let Some(v) = patch.ai.memory.lore_bytes {
        into.ai.memory.lore_bytes = Some(v);
    }
    if let Some(v) = patch.ai.memory.user_bytes {
        into.ai.memory.user_bytes = Some(v);
    }
    if let Some(v) = patch.ai.memory.state_bytes {
        into.ai.memory.state_bytes = Some(v);
    }
    if let Some(v) = patch.ai.memory.inject_byte_budget {
        into.ai.memory.inject_byte_budget = Some(v);
    }
    if let Some(v) = patch.ai.memory.max_state_files {
        into.ai.memory.max_state_files = Some(v);
    }
    // AI dreamer
    if let Some(v) = patch.ai.dreamer.enabled {
        into.ai.dreamer.enabled = Some(v);
    }
    if patch.ai.dreamer.model.is_some() {
        into.ai.dreamer.model = patch.ai.dreamer.model.clone();
    }
    if patch.ai.dreamer.reasoning_effort.is_some() {
        into.ai.dreamer.reasoning_effort = patch.ai.dreamer.reasoning_effort.clone();
    }
    if patch.ai.dreamer.service_tier.is_some() {
        into.ai.dreamer.service_tier = patch.ai.dreamer.service_tier.clone();
    }
    if let Some(v) = patch.ai.dreamer.run_at.as_ref() {
        into.ai.dreamer.run_at = Some(v.clone());
    }
    if let Some(v) = patch.ai.dreamer.timeout_secs {
        into.ai.dreamer.timeout_secs = Some(v);
    }
    if let Some(v) = patch.ai.dreamer.max_rounds {
        into.ai.dreamer.max_rounds = Some(v);
    }
    // AI prefill
    if let Some(v) = patch.ai.prefill.enabled {
        into.ai.prefill.enabled = Some(v);
    }
    if let Some(v) = patch.ai.prefill.base_url.as_ref() {
        into.ai.prefill.base_url = Some(v.clone());
    }
    if let Some(v) = patch.ai.prefill.threshold {
        into.ai.prefill.threshold = Some(v);
    }
    // AI web
    if let Some(v) = patch.ai.web.enabled {
        into.ai.web.enabled = Some(v);
    }
    if let Some(v) = patch.ai.web.base_url.as_ref() {
        into.ai.web.base_url = Some(v.clone());
    }
    if let Some(v) = patch.ai.web.timeout {
        into.ai.web.timeout = Some(v);
    }
    if let Some(v) = patch.ai.web.max_results {
        into.ai.web.max_results = Some(v);
    }
    if let Some(v) = patch.ai.web.max_rounds {
        into.ai.web.max_rounds = Some(v);
    }
    if let Some(v) = patch.ai.web.cache_ttl_secs {
        into.ai.web.cache_ttl_secs = Some(v);
    }
    if let Some(v) = patch.ai.web.cache_capacity {
        into.ai.web.cache_capacity = Some(v);
    }
    // AI emotes
    if let Some(v) = patch.ai.emotes.enabled {
        into.ai.emotes.enabled = Some(v);
    }
    if let Some(v) = patch.ai.emotes.include_global {
        into.ai.emotes.include_global = Some(v);
    }
    if let Some(v) = patch.ai.emotes.refresh_interval_secs {
        into.ai.emotes.refresh_interval_secs = Some(v);
    }
    if let Some(v) = patch.ai.emotes.max_prompt_emotes {
        into.ai.emotes.max_prompt_emotes = Some(v);
    }
    if let Some(v) = patch.ai.emotes.min_baseline_emotes {
        into.ai.emotes.min_baseline_emotes = Some(v);
    }
    if patch.ai.emotes.pinned_emotes.is_some() {
        into.ai.emotes.pinned_emotes = patch.ai.emotes.pinned_emotes.clone();
    }
    if patch.ai.emotes.base_url.is_some() {
        into.ai.emotes.base_url = patch.ai.emotes.base_url.clone();
    }
    // AI media
    if let Some(v) = patch.ai.media.model.as_ref() {
        into.ai.media.model = Some(v.clone());
    }
    if let Some(v) = patch.ai.media.timeout {
        into.ai.media.timeout = Some(v);
    }
    if let Some(v) = patch.ai.media.max_image_size {
        into.ai.media.max_image_size = Some(v);
    }
    if let Some(v) = patch.ai.media.max_pdf_size {
        into.ai.media.max_pdf_size = Some(v);
    }
    if let Some(v) = patch.ai.media.max_audio_size {
        into.ai.media.max_audio_size = Some(v);
    }
    if let Some(v) = patch.ai.media.max_video_size {
        into.ai.media.max_video_size = Some(v);
    }
    if let Some(v) = patch.ai.media.max_text_size {
        into.ai.media.max_text_size = Some(v);
    }
    // Twitch
    if let Some(v) = patch.twitch.expected_latency {
        into.twitch.expected_latency = Some(v);
    }
    if patch.twitch.hidden_admins.is_some() {
        into.twitch.hidden_admins = patch.twitch.hidden_admins.clone();
    }
    if patch.twitch.viewer_allowlist.is_some() {
        into.twitch.viewer_allowlist = patch.twitch.viewer_allowlist.clone();
    }
    if patch.twitch.admin_channel.is_some() {
        into.twitch.admin_channel = patch.twitch.admin_channel.clone();
    }
    if patch.twitch.ai_channel.is_some() {
        into.twitch.ai_channel = patch.twitch.ai_channel.clone();
    }
    // Aviationstack
    if let Some(v) = patch.aviationstack.enabled {
        into.aviationstack.enabled = Some(v);
    }
    if let Some(v) = patch.aviationstack.base_url.as_ref() {
        into.aviationstack.base_url = Some(v.clone());
    }
    if let Some(v) = patch.aviationstack.timeout_secs {
        into.aviationstack.timeout_secs = Some(v);
    }
    // Suspend
    if let Some(v) = patch.suspend.default_duration_secs {
        into.suspend.default_duration_secs = Some(v);
    }
    // Web
    if let Some(v) = patch.web.session_ttl_secs {
        into.web.session_ttl_secs = Some(v);
    }
    if let Some(v) = patch.web.mod_check_refresh_secs {
        into.web.mod_check_refresh_secs = Some(v);
    }
    // Schedules — wholesale replace
    if patch.schedules.is_some() {
        into.schedules = patch.schedules.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{Schedule, Trigger, WeekdaySet};
    use crate::settings::overrides::{
        AviationstackOverrides, SuspendOverrides, TwitchOverrides, WebRuntimeOverrides,
    };

    #[test]
    fn patch_round_trips_service_tier_on_connection_and_dreamer() {
        let mut into = SettingsOverrides::default();
        let mut patch = SettingsOverrides::default();
        patch.ai.connection.service_tier = Some(Some("flex".to_string()));
        patch.ai.dreamer.service_tier = Some(Some("priority".to_string()));
        merge_into(&mut into, &patch);
        assert_eq!(
            into.ai
                .connection
                .service_tier
                .as_ref()
                .and_then(|v| v.as_deref()),
            Some("flex")
        );
        assert_eq!(
            into.ai
                .dreamer
                .service_tier
                .as_ref()
                .and_then(|v| v.as_deref()),
            Some("priority")
        );
    }

    #[test]
    fn merge_twitch_overrides_replaces_lists() {
        let mut into = SettingsOverrides::default();
        into.twitch.hidden_admins = Some(vec!["old".into()]);
        let patch = SettingsOverrides {
            twitch: TwitchOverrides {
                hidden_admins: Some(vec!["new1".into(), "new2".into()]),
                expected_latency: Some(150),
                ..Default::default()
            },
            ..Default::default()
        };
        merge_into(&mut into, &patch);
        assert_eq!(into.twitch.expected_latency, Some(150));
        assert_eq!(
            into.twitch.hidden_admins.as_deref(),
            Some(&vec!["new1".to_string(), "new2".to_string()][..])
        );
    }

    #[test]
    fn merge_aviationstack_suspend_web() {
        let mut into = SettingsOverrides::default();
        let patch = SettingsOverrides {
            aviationstack: AviationstackOverrides {
                enabled: Some(true),
                ..Default::default()
            },
            suspend: SuspendOverrides {
                default_duration_secs: Some(900),
            },
            web: WebRuntimeOverrides {
                session_ttl_secs: Some(3600),
                mod_check_refresh_secs: Some(60),
            },
            ..Default::default()
        };
        merge_into(&mut into, &patch);
        assert_eq!(into.aviationstack.enabled, Some(true));
        assert_eq!(into.suspend.default_duration_secs, Some(900));
        assert_eq!(into.web.session_ttl_secs, Some(3600));
        assert_eq!(into.web.mod_check_refresh_secs, Some(60));
    }

    fn make_schedule(name: &str, message: &str) -> Schedule {
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
    fn merge_replaces_schedules_wholesale() {
        let mut into = SettingsOverrides {
            schedules: Some(vec![make_schedule("old", "m")]),
            ..Default::default()
        };
        let patch = SettingsOverrides {
            schedules: Some(vec![make_schedule("new1", "a"), make_schedule("new2", "b")]),
            ..Default::default()
        };
        merge_into(&mut into, &patch);
        let v = into.schedules.expect("Some");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "new1");
        assert_eq!(v[1].name, "new2");
    }

    #[test]
    fn merge_skips_schedules_when_patch_is_none() {
        let mut into = SettingsOverrides {
            schedules: Some(vec![make_schedule("keep", "m")]),
            ..Default::default()
        };
        let patch = SettingsOverrides::default();
        merge_into(&mut into, &patch);
        let v = into.schedules.expect("Some");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "keep");
    }
}
