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

/// In-place migration of raw RON `schedules` from v3 (`ScheduleSettings`
/// with `interval: String`, `active_time_*: Option<String>`,
/// `start_date: Option<String>` "YYYY-MM-DDTHH:MM:SS") to v4
/// (`Schedule` with `trigger: Trigger::Interval`, `start_date:
/// Option<NaiveDate>`).
///
/// Operates on a `ron::Value` representing the deserialized
/// `SettingsOverrides` map. Returns `true` if any change was made.
pub fn migrate_schedules_v3_to_v4(root: &mut ron::Value) -> bool {
    let Some(sched_field) = map_get_mut(root, "schedules") else {
        return false;
    };

    // schedules is `Option<Vec<Schedule>>` in serialized form:
    //   None  => Value::Option(None)
    //   Some(v) => Value::Option(Some(Box<Value::Seq(...)>))
    let rows = match sched_field {
        ron::Value::Option(Some(boxed)) => match boxed.as_mut() {
            ron::Value::Seq(rows) => rows,
            _ => return false,
        },
        _ => return false,
    };

    let mut changed = false;
    for row in rows.iter_mut() {
        if migrate_one_row(row) {
            changed = true;
        }
    }
    changed
}

fn migrate_one_row(row: &mut ron::Value) -> bool {
    // Already migrated? Skip if `trigger` is present.
    if map_get(row, "trigger").is_some() {
        return false;
    }

    let interval_secs = map_get(row, "interval")
        .and_then(value_as_str)
        .and_then(parse_interval_legacy)
        .unwrap_or(60);

    let active_from = take_optional_string(row, "active_time_start");
    let active_to = take_optional_string(row, "active_time_end");

    // Build the trigger Value::Map.
    let mut t_map = ron::Map::new();
    t_map.insert(
        ron::Value::String("kind".into()),
        ron::Value::String("interval".into()),
    );
    t_map.insert(
        ron::Value::String("every".into()),
        ron::Value::String(format!("{interval_secs}s")),
    );
    t_map.insert(
        ron::Value::String("days".into()),
        ron::Value::Seq(Vec::new()),
    );
    t_map.insert(
        ron::Value::String("active_from".into()),
        optional_string_to_value(active_from),
    );
    t_map.insert(
        ron::Value::String("active_to".into()),
        optional_string_to_value(active_to),
    );

    // Convert start_date/end_date "YYYY-MM-DDTHH:MM:SS" -> "YYYY-MM-DD".
    convert_date_field(row, "start_date");
    convert_date_field(row, "end_date");

    map_insert(row, "trigger", ron::Value::Map(t_map));
    map_remove(row, "interval");
    map_remove(row, "active_time_start");
    map_remove(row, "active_time_end");
    true
}

fn convert_date_field(row: &mut ron::Value, key: &str) {
    let Some(v) = map_get(row, key) else {
        return;
    };
    let new = match v {
        ron::Value::Option(Some(boxed)) => match boxed.as_ref() {
            ron::Value::String(s) => {
                let date_only = s.split('T').next().unwrap_or(s).to_owned();
                ron::Value::Option(Some(Box::new(ron::Value::String(date_only))))
            }
            _ => ron::Value::Option(None),
        },
        _ => return,
    };
    map_insert(row, key, new);
}

fn parse_interval_legacy(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some((h, m)) = s.split_once(':') {
        let h: i64 = h.parse().ok()?;
        let m: i64 = m.parse().ok()?;
        let total = h * 3600 + m * 60;
        if total <= 0 {
            return None;
        }
        Some(total)
    } else {
        let s = s.to_lowercase();
        let mut total = 0i64;
        let mut cur = String::new();
        for ch in s.chars() {
            if ch.is_ascii_digit() {
                cur.push(ch);
            } else {
                let n: i64 = cur.parse().ok()?;
                cur.clear();
                total += match ch {
                    'h' => n * 3600,
                    'm' => n * 60,
                    's' => n,
                    _ => return None,
                };
            }
        }
        if total == 0 { None } else { Some(total) }
    }
}

// --- ron::Value::Map helpers -------------------------------------------

fn map_get<'a>(root: &'a ron::Value, key: &str) -> Option<&'a ron::Value> {
    match root {
        ron::Value::Map(m) => m.get(&ron::Value::String(key.into())),
        _ => None,
    }
}

fn map_get_mut<'a>(root: &'a mut ron::Value, key: &str) -> Option<&'a mut ron::Value> {
    match root {
        ron::Value::Map(m) => m.get_mut(&ron::Value::String(key.into())),
        _ => None,
    }
}

fn map_insert(root: &mut ron::Value, key: &str, value: ron::Value) {
    if let ron::Value::Map(m) = root {
        m.insert(ron::Value::String(key.into()), value);
    }
}

fn map_remove(root: &mut ron::Value, key: &str) {
    if let ron::Value::Map(m) = root {
        m.remove(&ron::Value::String(key.into()));
    }
}

fn value_as_str(v: &ron::Value) -> Option<&str> {
    match v {
        ron::Value::String(s) => Some(s),
        _ => None,
    }
}

fn take_optional_string(row: &ron::Value, key: &str) -> Option<String> {
    match map_get(row, key)? {
        ron::Value::Option(Some(boxed)) => match boxed.as_ref() {
            ron::Value::String(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn optional_string_to_value(v: Option<String>) -> ron::Value {
    match v {
        Some(s) => ron::Value::Option(Some(Box::new(ron::Value::String(s)))),
        None => ron::Value::Option(None),
    }
}

#[cfg(test)]
mod v4_migration_tests {
    use super::*;

    #[test]
    fn legacy_row_gets_trigger_interval() {
        // Build a minimal SettingsOverrides RON shape with a v3 schedule.
        let raw = r#"(
            schedules: Some([
                (
                    name: "noon",
                    message: "hi",
                    interval: "01:00",
                    start_date: None,
                    end_date: None,
                    active_time_start: None,
                    active_time_end: None,
                    enabled: true,
                ),
            ]),
        )"#;
        let mut val: ron::Value = ron::from_str(raw).expect("parse");
        assert!(migrate_schedules_v3_to_v4(&mut val));
        let ser = ron::ser::to_string(&val).expect("ser");
        assert!(ser.contains("trigger"));
        assert!(ser.contains("interval"));
        assert!(!ser.contains("active_time_start"));
    }

    #[test]
    fn already_migrated_row_is_unchanged() {
        let raw = r#"(
            schedules: Some([
                (
                    name: "x",
                    message: "y",
                    trigger: (kind: "calendar", days: [], at: "09:00:00"),
                    start_date: None,
                    end_date: None,
                    enabled: true,
                ),
            ]),
        )"#;
        let mut val: ron::Value = ron::from_str(raw).expect("parse");
        let changed = migrate_schedules_v3_to_v4(&mut val);
        assert!(!changed);
    }

    #[test]
    fn no_schedules_field_returns_false() {
        let raw = r#"( cooldowns: () )"#;
        let mut val: ron::Value = ron::from_str(raw).expect("parse");
        assert!(!migrate_schedules_v3_to_v4(&mut val));
    }

    #[test]
    fn parse_interval_legacy_rejects_zero() {
        assert_eq!(parse_interval_legacy("00:00"), None);
        assert_eq!(parse_interval_legacy("0:0"), None);
    }

    #[test]
    fn date_field_truncates_time_component() {
        let raw = r#"(
            schedules: Some([
                (
                    name: "x",
                    message: "y",
                    interval: "01:00",
                    start_date: Some("2026-06-01T00:00:00"),
                    end_date: Some("2026-12-31T23:59:59"),
                    active_time_start: None,
                    active_time_end: None,
                    enabled: true,
                ),
            ]),
        )"#;
        let mut val: ron::Value = ron::from_str(raw).expect("parse");
        assert!(migrate_schedules_v3_to_v4(&mut val));
        let ser = ron::ser::to_string(&val).expect("ser");
        assert!(ser.contains("2026-06-01"));
        assert!(!ser.contains("2026-06-01T"));
    }
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
