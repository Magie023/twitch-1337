# Final config.toml → settings.ron migration

Status: design
Date: 2026-05-24

## Goal

Complete the migration begun in `2026-05-15-ai-settings-hoist-design.md`. Move
every remaining non-secret, non-bootstrap config knob from `config.toml` into
the dashboard-managed `settings.ron` store. After this work, `config.toml`
holds only credentials, identity, and dashboard wiring.

## Scope split

### Stays in `config.toml`

Bootstrap-only — read before `settings.ron` is loaded, or holds a secret:

| Key | Reason |
|---|---|
| `twitch.channel` | IRC join target before dashboard reachable |
| `twitch.username` | IRC identity |
| `twitch.refresh_token`, `client_id`, `client_secret` | Twitch OAuth secrets |
| `ai.api_key` | LLM provider secret |
| `aviationstack.api_key` | Aviationstack secret |
| `web.enabled`, `bind_addr`, `public_url`, `session_secret` | Dashboard bootstrap |

### Moves to `settings.ron` (schema v3)

| Key | Restart? |
|---|---|
| `twitch.expected_latency` | live (already a runtime EMA seed) |
| `twitch.hidden_admins` | live |
| `twitch.viewer_allowlist` | live |
| `twitch.owner` | live |
| `twitch.admin_channel` | **restart** (IRC join at startup) |
| `twitch.ai_channel` | **restart** (IRC join + routing) |
| `suspend.default_duration_secs` | live |
| `aviationstack.enabled` | **restart** (client built once) |
| `aviationstack.base_url` | **restart** |
| `aviationstack.timeout_secs` | **restart** |
| `web.session_ttl_secs` | live (new sessions) |
| `web.mod_check_refresh_secs` | live (next refresh tick) |

`[[schedules]]` migration is **deferred to PR 2** — different shape (collection
CRUD vs scalars). The `/schedules` stub page already exists and will host the
PR 2 UI.

## Plan: two PRs

1. **PR 1 (this spec):** all scalar sections above. Schema v2 → v3,
   `.config_migrated_v3` sentinel, new dashboard cards.
2. **PR 2 (separate spec, later):** `[[schedules]]` CRUD on the existing
   `/schedules` stub page, drop the `notify-debouncer-mini` watcher.

## Types (PR 1)

New modules in `crates/core/src/settings/`:

```rust
// settings/twitch.rs
pub struct TwitchRuntime {
    pub expected_latency: u32,        // 0..=1000ms
    pub hidden_admins: Vec<String>,   // Twitch user IDs
    pub viewer_allowlist: Vec<String>,
    pub owner: Option<String>,
    pub admin_channel: Option<String>,  // restart required
    pub ai_channel: Option<String>,     // restart required
}

// settings/aviationstack.rs
pub struct AviationstackSettings {
    pub enabled: bool,
    pub base_url: String,
    pub timeout_secs: u64,
}

// settings/suspend.rs
pub struct SuspendSettings { pub default_duration_secs: u64 }

// settings/web.rs
pub struct WebRuntime {
    pub session_ttl_secs: u64,
    pub mod_check_refresh_secs: u64,
}
```

Added to `Settings`:

```rust
pub struct Settings {
    pub schema_version: u32,  // 3
    pub cooldowns: Cooldowns,
    pub pings: PingsSettings,
    pub ai: AiSettings,
    pub twitch: TwitchRuntime,
    pub aviationstack: AviationstackSettings,
    pub suspend: SuspendSettings,
    pub web: WebRuntime,
}
```

Each gets a matching `*Overrides` struct in `overrides.rs`. Optional scalar
overrides use `Option<Option<T>>` (tri-state: default / explicit clear / set),
matching the `service_tier` precedent. List overrides use `Option<Vec<String>>`
— `None` = use defaults (empty), `Some(v)` = explicit set; no per-entry
override.

`SettingsSection` variants added: `TwitchPermissions`, `TwitchChannels`,
`Aviationstack`, `Suspend`, `WebRuntime`.

## Migration helper

New `migrate_legacy_config(root: &toml::Value) -> SettingsOverrides` in
`settings/migrate.rs`. Extends the existing `migrate_legacy_ai` style — sparse
patch, no-op for absent sections.

Trigger in `main.rs`:

- Sentinel: `$DATA_DIR/.config_migrated_v3`
- On first v3 launch: read raw `toml::Value` (already returned from
  `load_configuration`), build patch, call `SettingsStore::apply` once per
  non-empty section, then touch sentinel.
- Idempotent: re-running is safe; once the sentinel exists the legacy keys in
  `config.toml` are ignored.

Reads include `web.session_ttl` / `web.mod_check_refresh` (parse humantime →
secs). Skips `web.enabled`, `bind_addr`, `public_url`, `session_secret` and
both `*.api_key` fields.

Audit log: one entry per migrated section, `Actor::System`, reason
`"v3 migration"`.

Stale config.toml keys: log a single `warn!` at startup listing migrated keys
still present in the file. Never delete the user's file.

## Runtime wire-up

| Field | Read site | Hot/restart |
|---|---|---|
| `expected_latency` | `latency.rs` init seed | live |
| `hidden_admins` | permission checks in command handlers | live |
| `viewer_allowlist` | `web/auth` role resolution | live |
| `owner` | `web/routes/settings.rs` role gate | live |
| `admin_channel` | IRC join in `twitch/setup.rs` | restart |
| `ai_channel` | IRC join + message routing | restart |
| `suspend.default_duration_secs` | `commands/suspend.rs` | live |
| `aviationstack.*` | `aviation/client.rs` init | restart |
| `web.session_ttl_secs` | web session creation | live (next session) |
| `web.mod_check_refresh_secs` | web mod cache | live (next refresh tick) |

Pattern: handlers already accept `SettingsHandle`; each call site reads
`handle.load().<section>.<field>` per use to avoid stale captured values for
hot fields. Restart-required fields are read once at startup and surface a
"restart required" badge in the dashboard.

The aviationstack HTTP client today fails-and-disables on bad init. That stays
unchanged. A runtime `enabled=false` toggle requires a restart in PR 1
(rebuilding the client mid-run is out of scope).

## Validation

Port rules from `validate_config` into `Settings::validate`:

- `twitch.expected_latency` ≤ 1000
- `twitch.admin_channel` non-empty when `Some`, ≠ bootstrap channel
- `twitch.ai_channel` non-empty when `Some`, ≠ bootstrap channel, ≠
  `admin_channel`
- `twitch.hidden_admins` / `viewer_allowlist`: each entry trimmed non-empty
- `twitch.owner` trimmed non-empty when `Some`
- `suspend.default_duration_secs` ∈ 1..=604_800
- `aviationstack.base_url` parses as `reqwest::Url`, non-empty
- `aviationstack.timeout_secs` > 0
- `web.session_ttl_secs` ∈ 3600..=2_592_000
- `web.mod_check_refresh_secs` ∈ 30..=3600

Cross-field rules need the bootstrap channel, which is not part of `Settings`.
Introduce `ValidationContext { channel: String }` and change the signature to
`Settings::validate(&self, ctx: &ValidationContext)`. Update all callers; tests
build a ctx with `"test"`.

`load_configuration` continues returning `(Configuration, toml::Value)` as it
does today; `main.rs` reads the raw `toml::Value` once during the migration
step and then drops it. Dropping the raw return value once every legacy
section is stable is a future cleanup, out of scope for PR 1.

## Dashboard UI

`/settings` page (template `crates/web/templates/settings/index.html`) gains
five new cards, each following the established per-card `SaveForm` + tri-state
helpers pattern with a `reset/<section>` route:

1. **Twitch · Permissions** — `hidden_admins`, `viewer_allowlist` (textarea,
   one ID per line, validated as digits), `owner` (single optional ID).
2. **Twitch · Channels** — `expected_latency`, `admin_channel`, `ai_channel`.
   Restart badge on the two channel fields.
3. **Aviationstack** — `enabled` checkbox, `base_url`, `timeout_secs`.
   Restart badge on all fields.
4. **Suspend** — `default_duration_secs`.
5. **Web · Sessions** — `session_ttl_secs`, `mod_check_refresh_secs`.

Each card has its own POST handler in `routes/settings.rs` mirroring
`save_cooldowns` / `save_pings`. Reset clears one section back to compiled
defaults.

## Testing

**Unit (`settings/migrate.rs`):**

- Legacy `[twitch]` / `[suspend]` / `[aviationstack]` / `[web]` keys produce
  expected overrides.
- Absent sections → `SettingsOverrides::default()` no-op.
- `web.session_ttl` humantime parses to secs; malformed values logged and
  skipped.
- Partial sections migrate only the present keys.

**Unit (`settings/mod.rs` resolve_tests):**

- Per-field override wins; siblings fall through to defaults.
- List overrides: `None` = defaults, `Some(vec![])` = explicit empty.
- Channel `Option<Option<String>>`: `None` = default, `Some(None)` = explicit
  clear, `Some(Some(v))` = set.

**Unit (validate):**

- `expected_latency` > 1000 rejected.
- `admin_channel` / `ai_channel` cross-validation against
  `ValidationContext.channel`.
- `suspend.default_duration_secs` bound.
- `aviationstack.base_url` URL parse, `timeout_secs` > 0.
- `web.session_ttl_secs` / `mod_check_refresh_secs` ranges.

**Integration (`crates/web/tests/settings_route.rs`):**

- GET `/settings` renders new cards.
- POST save updates `settings.ron` and swaps the handle.
- `reset/twitch_permissions` etc. clears one section.
- Invalid input renders errors without persisting.

**End-to-end migration test:**

- Legacy `config.toml` → sentinel created, `settings.ron` contains migrated
  values, second run is idempotent.

## PR 2 preview — schedules (out of scope for PR 1)

- `Vec<ScheduleSettings>` added to `Settings` in PR 2.
- CRUD UI built on the existing `/schedules` stub page: list + add / edit /
  delete rows. Each row: name (unique), message, interval, optional start/end
  date, optional active-time window, enabled.
- Drop `notify-debouncer-mini` watcher entirely; remove `[[schedules]]` from
  `config.toml.example`.
- Both PRs share the `.config_migrated_v3` sentinel; the migrate helper reads
  only present keys, so landing PR 2 before all users have run PR 1 is safe.
- Hot apply: extend the existing `tokio::sync::Notify` shutdown channel to
  also signal settings changes, so the schedule handler reloads its list
  without restart.
- Validation: port today's `validate_config` schedule loop into
  `Settings::validate`.
