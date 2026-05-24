//! Migrate a v1 config.toml [ai] block into a v2 AiOverrides patch.

use eyre::Result;
use humantime::parse_duration;

use super::overrides::*;

/// Extract every legacy `[ai]` key from a raw config.toml `Value` and return
/// a sparse `AiOverrides` patch suitable for `SettingsStore::apply`.
///
/// Returns `AiOverrides::default()` (a no-op patch) when the `[ai]` table is
/// absent — fresh installs trigger the migration sentinel without writing
/// anything noisy to the audit log.
pub fn migrate_legacy_ai(root: &toml::Value) -> Result<AiOverrides> {
    let mut out = AiOverrides::default();
    let Some(ai) = root.get("ai").and_then(|v| v.as_table()) else {
        return Ok(out);
    };

    fn s(t: &toml::Value, k: &str) -> Option<String> {
        t.get(k).and_then(|v| v.as_str()).map(str::to_owned)
    }
    fn u(t: &toml::Value, k: &str) -> Option<u64> {
        t.get(k)
            .and_then(toml::Value::as_integer)
            .and_then(|i| u64::try_from(i).ok())
    }
    fn usz(t: &toml::Value, k: &str) -> Option<usize> {
        t.get(k)
            .and_then(toml::Value::as_integer)
            .and_then(|i| usize::try_from(i).ok())
    }
    fn b(t: &toml::Value, k: &str) -> Option<bool> {
        t.get(k).and_then(toml::Value::as_bool)
    }
    fn f(t: &toml::Value, k: &str) -> Option<f64> {
        t.get(k).and_then(toml::Value::as_float)
    }

    let ai_val = toml::Value::Table(ai.clone());

    if let Some(backend) = s(&ai_val, "backend") {
        out.connection.backend = match backend.as_str() {
            "openai" => Some(super::ai::AiBackendKind::OpenAi),
            "ollama" => Some(super::ai::AiBackendKind::Ollama),
            _ => None,
        };
    }
    if let Some(url) = s(&ai_val, "base_url") {
        out.connection.base_url = Some(Some(url));
    }
    out.connection.model = s(&ai_val, "model");
    out.connection.timeout = u(&ai_val, "timeout");
    if let Some(re) = s(&ai_val, "reasoning_effort") {
        out.connection.reasoning_effort = Some(Some(re));
    }

    out.behavior.max_turn_rounds = usz(&ai_val, "max_turn_rounds");
    out.behavior.max_writes_per_turn = usz(&ai_val, "max_writes_per_turn");

    out.history.length = u(&ai_val, "history_length");
    out.history.ai_channel_length = u(&ai_val, "ai_channel_history_length");

    if let Some(mem) = ai.get("memory").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(mem.clone());
        out.memory.soul_bytes = usz(&v, "soul_bytes");
        out.memory.lore_bytes = usz(&v, "lore_bytes");
        out.memory.user_bytes = usz(&v, "user_bytes");
        out.memory.state_bytes = usz(&v, "state_bytes");
        out.memory.inject_byte_budget = usz(&v, "inject_byte_budget");
        out.memory.max_state_files = usz(&v, "max_state_files");
    }

    if let Some(d) = ai.get("dreamer").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(d.clone());
        out.dreamer.enabled = b(&v, "enabled");
        out.dreamer.model = s(&v, "model").map(Some);
        out.dreamer.reasoning_effort = s(&v, "reasoning_effort").map(Some);
        out.dreamer.run_at = s(&v, "run_at");
        out.dreamer.timeout_secs = u(&v, "timeout_secs");
        out.dreamer.max_rounds = usz(&v, "max_rounds");
    }

    if let Some(p) = ai.get("history_prefill").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(p.clone());
        out.prefill.enabled = Some(true);
        out.prefill.base_url = s(&v, "base_url");
        out.prefill.threshold = f(&v, "threshold");
    }

    if let Some(w) = ai.get("web").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(w.clone());
        out.web.enabled = b(&v, "enabled");
        out.web.base_url = s(&v, "base_url");
        out.web.timeout = u(&v, "timeout");
        out.web.max_results = usz(&v, "max_results");
        out.web.max_rounds = usz(&v, "max_rounds");
        out.web.cache_ttl_secs = u(&v, "cache_ttl_secs");
        out.web.cache_capacity = usz(&v, "cache_capacity");
    }

    if let Some(em) = ai.get("emotes").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(em.clone());
        out.emotes.enabled = b(&v, "enabled");
        out.emotes.include_global = b(&v, "include_global");
        out.emotes.refresh_interval_secs = u(&v, "refresh_interval_secs");
        out.emotes.max_prompt_emotes = usz(&v, "max_prompt_emotes");
        out.emotes.min_baseline_emotes = usz(&v, "min_baseline_emotes");
        out.emotes.base_url = s(&v, "base_url").map(Some);
    }

    if let Some(med) = ai.get("media").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(med.clone());
        out.media.model = s(&v, "model");
        out.media.timeout = u(&v, "timeout");
        out.media.max_image_size = s(&v, "max_image_size").and_then(|s| s.parse().ok());
        out.media.max_pdf_size = s(&v, "max_pdf_size").and_then(|s| s.parse().ok());
        out.media.max_audio_size = s(&v, "max_audio_size").and_then(|s| s.parse().ok());
        out.media.max_video_size = s(&v, "max_video_size").and_then(|s| s.parse().ok());
        out.media.max_text_size = s(&v, "max_text_size").and_then(|s| s.parse().ok());
    }

    Ok(out)
}

/// Extract every legacy non-secret, non-bootstrap key from a raw config.toml
/// `Value` and return a sparse `SettingsOverrides` patch suitable for
/// `SettingsStore::apply`. Pairs with `migrate_legacy_ai` for the `[ai]`
/// section; this helper covers the v3 migration of `[twitch]`, `[suspend]`,
/// `[aviationstack]` (non-secret), and `[web]` (non-bootstrap) keys.
pub fn migrate_legacy_config(root: &toml::Value) -> Result<super::overrides::SettingsOverrides> {
    let mut out = super::overrides::SettingsOverrides::default();

    if let Some(t) = root.get("twitch").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(t.clone());
        out.twitch.expected_latency = v
            .get("expected_latency")
            .and_then(toml::Value::as_integer)
            .and_then(|i| u32::try_from(i).ok());
        if let Some(arr) = v.get("hidden_admins").and_then(toml::Value::as_array) {
            out.twitch.hidden_admins = Some(
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect(),
            );
        }
        if let Some(arr) = v.get("viewer_allowlist").and_then(toml::Value::as_array) {
            out.twitch.viewer_allowlist = Some(
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect(),
            );
        }
        // `owner` is bootstrap-only (config.toml) — not migrated to settings.ron.
        if let Some(s) = v.get("admin_channel").and_then(toml::Value::as_str) {
            out.twitch.admin_channel = Some(if s.trim().is_empty() {
                None
            } else {
                Some(s.to_owned())
            });
        }
        if let Some(s) = v.get("ai_channel").and_then(toml::Value::as_str) {
            out.twitch.ai_channel = Some(if s.trim().is_empty() {
                None
            } else {
                Some(s.to_owned())
            });
        }
    }

    if let Some(t) = root.get("suspend").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(t.clone());
        out.suspend.default_duration_secs = v
            .get("default_duration_secs")
            .and_then(toml::Value::as_integer)
            .and_then(|i| u64::try_from(i).ok());
    }

    if let Some(t) = root.get("aviationstack").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(t.clone());
        out.aviationstack.enabled = v.get("enabled").and_then(toml::Value::as_bool);
        out.aviationstack.base_url = v
            .get("base_url")
            .and_then(toml::Value::as_str)
            .map(str::to_owned);
        out.aviationstack.timeout_secs = v
            .get("timeout_secs")
            .and_then(toml::Value::as_integer)
            .and_then(|i| u64::try_from(i).ok());
    }

    if let Some(t) = root.get("web").and_then(|v| v.as_table()) {
        let v = toml::Value::Table(t.clone());
        out.web.session_ttl_secs = v
            .get("session_ttl")
            .and_then(toml::Value::as_str)
            .and_then(|s| parse_duration(s).ok())
            .map(|d| d.as_secs());
        out.web.mod_check_refresh_secs = v
            .get("mod_check_refresh")
            .and_then(toml::Value::as_str)
            .and_then(|s| parse_duration(s).ok())
            .map(|d| d.as_secs());
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ai_keys_migrate_into_settings_ron() {
        let raw = r#"
            [twitch]
            channel = "c"
            username = "u"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"

            [ai]
            api_key = "sk"
            backend = "ollama"
            model = "gemma3:4b"
            timeout = 45
            max_turn_rounds = 5

            [ai.memory]
            soul_bytes = 8192
            lore_bytes = 16384
            inject_byte_budget = 32768

            [ai.web]
            enabled = true
            base_url = "https://searxng.test/search"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_ai(&value).expect("migrate");
        assert_eq!(overrides.connection.model.as_deref(), Some("gemma3:4b"));
        assert_eq!(overrides.connection.timeout, Some(45));
        assert_eq!(overrides.memory.soul_bytes, Some(8192));
        assert_eq!(overrides.web.enabled, Some(true));
        assert_eq!(
            overrides.web.base_url.as_deref(),
            Some("https://searxng.test/search")
        );
    }

    #[test]
    fn no_ai_section_returns_default() {
        let raw = r#"
            [twitch]
            channel = "c"
            username = "u"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_ai(&value).expect("migrate");
        assert_eq!(overrides, AiOverrides::default());
    }

    #[test]
    fn backend_openai_migrates() {
        let raw = r#"
            [twitch]
            channel = "c"
            username = "u"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"

            [ai]
            api_key = "sk"
            backend = "openai"
            model = "gpt-4o"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_ai(&value).expect("migrate");
        assert_eq!(
            overrides.connection.backend,
            Some(super::super::ai::AiBackendKind::OpenAi)
        );
        assert_eq!(overrides.connection.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn legacy_twitch_keys_migrate_into_overrides() {
        let raw = r#"
            [twitch]
            channel = "main"
            username = "bot"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"
            expected_latency = 250
            hidden_admins = ["111", "222"]
            viewer_allowlist = ["999"]
            owner = "777"
            admin_channel = "admins"
            ai_channel = "ai"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        assert_eq!(overrides.twitch.expected_latency, Some(250));
        assert_eq!(
            overrides.twitch.hidden_admins.as_deref(),
            Some(&vec!["111".to_string(), "222".to_string()][..])
        );
        assert_eq!(
            overrides.twitch.viewer_allowlist.as_deref(),
            Some(&vec!["999".to_string()][..])
        );
        // `owner` is bootstrap-only (config.toml) and is no longer migrated.
        assert_eq!(overrides.twitch.admin_channel, Some(Some("admins".into())));
        assert_eq!(overrides.twitch.ai_channel, Some(Some("ai".into())));
    }

    #[test]
    fn legacy_twitch_minimal_returns_default() {
        let raw = r#"
            [twitch]
            channel = "main"
            username = "bot"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        assert_eq!(overrides.twitch, TwitchOverrides::default());
    }

    #[test]
    fn legacy_suspend_aviationstack_web_keys_migrate() {
        let raw = r#"
            [twitch]
            channel = "c"
            username = "u"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"

            [suspend]
            default_duration_secs = 900

            [aviationstack]
            enabled = true
            api_key = "k"
            base_url = "https://aviationstack.example/v1"
            timeout_secs = 7

            [web]
            enabled = true
            bind_addr = "127.0.0.1:8080"
            public_url = "https://bot.example"
            session_secret = "00112233"
            session_ttl = "12h"
            mod_check_refresh = "2m"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        assert_eq!(overrides.suspend.default_duration_secs, Some(900));
        assert_eq!(overrides.aviationstack.enabled, Some(true));
        assert_eq!(
            overrides.aviationstack.base_url.as_deref(),
            Some("https://aviationstack.example/v1")
        );
        assert_eq!(overrides.aviationstack.timeout_secs, Some(7));
        assert_eq!(overrides.web.session_ttl_secs, Some(12 * 3600));
        assert_eq!(overrides.web.mod_check_refresh_secs, Some(120));
    }

    #[test]
    fn legacy_blank_optional_strings_migrate_as_explicit_clear() {
        let raw = r#"
            [twitch]
            channel = "main"
            username = "bot"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"
            owner = ""
            admin_channel = "   "
            ai_channel = ""
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        // `owner` is bootstrap-only (config.toml) and is no longer migrated.
        assert_eq!(overrides.twitch.admin_channel, Some(None));
        assert_eq!(overrides.twitch.ai_channel, Some(None));
    }

    #[test]
    fn legacy_web_invalid_humantime_is_skipped() {
        let raw = r#"
            [twitch]
            channel = "c"
            username = "u"
            refresh_token = "r"
            client_id = "i"
            client_secret = "s"

            [web]
            session_ttl = "not-a-duration"
        "#;
        let value: toml::Value = toml::from_str(raw).expect("parse");
        let overrides = migrate_legacy_config(&value).expect("migrate");
        assert_eq!(overrides.web.session_ttl_secs, None);
    }
}
