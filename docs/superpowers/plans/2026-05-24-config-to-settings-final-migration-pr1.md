# Final config.toml → settings.ron migration — PR 1 (scalars)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bump settings schema v2 → v3 and migrate every non-secret, non-bootstrap scalar from `config.toml` (`twitch.*`, `suspend.*`, `aviationstack.*` non-secret, `web.*` non-bootstrap) into dashboard-managed `settings.ron`, with one-time legacy migration + new dashboard cards.

**Architecture:** Mirror the v2 `[ai]` precedent: per-section settings/override structs in `crates/core/src/settings/`, sparse overrides merged onto compile-time defaults, atomic `settings.ron` persistence via `SettingsStore::apply`, audit-logged changes, dashboard cards on `/settings` with tri-state helpers and per-section reset routes. A new `migrate_legacy_config` helper runs once (sentinel: `$DATA_DIR/.config_migrated_v3`) at startup.

**Tech Stack:** Rust 2024, serde, RON, axum, askama, arc_swap, toml, humantime_serde, cargo nextest.

**Spec:** `docs/superpowers/specs/2026-05-24-config-to-settings-final-migration-design.md`

**Branch:** `spec/config-to-settings-final-migration` (already created; spec commit `98064e0` lives here).

**Commit cadence:** one commit per task. Use Conventional Commits (`feat:`, `refactor:`, `test:`, `docs:`). Commit message body unhinged-genz first line per repo style — keep terse for chore-y tasks.

**Pre-commit gate (per CLAUDE.md):**
```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail
```
Run before every commit. CI mirrors these.

---

## Task 1: Add `TwitchRuntime` settings type

**Files:**
- Create: `crates/core/src/settings/twitch.rs`
- Modify: `crates/core/src/settings/mod.rs:12-21` (`pub mod` + `pub use`)

- [ ] **Step 1: Create the new module**

Create `crates/core/src/settings/twitch.rs`:

```rust
//! Twitch runtime settings — formerly inline in `[twitch]` of config.toml.
//!
//! `expected_latency`, `hidden_admins`, `viewer_allowlist`, `owner` apply live.
//! `admin_channel` and `ai_channel` are read once at startup (IRC join +
//! message routing); changes require a bot restart and the dashboard surfaces
//! a "restart required" badge on those fields.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TwitchRuntime {
    pub expected_latency: u32,
    pub hidden_admins: Vec<String>,
    pub viewer_allowlist: Vec<String>,
    pub owner: Option<String>,
    pub admin_channel: Option<String>,
    pub ai_channel: Option<String>,
}

impl Default for TwitchRuntime {
    fn default() -> Self {
        Self {
            expected_latency: 100,
            hidden_admins: Vec::new(),
            viewer_allowlist: Vec::new(),
            owner: None,
            admin_channel: None,
            ai_channel: None,
        }
    }
}
```

- [ ] **Step 2: Register module + re-export**

Edit `crates/core/src/settings/mod.rs`. Inside the `pub mod` block (~line 12), add:
```rust
pub mod twitch;
```
Inside the `pub use ai::{...}` re-export block (~line 18), add a new line below it:
```rust
pub use twitch::TwitchRuntime;
```

- [ ] **Step 3: Compile-check**

Run: `cargo check -p twitch-1337-core`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/settings/twitch.rs crates/core/src/settings/mod.rs
git commit -m "feat(settings): add TwitchRuntime scaffold"
```

---

## Task 2: Add `AviationstackSettings` type

**Files:**
- Create: `crates/core/src/settings/aviationstack.rs`
- Modify: `crates/core/src/settings/mod.rs`

- [ ] **Step 1: Create module**

Create `crates/core/src/settings/aviationstack.rs`:

```rust
//! Aviationstack metadata enrichment settings. `api_key` stays in
//! config.toml as a secret; the dashboard manages the rest.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AviationstackSettings {
    pub enabled: bool,
    pub base_url: String,
    pub timeout_secs: u64,
}

impl Default for AviationstackSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.aviationstack.com/v1".to_owned(),
            timeout_secs: 5,
        }
    }
}
```

- [ ] **Step 2: Register**

Add to `crates/core/src/settings/mod.rs`:
```rust
pub mod aviationstack;
```
and
```rust
pub use aviationstack::AviationstackSettings;
```

- [ ] **Step 3: Compile-check**

Run: `cargo check -p twitch-1337-core`

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/settings/aviationstack.rs crates/core/src/settings/mod.rs
git commit -m "feat(settings): add AviationstackSettings scaffold"
```

---

## Task 3: Add `SuspendSettings` type

**Files:**
- Create: `crates/core/src/settings/suspend.rs`
- Modify: `crates/core/src/settings/mod.rs`

- [ ] **Step 1: Create module**

Create `crates/core/src/settings/suspend.rs`:

```rust
//! Suspend command runtime settings.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuspendSettings {
    pub default_duration_secs: u64,
}

impl Default for SuspendSettings {
    fn default() -> Self {
        Self { default_duration_secs: 600 }
    }
}
```

- [ ] **Step 2: Register**

Add to `crates/core/src/settings/mod.rs`:
```rust
pub mod suspend;
```
and
```rust
pub use suspend::SuspendSettings;
```

- [ ] **Step 3: Compile-check**

Run: `cargo check -p twitch-1337-core`

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/settings/suspend.rs crates/core/src/settings/mod.rs
git commit -m "feat(settings): add SuspendSettings scaffold"
```

---

## Task 4: Add `WebRuntime` type

**Files:**
- Create: `crates/core/src/settings/web.rs`
- Modify: `crates/core/src/settings/mod.rs`

- [ ] **Step 1: Create module**

Create `crates/core/src/settings/web.rs`:

```rust
//! Web dashboard runtime settings. Bootstrap fields (`enabled`, `bind_addr`,
//! `public_url`, `session_secret`) stay in config.toml — the dashboard cannot
//! manage what bootstraps it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebRuntime {
    pub session_ttl_secs: u64,
    pub mod_check_refresh_secs: u64,
}

impl Default for WebRuntime {
    fn default() -> Self {
        Self {
            session_ttl_secs: 7 * 24 * 60 * 60,
            mod_check_refresh_secs: 300,
        }
    }
}
```

- [ ] **Step 2: Register**

Add to `crates/core/src/settings/mod.rs`:
```rust
pub mod web;
```
and
```rust
pub use web::WebRuntime;
```

- [ ] **Step 3: Compile-check**

Run: `cargo check -p twitch-1337-core`

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/settings/web.rs crates/core/src/settings/mod.rs
git commit -m "feat(settings): add WebRuntime scaffold"
```

---

## Task 5: Extend `SettingsOverrides` with new sections

**Files:**
- Modify: `crates/core/src/settings/overrides.rs`

- [ ] **Step 1: Add override structs**

At the end of `crates/core/src/settings/overrides.rs`, append:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TwitchOverrides {
    #[serde(default)]
    pub expected_latency: Option<u32>,
    #[serde(default)]
    pub hidden_admins: Option<Vec<String>>,
    #[serde(default)]
    pub viewer_allowlist: Option<Vec<String>>,
    /// Tri-state: `None` = leave at default, `Some(None)` = explicit clear,
    /// `Some(Some(x))` = set.
    #[serde(default)]
    pub owner: Option<Option<String>>,
    #[serde(default)]
    pub admin_channel: Option<Option<String>>,
    #[serde(default)]
    pub ai_channel: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AviationstackOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuspendOverrides {
    #[serde(default)]
    pub default_duration_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebRuntimeOverrides {
    #[serde(default)]
    pub session_ttl_secs: Option<u64>,
    #[serde(default)]
    pub mod_check_refresh_secs: Option<u64>,
}
```

- [ ] **Step 2: Add fields to `SettingsOverrides`**

Modify the `SettingsOverrides` struct (top of the file) to add four fields:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsOverrides {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub cooldowns: CooldownsOverrides,
    #[serde(default)]
    pub pings: PingsOverrides,
    #[serde(default)]
    pub ai: AiOverrides,
    #[serde(default)]
    pub twitch: TwitchOverrides,
    #[serde(default)]
    pub aviationstack: AviationstackOverrides,
    #[serde(default)]
    pub suspend: SuspendOverrides,
    #[serde(default)]
    pub web: WebRuntimeOverrides,
}
```

And update the manual `Default` impl just below it:

```rust
impl Default for SettingsOverrides {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            cooldowns: CooldownsOverrides::default(),
            pings: PingsOverrides::default(),
            ai: AiOverrides::default(),
            twitch: TwitchOverrides::default(),
            aviationstack: AviationstackOverrides::default(),
            suspend: SuspendOverrides::default(),
            web: WebRuntimeOverrides::default(),
        }
    }
}
```

- [ ] **Step 3: Compile-check**

Run: `cargo check -p twitch-1337-core`

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/settings/overrides.rs
git commit -m "feat(settings): add overrides for new sections"
```

---

## Task 6: Bump `SCHEMA_VERSION` and extend `Settings`

**Files:**
- Modify: `crates/core/src/settings/mod.rs`

- [ ] **Step 1: Write failing test for v3 defaults**

Edit `crates/core/src/settings/mod.rs`. Inside the `resolve_tests` mod, replace the existing `compiled_defaults_include_ai_block_v2` test with:

```rust
#[test]
fn compiled_defaults_v3_layout() {
    let s = Settings::compiled_defaults();
    assert_eq!(s.schema_version, 3);
    assert_eq!(s.ai, AiSettings::default());
    assert_eq!(s.twitch, TwitchRuntime::default());
    assert_eq!(s.aviationstack, AviationstackSettings::default());
    assert_eq!(s.suspend, SuspendSettings::default());
    assert_eq!(s.web, WebRuntime::default());
}
```

- [ ] **Step 2: Run test and confirm it fails**

Run: `cargo nextest run -p twitch-1337-core settings::resolve_tests::compiled_defaults_v3_layout`
Expected: FAIL — `Settings` has no `twitch` / `aviationstack` / `suspend` / `web` field, and `schema_version` is 2.

- [ ] **Step 3: Bump SCHEMA_VERSION and extend struct**

In `crates/core/src/settings/mod.rs`:
- Change `pub const SCHEMA_VERSION: u32 = 2;` → `pub const SCHEMA_VERSION: u32 = 3;`
- Extend the `Settings` struct:

```rust
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
}
```

- Extend `Settings::compiled_defaults` to populate the new fields with `*::default()`:

```rust
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
            pings: PingsSettings { cooldown: 300, public: false },
            ai: AiSettings::default(),
            twitch: TwitchRuntime::default(),
            aviationstack: AviationstackSettings::default(),
            suspend: SuspendSettings::default(),
            web: WebRuntime::default(),
        }
    }
    // ... validate + resolve unchanged for now
}
```

- [ ] **Step 4: Run test, verify pass**

Run: `cargo nextest run -p twitch-1337-core settings::resolve_tests::compiled_defaults_v3_layout`
Expected: PASS.

- [ ] **Step 5: Build the whole workspace**

Run: `cargo check --workspace`
Expected: failures everywhere `Settings` is constructed positionally — chase them down. Most call sites use `Settings::compiled_defaults()` or struct update syntax, which keeps working; a few tests may construct directly.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/settings/mod.rs
git commit -m "feat(settings): bump schema to v3 with twitch/aviationstack/suspend/web sections"
```

---

## Task 7: Extend `Settings::resolve` for new sections

**Files:**
- Modify: `crates/core/src/settings/mod.rs`

- [ ] **Step 1: Write failing test**

Inside the `resolve_tests` mod, add:

```rust
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
        suspend: SuspendOverrides { default_duration_secs: Some(900) },
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
```

- [ ] **Step 2: Run tests, confirm failure**

Run: `cargo nextest run -p twitch-1337-core settings::resolve_tests::twitch_override_resolves settings::resolve_tests::aviationstack_suspend_web_resolve`
Expected: FAIL — `Settings::resolve` does not yet populate the new fields.

- [ ] **Step 3: Extend `Settings::resolve`**

In `crates/core/src/settings/mod.rs`, edit `Settings::resolve` to populate the four new fields by adding these lines just before the closing brace of the returned `Settings { ... }`:

```rust
twitch: TwitchRuntime {
    expected_latency: overrides.twitch.expected_latency.unwrap_or(defaults.twitch.expected_latency),
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
    owner: match &overrides.twitch.owner {
        Some(v) => v.clone(),
        None => defaults.twitch.owner.clone(),
    },
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
    enabled: overrides.aviationstack.enabled.unwrap_or(defaults.aviationstack.enabled),
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
    session_ttl_secs: overrides.web.session_ttl_secs.unwrap_or(defaults.web.session_ttl_secs),
    mod_check_refresh_secs: overrides
        .web
        .mod_check_refresh_secs
        .unwrap_or(defaults.web.mod_check_refresh_secs),
},
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo nextest run -p twitch-1337-core settings::resolve_tests`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/settings/mod.rs
git commit -m "feat(settings): resolve twitch/aviationstack/suspend/web sections"
```

---

## Task 8: Introduce `ValidationContext` and extend `Settings::validate`

**Files:**
- Modify: `crates/core/src/settings/mod.rs`
- Modify: `crates/core/src/settings/store.rs:46-49`, `:92-94`

`Settings::validate(&self)` becomes `validate(&self, ctx: &ValidationContext)` so cross-field rules can compare against the bootstrap channel.

- [ ] **Step 1: Write failing tests**

Add to the `resolve_tests` mod:

```rust
#[test]
fn validate_rejects_expected_latency_over_1000() {
    let mut s = Settings::compiled_defaults();
    s.twitch.expected_latency = 1500;
    let errs = s
        .validate(&ValidationContext { channel: "test".into() })
        .expect_err("must fail");
    assert!(errs.iter().any(|e| e.field == "twitch.expected_latency"));
}

#[test]
fn validate_rejects_admin_channel_equal_to_bootstrap_channel() {
    let mut s = Settings::compiled_defaults();
    s.twitch.admin_channel = Some("test".into());
    let errs = s
        .validate(&ValidationContext { channel: "test".into() })
        .expect_err("must fail");
    assert!(errs.iter().any(|e| e.field == "twitch.admin_channel"));
}

#[test]
fn validate_rejects_ai_channel_equal_to_admin_channel() {
    let mut s = Settings::compiled_defaults();
    s.twitch.admin_channel = Some("admins".into());
    s.twitch.ai_channel = Some("admins".into());
    let errs = s
        .validate(&ValidationContext { channel: "main".into() })
        .expect_err("must fail");
    assert!(errs.iter().any(|e| e.field == "twitch.ai_channel"));
}

#[test]
fn validate_rejects_suspend_out_of_range() {
    let mut s = Settings::compiled_defaults();
    s.suspend.default_duration_secs = 0;
    let errs = s
        .validate(&ValidationContext { channel: "test".into() })
        .expect_err("must fail");
    assert!(errs.iter().any(|e| e.field == "suspend.default_duration_secs"));
}

#[test]
fn validate_rejects_invalid_aviationstack_url() {
    let mut s = Settings::compiled_defaults();
    s.aviationstack.base_url = "not a url".into();
    let errs = s
        .validate(&ValidationContext { channel: "test".into() })
        .expect_err("must fail");
    assert!(errs.iter().any(|e| e.field == "aviationstack.base_url"));
}

#[test]
fn validate_rejects_web_session_ttl_out_of_range() {
    let mut s = Settings::compiled_defaults();
    s.web.session_ttl_secs = 60; // below 1h
    let errs = s
        .validate(&ValidationContext { channel: "test".into() })
        .expect_err("must fail");
    assert!(errs.iter().any(|e| e.field == "web.session_ttl_secs"));
}
```

Also update every existing `s.validate()` call in the existing tests in this mod to pass `&ValidationContext { channel: "test".into() }`.

- [ ] **Step 2: Run tests to confirm failure**

Run: `cargo nextest run -p twitch-1337-core settings::resolve_tests`
Expected: FAIL — compile error (`validate` takes no arg) or missing validation rules.

- [ ] **Step 3: Add `ValidationContext` and change `validate` signature**

In `crates/core/src/settings/mod.rs`, just below the `FieldError` struct, add:

```rust
/// Bootstrap-side context required for cross-field validation. `channel`
/// is the IRC channel from config.toml, used to enforce that
/// `twitch.admin_channel` and `twitch.ai_channel` differ from it.
#[derive(Debug, Clone)]
pub struct ValidationContext {
    pub channel: String,
}
```

Change `pub fn validate(&self) -> Result<(), Vec<FieldError>>` to:

```rust
pub fn validate(&self, ctx: &ValidationContext) -> Result<(), Vec<FieldError>> {
    // ... existing body, then append the new rules below
}
```

- [ ] **Step 4: Add new validation rules**

Inside `validate`, after the existing `validate_ai(...)` call but before the final `if errs.is_empty() { Ok(()) }`, append:

```rust
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
            errs.push(FieldError { field: field.into(), message: "must not be blank when set".into() });
        } else if t == ctx.channel {
            errs.push(FieldError {
                field: field.into(),
                message: format!("must differ from twitch.channel ({:?})", ctx.channel),
            });
        }
    }
}
if let (Some(ad), Some(ai)) =
    (self.twitch.admin_channel.as_deref(), self.twitch.ai_channel.as_deref())
    && ad.trim() == ai.trim()
{
    errs.push(FieldError {
        field: "twitch.ai_channel".into(),
        message: "must differ from twitch.admin_channel".into(),
    });
}
for (field, val) in [
    ("twitch.owner", self.twitch.owner.as_deref()),
] {
    if let Some(v) = val
        && v.trim().is_empty()
    {
        errs.push(FieldError { field: field.into(), message: "must not be blank when set".into() });
    }
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
        message: format!("must be 1..=604800 (got {})", self.suspend.default_duration_secs),
    });
}
if reqwest::Url::parse(&self.aviationstack.base_url).is_err() {
    errs.push(FieldError {
        field: "aviationstack.base_url".into(),
        message: format!("must be a valid URL (got {:?})", self.aviationstack.base_url),
    });
}
if self.aviationstack.timeout_secs == 0 {
    errs.push(FieldError {
        field: "aviationstack.timeout_secs".into(),
        message: "must be > 0".into(),
    });
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
        message: format!("must be 30..=3600 (got {})", self.web.mod_check_refresh_secs),
    });
}
```

- [ ] **Step 5: Re-export `ValidationContext`**

Near `pub use overrides::{...}` in `crates/core/src/settings/mod.rs`, add `ValidationContext` to the items exported from this module so external crates (the binary, web crate) can construct one.

The `pub` keyword on the struct is enough — no extra `pub use` needed; consumers will write `use twitch_1337_core::settings::ValidationContext;`.

- [ ] **Step 6: Update internal callers**

Edit `crates/core/src/settings/store.rs`:

- `SettingsStore::open` (~line 46): replace `if let Err(errs) = resolved.validate() {` with a placeholder context until we wire the real one through. Add a parameter `boot_channel: &str` to `open` and pass `&ValidationContext { channel: boot_channel.to_owned() }`. Likewise for the `apply` path at line ~92.

In `SettingsStore::open` signature change:

```rust
pub fn open(
    data_dir: &Path,
    audit: Arc<dyn AuditLog>,
    boot_channel: &str,
) -> Result<(Arc<Self>, SettingsHandle), SettingsError> {
    let ctx = super::ValidationContext { channel: boot_channel.to_owned() };
    // ...
    if let Err(errs) = resolved.validate(&ctx) { /* ... */ }
}
```

Add a private `boot_channel: String` field on `SettingsStore` and store it. In `apply`, replace `if let Err(errs) = resolved.validate() {` with:

```rust
let ctx = super::ValidationContext { channel: self.boot_channel.clone() };
if let Err(errs) = resolved.validate(&ctx) {
    return Err(SettingsError::Validation(errs));
}
```

- [ ] **Step 7: Update external call sites**

Run: `cargo check --workspace`

Expected compile errors at every `SettingsStore::open(...)` call. Find them and pass the bootstrap channel:

```bash
rg -n "SettingsStore::open\(" --type rust
```

For each call site (typically `crates/twitch-1337/src/main.rs`, `crates/core/tests/common/test_bot.rs`, `crates/web/tests/helpers/mod.rs`, `crates/web/src/bin/web_dev.rs`):
- Pass `&config.twitch.channel` (binary) or `"test_chan"` (tests).

- [ ] **Step 8: Run all tests**

Run: `cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/settings/mod.rs crates/core/src/settings/store.rs crates/twitch-1337/src/main.rs crates/core/tests/common/test_bot.rs crates/web/tests/helpers/mod.rs crates/web/src/bin/web_dev.rs
git commit -m "feat(settings): add ValidationContext + cross-field rules for v3 sections"
```

---

## Task 9: Extend `merge_into` in store.rs for new sections

**Files:**
- Modify: `crates/core/src/settings/store.rs` (the `merge_into` function near the bottom)

- [ ] **Step 1: Write failing test for merge of twitch.hidden_admins**

Add to the `tests` mod inside `store.rs` (or to `resolve_tests` if no `tests` mod exists in store.rs, create one):

```rust
#[cfg(test)]
mod merge_tests {
    use super::*;
    use crate::settings::overrides::{
        AviationstackOverrides, SuspendOverrides, TwitchOverrides, WebRuntimeOverrides,
    };

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
        assert_eq!(into.twitch.hidden_admins.as_deref(), Some(&vec!["new1".to_string(), "new2".to_string()][..]));
    }

    #[test]
    fn merge_aviationstack_suspend_web() {
        let mut into = SettingsOverrides::default();
        let patch = SettingsOverrides {
            aviationstack: AviationstackOverrides { enabled: Some(true), ..Default::default() },
            suspend: SuspendOverrides { default_duration_secs: Some(900) },
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
}
```

- [ ] **Step 2: Run tests, confirm fail**

Run: `cargo nextest run -p twitch-1337-core settings::store::merge_tests`

- [ ] **Step 3: Extend `merge_into`**

Append to the existing `merge_into` body in `crates/core/src/settings/store.rs`:

```rust
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
if patch.twitch.owner.is_some() {
    into.twitch.owner = patch.twitch.owner.clone();
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
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo nextest run -p twitch-1337-core settings::store::merge_tests`

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/settings/store.rs
git commit -m "feat(settings): merge twitch/aviationstack/suspend/web overrides"
```

---

## Task 10: Extend `SettingsSection` + reset

**Files:**
- Modify: `crates/core/src/settings/mod.rs`
- Modify: `crates/core/src/settings/store.rs` (`reset` match)

- [ ] **Step 1: Extend enum**

In `crates/core/src/settings/mod.rs`, edit the `SettingsSection` enum:

```rust
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
}
```

- [ ] **Step 2: Extend `SettingsStore::reset` match**

In `crates/core/src/settings/store.rs` `reset`'s match, add:

```rust
SettingsSection::TwitchPermissions => {
    current.twitch.hidden_admins = None;
    current.twitch.viewer_allowlist = None;
    current.twitch.owner = None;
}
SettingsSection::TwitchChannels => {
    current.twitch.expected_latency = None;
    current.twitch.admin_channel = None;
    current.twitch.ai_channel = None;
}
SettingsSection::Aviationstack => current.aviationstack = Default::default(),
SettingsSection::Suspend => current.suspend = Default::default(),
SettingsSection::WebRuntime => current.web = Default::default(),
```

- [ ] **Step 3: Write reset test**

Add to `merge_tests` in `store.rs`:

```rust
#[tokio::test]
async fn reset_twitch_permissions_clears_only_perm_fields() {
    let dir = tempfile::tempdir().expect("tmp");
    let audit = Arc::new(crate::settings::audit::MemoryAuditLog::default());
    let (store, _h) = SettingsStore::open(dir.path(), audit, "main").expect("open");
    store
        .apply(
            SettingsOverrides {
                twitch: crate::settings::overrides::TwitchOverrides {
                    expected_latency: Some(250),
                    hidden_admins: Some(vec!["111".into()]),
                    ..Default::default()
                },
                ..Default::default()
            },
            Actor { user_id: "owner".into(), user_login: "owner".into() },
        )
        .await
        .expect("apply");
    let s = store
        .reset(
            SettingsSection::TwitchPermissions,
            Actor { user_id: "owner".into(), user_login: "owner".into() },
        )
        .await
        .expect("reset");
    assert!(s.twitch.hidden_admins.is_empty());
    assert_eq!(s.twitch.expected_latency, 250); // perm reset does not touch channels
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo nextest run -p twitch-1337-core settings::store`
Expected: PASS.

```bash
git add crates/core/src/settings/mod.rs crates/core/src/settings/store.rs
git commit -m "feat(settings): SettingsSection variants for new sections + reset"
```

---

## Task 11: Write `migrate_legacy_config` helper (twitch)

**Files:**
- Modify: `crates/core/src/settings/migrate.rs`

- [ ] **Step 1: Write failing test for twitch migration**

Append to `tests` mod in `crates/core/src/settings/migrate.rs`:

```rust
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
    assert_eq!(overrides.twitch.viewer_allowlist.as_deref(), Some(&vec!["999".to_string()][..]));
    assert_eq!(overrides.twitch.owner, Some(Some("777".into())));
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
    assert_eq!(overrides.twitch, super::overrides::TwitchOverrides::default());
}
```

- [ ] **Step 2: Run + confirm fail**

Run: `cargo nextest run -p twitch-1337-core settings::migrate::tests`
Expected: FAIL — `migrate_legacy_config` does not exist.

- [ ] **Step 3: Implement the helper**

Append to `crates/core/src/settings/migrate.rs`:

```rust
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
                arr.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect(),
            );
        }
        if let Some(arr) = v.get("viewer_allowlist").and_then(toml::Value::as_array) {
            out.twitch.viewer_allowlist = Some(
                arr.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect(),
            );
        }
        if let Some(s) = v.get("owner").and_then(toml::Value::as_str) {
            out.twitch.owner = Some(Some(s.to_owned()));
        }
        if let Some(s) = v.get("admin_channel").and_then(toml::Value::as_str) {
            out.twitch.admin_channel = Some(Some(s.to_owned()));
        }
        if let Some(s) = v.get("ai_channel").and_then(toml::Value::as_str) {
            out.twitch.ai_channel = Some(Some(s.to_owned()));
        }
    }

    Ok(out)
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p twitch-1337-core settings::migrate::tests`

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/settings/migrate.rs
git commit -m "feat(settings): migrate legacy [twitch] keys into overrides"
```

---

## Task 12: Extend migrate helper for suspend / aviationstack / web

**Files:**
- Modify: `crates/core/src/settings/migrate.rs`

- [ ] **Step 1: Write failing test**

Append to `tests` mod:

```rust
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
```

- [ ] **Step 2: Run, confirm fail**

- [ ] **Step 3: Extend helper**

In `migrate_legacy_config`, after the `if let Some(t) = root.get("twitch")` block append:

```rust
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
        .and_then(|s| humantime::parse_duration(s).ok())
        .map(|d| d.as_secs());
    out.web.mod_check_refresh_secs = v
        .get("mod_check_refresh")
        .and_then(toml::Value::as_str)
        .and_then(|s| humantime::parse_duration(s).ok())
        .map(|d| d.as_secs());
}
```

If `humantime` isn't an explicit dep, check `Cargo.toml` — it comes in via `humantime-serde`. If direct `humantime::` isn't accessible, add to `crates/core/Cargo.toml`:

```toml
humantime = "2"
```

and stage `Cargo.lock` in the same commit (per `feedback_cargo_lock_with_dep` memory).

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p twitch-1337-core settings::migrate::tests`

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/settings/migrate.rs crates/core/Cargo.toml Cargo.lock
git commit -m "feat(settings): migrate legacy suspend/aviationstack/web keys"
```

---

## Task 13: Wire migration call into `main.rs`

**Files:**
- Modify: `crates/twitch-1337/src/main.rs`

- [ ] **Step 1: Locate the current `[ai]` migration call**

```bash
rg -n "ai_migrated_v2|migrate_legacy_ai" crates/twitch-1337/src/main.rs
```

- [ ] **Step 2: Add a parallel v3 block right after the existing `[ai]` migration**

The sentinel path is `$DATA_DIR/.config_migrated_v3`. Read the raw toml::Value returned from `load_configuration`, call `migrate_legacy_config`, and apply each non-empty section via `SettingsStore::apply`.

Insert (adjust local bindings to match what's in scope: `raw_toml: &toml::Value`, `settings_store: &Arc<SettingsStore>`, `data_dir: &Path`):

```rust
let sentinel_v3 = data_dir.join(".config_migrated_v3");
if !sentinel_v3.exists() {
    let patch = twitch_1337_core::settings::migrate::migrate_legacy_config(&raw_toml)?;
    let twitch_empty = patch.twitch == Default::default();
    let aviation_empty = patch.aviationstack == Default::default();
    let suspend_empty = patch.suspend == Default::default();
    let web_empty = patch.web == Default::default();
    if !(twitch_empty && aviation_empty && suspend_empty && web_empty) {
        settings_store
            .apply(
                patch,
                twitch_1337_core::settings::Actor {
                    user_id: "system".into(),
                    user_login: "v3-migration".into(),
                },
            )
            .await?;
        tracing::info!("migrated legacy config.toml keys into settings.ron (v3)");
    }
    tokio::fs::write(&sentinel_v3, b"v3").await?;
}
```

Also after the migration call, emit a single warn listing any migrated keys that still appear in config.toml — these would be silently ignored. Pseudocode:

```rust
let legacy_keys = [
    ("twitch.expected_latency", raw_toml.get("twitch").and_then(|t| t.get("expected_latency"))),
    ("twitch.hidden_admins", raw_toml.get("twitch").and_then(|t| t.get("hidden_admins"))),
    ("twitch.viewer_allowlist", raw_toml.get("twitch").and_then(|t| t.get("viewer_allowlist"))),
    ("twitch.owner", raw_toml.get("twitch").and_then(|t| t.get("owner"))),
    ("twitch.admin_channel", raw_toml.get("twitch").and_then(|t| t.get("admin_channel"))),
    ("twitch.ai_channel", raw_toml.get("twitch").and_then(|t| t.get("ai_channel"))),
    ("suspend.default_duration_secs", raw_toml.get("suspend").and_then(|t| t.get("default_duration_secs"))),
    ("aviationstack.enabled", raw_toml.get("aviationstack").and_then(|t| t.get("enabled"))),
    ("aviationstack.base_url", raw_toml.get("aviationstack").and_then(|t| t.get("base_url"))),
    ("aviationstack.timeout_secs", raw_toml.get("aviationstack").and_then(|t| t.get("timeout_secs"))),
    ("web.session_ttl", raw_toml.get("web").and_then(|t| t.get("session_ttl"))),
    ("web.mod_check_refresh", raw_toml.get("web").and_then(|t| t.get("mod_check_refresh"))),
];
let stale: Vec<&str> = legacy_keys.iter().filter_map(|(k, v)| v.is_some().then_some(*k)).collect();
if sentinel_v3.exists() && !stale.is_empty() {
    tracing::warn!(?stale, "legacy config.toml keys are now ignored after v3 migration; remove them from config.toml");
}
```

- [ ] **Step 3: Run the bot in a temp data dir**

Manual smoke (optional): point `DATA_DIR` at `/tmp/twitch-1337-mig-test`, copy `config.toml` with legacy keys, `cargo run`, confirm `.config_migrated_v3` is created and `settings.ron` contains the new section values.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/twitch-1337/src/main.rs
git commit -m "feat(main): run v3 settings migration once on startup"
```

---

## Task 14: Wire `expected_latency` from settings

**Files:**
- Modify: `crates/core/src/twitch/handlers/latency.rs` (init seed read site)
- Modify: `crates/core/src/lib.rs` (call site in `run_bot`)

- [ ] **Step 1: Find the current seed**

```bash
rg -n "expected_latency" crates/core/src
```

- [ ] **Step 2: Replace `config.twitch.expected_latency` reads with `settings.load().twitch.expected_latency`**

Wherever a `Configuration` value's `expected_latency` is read, replace with `settings_handle.load().twitch.expected_latency` (clone the `Arc<Settings>` once at startup since the latency monitor reads it only as an init seed).

- [ ] **Step 3: Run tests**

Run: `cargo nextest run --workspace`

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor(latency): seed from settings.twitch.expected_latency"
```

---

## Task 15: Wire `hidden_admins` / `viewer_allowlist` / `owner` from settings

**Files:**
- Modify: any handler that reads `config.twitch.hidden_admins` (locate with rg)
- Modify: `crates/web/src/auth/` (viewer_allowlist + owner gates)

- [ ] **Step 1: Find read sites**

```bash
rg -n "hidden_admins|viewer_allowlist|\.owner\b" crates --type rust
```

- [ ] **Step 2: Replace each read with the live settings handle**

Handlers already receive `SettingsHandle`. For each call site, replace `config.twitch.hidden_admins` with `settings.load().twitch.hidden_admins`, and likewise for `viewer_allowlist` and `owner`.

For the web crate's role resolution, `WebState` already exposes `settings: SettingsHandle` — read from it instead of cloning the bootstrap `Configuration`.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run --workspace`

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor(permissions): read hidden_admins/viewer_allowlist/owner from settings"
```

---

## Task 16: Wire `admin_channel` / `ai_channel` from settings (restart fields)

**Files:**
- Modify: `crates/core/src/twitch/setup.rs` (IRC join targets)
- Modify: `crates/core/src/lib.rs` (handler spawn / message routing)
- Modify: `crates/core/src/twitch/handlers/spawn.rs` if relevant

These are read once at startup. Locate every reference to `config.twitch.admin_channel` / `config.twitch.ai_channel` and replace with the value pulled from `settings_handle.load()` at startup, captured into a local for the rest of `run_bot`.

- [ ] **Step 1: Identify read sites**

```bash
rg -n "admin_channel|ai_channel" crates --type rust
```

- [ ] **Step 2: Replace startup reads**

At the top of `run_bot` (or wherever the channels are first resolved), after the v3 migration has run, read:

```rust
let snapshot = settings_handle.load();
let admin_channel = snapshot.twitch.admin_channel.clone();
let ai_channel = snapshot.twitch.ai_channel.clone();
```

Pass these into the IRC setup. The bootstrap channel from `config.twitch.channel` stays.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run --workspace`

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor(twitch): read admin/ai channel from settings (restart-required)"
```

---

## Task 17: Wire `suspend.default_duration_secs` from settings

**Files:**
- Modify: `crates/core/src/commands/suspend.rs`

- [ ] **Step 1: Find read site**

```bash
rg -n "default_duration_secs" crates --type rust
```

- [ ] **Step 2: Replace `SuspendConfig` read with `settings.load().suspend.default_duration_secs`**

The suspend command handler already receives `SettingsHandle` (or can — pass it through `Services`). Replace the captured `SuspendConfig` with a `SettingsHandle` and read live.

- [ ] **Step 3: Test + commit**

Run: `cargo nextest run --workspace`

```bash
git add -A
git commit -m "refactor(suspend): read default duration from settings"
```

---

## Task 18: Wire aviationstack settings (restart)

**Files:**
- Modify: `crates/core/src/aviation/client.rs` (HTTP client init)
- Modify: `crates/core/src/lib.rs` (`run_bot` startup branch that creates the client)

- [ ] **Step 1: Replace config reads with settings reads at startup**

```bash
rg -n "aviationstack" crates/core/src/aviation
```

Where the client is built, replace `config.aviationstack.base_url` / `timeout_secs` / `enabled` with the corresponding `settings.load().aviationstack.*` values. `api_key` continues to come from `config.aviationstack.api_key` (or from `config.ai.api_key` — whatever the current source is; do not change). If `settings.aviationstack.enabled` is `false`, skip client construction entirely.

- [ ] **Step 2: Run tests**

Run: `cargo nextest run --workspace`

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor(aviation): build client from settings.aviationstack (restart-required)"
```

---

## Task 19: Wire web `session_ttl_secs` + `mod_check_refresh_secs` from settings

**Files:**
- Modify: `crates/web/src/auth/session.rs` (new session creation) — or wherever TTL is read
- Modify: `crates/web/src/auth/mods.rs` or equivalent (mod cache refresh interval) — confirm with rg

- [ ] **Step 1: Find read sites**

```bash
rg -n "session_ttl|mod_check_refresh" crates/web/src
```

- [ ] **Step 2: Read live from `WebState.settings`**

For each read, replace `config.web.session_ttl` with:

```rust
let ttl = std::time::Duration::from_secs(state.settings.load().web.session_ttl_secs);
```

and likewise for `mod_check_refresh`.

- [ ] **Step 3: Test + commit**

```bash
git add -A
git commit -m "refactor(web): read session_ttl + mod_check_refresh from settings"
```

---

## Task 20: Remove migrated fields from `Configuration`

**Files:**
- Modify: `crates/core/src/config.rs`

The bootstrap `Configuration` no longer carries these fields. After the migration sentinel runs, the raw toml::Value is the only place legacy keys are read from.

- [ ] **Step 1: Slim `TwitchConfiguration`**

Drop the migrated fields:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct TwitchConfiguration {
    pub channel: String,
    pub username: String,
    pub refresh_token: SecretString,
    pub client_id: SecretString,
    pub client_secret: SecretString,
}
```

Likewise drop the `default_expected_latency` helper and the `#[serde(default)]` on `expected_latency`, plus the `hidden_admins`, `viewer_allowlist`, `owner`, `admin_channel`, `ai_channel` fields. **Use `#[serde(deny_unknown_fields)] is intentionally NOT added** — we want legacy keys to silently parse (and be warned about in main.rs), not error.

- [ ] **Step 2: Drop `AviationstackConfig` and `SuspendConfig` types**

The aviationstack `api_key` migrates into a dedicated bootstrap shape mirroring `AiBootstrap`. Replace `AviationstackConfig` with:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct AviationstackBootstrap {
    pub api_key: SecretString,
}
```

Remove `SuspendConfig` entirely; it has no remaining fields.

- [ ] **Step 3: Slim `WebConfig`**

Drop `session_ttl` and `mod_check_refresh`; keep `enabled`, `bind_addr`, `public_url`, `session_secret`.

- [ ] **Step 4: Slim `Configuration`**

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct Configuration {
    pub twitch: TwitchConfiguration,
    #[serde(default)]
    pub aviationstack: Option<AviationstackBootstrap>,
    #[serde(default)]
    pub ai: Option<AiBootstrap>,
    #[serde(default)]
    pub schedules: Vec<ScheduleConfig>,
    #[serde(default)]
    pub web: WebConfig,
}
```

`schedules` stays in PR 1 (its own PR moves it).

- [ ] **Step 5: Slim `validate_config`**

Delete the rules that now live in `Settings::validate`:
- `expected_latency` bound (gone)
- `admin_channel` / `ai_channel` non-empty + cross-channel checks (gone)
- `suspend.default_duration_secs` bound (gone)
- `aviationstack.base_url` URL parse + `timeout_secs` > 0 + `enabled→api_key` check (replaced by lighter `api_key` non-empty check inside the new bootstrap shape)
- `web.session_ttl` + `web.mod_check_refresh` range checks (gone)

Keep:
- `twitch.channel` / `username` non-empty
- `web.enabled` → secret length, https public_url
- `ai.api_key` non-empty when present
- aviationstack.api_key non-empty when present
- schedule validation (untouched — covered in PR 2)

- [ ] **Step 6: Update `Configuration::test_default`**

Drop fields that no longer exist.

- [ ] **Step 7: Build + test**

Run:
```bash
cargo check --workspace
cargo nextest run --workspace
```

Chase down compile errors at any remaining `config.twitch.expected_latency` etc. references (Tasks 14–19 should already have covered them).

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/config.rs
git commit -m "refactor(config): slim Configuration to bootstrap + secrets only"
```

---

## Task 21: Dashboard card — Twitch · Permissions

**Files:**
- Modify: `crates/web/src/routes/settings.rs`
- Modify: `crates/web/templates/settings/index.html`
- Modify: `crates/web/templates/settings/` (any partials)

Follow the pattern of an existing card such as `CooldownsForm` or `AiBehaviorForm`.

- [ ] **Step 1: Form struct + into_overrides**

In `crates/web/src/routes/settings.rs`, after the existing form structs add:

```rust
#[derive(Default, Deserialize)]
struct TwitchPermissionsForm {
    #[serde(default)]
    twitch_hidden_admins: Option<String>,   // newline-separated
    #[serde(default)]
    twitch_viewer_allowlist: Option<String>,
    #[serde(default)]
    twitch_owner: Option<String>,
}

fn parse_id_list(s: &str) -> Vec<String> {
    s.lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect()
}

impl TwitchPermissionsForm {
    fn into_overrides(self) -> TwitchOverrides {
        TwitchOverrides {
            hidden_admins: self.twitch_hidden_admins.as_deref().map(parse_id_list),
            viewer_allowlist: self.twitch_viewer_allowlist.as_deref().map(parse_id_list),
            owner: tri_state(self.twitch_owner),
            ..Default::default()
        }
    }
}
```

Add `TwitchOverrides` to the existing `use twitch_1337_core::settings::overrides::{...}` import.

- [ ] **Step 2: Route handler**

Mirror `save_cooldowns`. Add:

```rust
async fn save_twitch_permissions(
    State(state): State<WebState>,
    Extension(session): Extension<Session>,
    Form(form): Form<TwitchPermissionsForm>,
) -> Result<Redirect, WebError> {
    let patch = SettingsOverrides {
        twitch: form.into_overrides(),
        ..Default::default()
    };
    state.settings_store.apply(patch, actor_from_session(&session)).await?;
    Ok(Redirect::to("/settings#twitch-permissions"))
}
```

Wire into `owner_router`:

```rust
.route("/settings/twitch_permissions", post(save_twitch_permissions))
```

And add `TwitchPermissions` to the `reset/{section}` parser used by the existing reset handler.

- [ ] **Step 3: Template card**

Append a new card section to `crates/web/templates/settings/index.html`:

```html
<section class="card" id="twitch-permissions">
  <h2>Twitch · Permissions</h2>
  <form method="post" action="/settings/twitch_permissions">
    <input type="hidden" name="csrf" value="{{ csrf }}">
    <label>Hidden admins (one Twitch user ID per line)
      <textarea name="twitch_hidden_admins" rows="4">{{ current.twitch.hidden_admins|join("\n") }}</textarea>
    </label>
    <label>Viewer allowlist (one Twitch user ID per line)
      <textarea name="twitch_viewer_allowlist" rows="4">{{ current.twitch.viewer_allowlist|join("\n") }}</textarea>
    </label>
    <label>Owner (Twitch user ID, empty to clear)
      <input type="text" name="twitch_owner"
             value="{{ current.twitch.owner.as_deref().unwrap_or(&\"\".to_string()) }}">
    </label>
    <button type="submit">Save</button>
    <form method="post" action="/settings/reset/twitch_permissions" style="display:inline">
      <input type="hidden" name="csrf" value="{{ csrf }}">
      <button type="submit">Reset</button>
    </form>
  </form>
</section>
```

(Exact askama syntax matches existing cards — copy whichever helper they use to render lists.)

- [ ] **Step 4: Integration test**

Add to `crates/web/tests/settings_route.rs`:

```rust
#[tokio::test]
async fn twitch_permissions_save_persists() {
    let h = TestHarness::new().await;
    h.post_form(
        "/settings/twitch_permissions",
        &[
            ("csrf", &h.csrf),
            ("twitch_hidden_admins", "111\n222\n"),
            ("twitch_owner", "777"),
        ],
    )
    .await
    .assert_redirect("/settings#twitch-permissions");
    let s = h.settings_handle.load();
    assert_eq!(s.twitch.hidden_admins, vec!["111", "222"]);
    assert_eq!(s.twitch.owner.as_deref(), Some("777"));
}
```

(Use whatever helper exists in `crates/web/tests/helpers/mod.rs`; mirror an existing test.)

- [ ] **Step 5: Run + commit**

Run: `cargo nextest run -p twitch-1337-web settings_route`

```bash
git add crates/web/src/routes/settings.rs crates/web/templates/settings/index.html crates/web/tests/settings_route.rs
git commit -m "feat(web): add twitch permissions settings card"
```

---

## Task 22: Dashboard card — Twitch · Channels

**Files:**
- Modify: `crates/web/src/routes/settings.rs`
- Modify: `crates/web/templates/settings/index.html`
- Modify: `crates/web/tests/settings_route.rs`

Mirror Task 21 with these fields. Form:

```rust
#[derive(Default, Deserialize)]
struct TwitchChannelsForm {
    #[serde(default)]
    twitch_expected_latency: Option<u32>,
    #[serde(default)]
    twitch_admin_channel: Option<String>,
    #[serde(default)]
    twitch_ai_channel: Option<String>,
}

impl TwitchChannelsForm {
    fn into_overrides(self) -> TwitchOverrides {
        TwitchOverrides {
            expected_latency: self.twitch_expected_latency,
            admin_channel: tri_state(self.twitch_admin_channel),
            ai_channel: tri_state(self.twitch_ai_channel),
            ..Default::default()
        }
    }
}
```

Route: `POST /settings/twitch_channels`, reset `/settings/reset/twitch_channels`.

Template: same shape as permissions card; add a small "restart required" badge to the two channel fields (e.g. `<span class="restart-badge">restart required</span>`).

Test: assert save persists and `Settings::resolve` exposes the new values; assert validation rejects `admin_channel == config.twitch.channel`.

- [ ] **Step 1: Form + handler + route**
- [ ] **Step 2: Template card with restart badge**
- [ ] **Step 3: Integration test for save + validation reject**
- [ ] **Step 4: Run + commit**

```bash
git add -A
git commit -m "feat(web): add twitch channels settings card"
```

---

## Task 23: Dashboard card — Aviationstack

**Files:** same shape as Task 22.

Form fields: `aviationstack_enabled` (checkbox), `aviationstack_base_url`, `aviationstack_timeout_secs`.

Route: `POST /settings/aviationstack`, reset `/settings/reset/aviationstack`.

Template: include restart badge on all fields.

Test: save round-trips; bad URL rejects with field error on `aviationstack.base_url`.

- [ ] **Step 1: Form + handler + route**
- [ ] **Step 2: Template card with restart badges**
- [ ] **Step 3: Integration test**
- [ ] **Step 4: Run + commit**

```bash
git add -A
git commit -m "feat(web): add aviationstack settings card"
```

---

## Task 24: Dashboard card — Suspend

Form field: `suspend_default_duration_secs`.

Route: `POST /settings/suspend`, reset `/settings/reset/suspend`.

Test: save persists; out-of-range value renders error.

- [ ] **Step 1: Form + handler + route**
- [ ] **Step 2: Template card**
- [ ] **Step 3: Integration test**
- [ ] **Step 4: Run + commit**

```bash
git add -A
git commit -m "feat(web): add suspend settings card"
```

---

## Task 25: Dashboard card — Web · Sessions

Form fields: `web_session_ttl_secs`, `web_mod_check_refresh_secs`.

Route: `POST /settings/web_runtime`, reset `/settings/reset/web_runtime`.

Test: save persists; range violations render errors.

- [ ] **Step 1: Form + handler + route**
- [ ] **Step 2: Template card**
- [ ] **Step 3: Integration test**
- [ ] **Step 4: Run + commit**

```bash
git add -A
git commit -m "feat(web): add web sessions settings card"
```

---

## Task 26: End-to-end migration integration test

**Files:**
- Create: `crates/core/tests/v3_migration.rs` (new integration test file)

- [ ] **Step 1: Write the test**

```rust
//! Integration test: legacy config.toml -> settings.ron migration runs once,
//! is idempotent on second startup, and respects the sentinel.

use std::sync::Arc;
use tempfile::tempdir;
use twitch_1337_core::settings::{
    Actor, SettingsStore,
    audit::MemoryAuditLog,
    migrate::migrate_legacy_config,
};

#[tokio::test]
async fn legacy_keys_migrate_then_idempotent() {
    let dir = tempdir().expect("tmp");
    let audit = Arc::new(MemoryAuditLog::default());
    let (store, _h) = SettingsStore::open(dir.path(), audit.clone(), "main").expect("open");

    let raw = r#"
        [twitch]
        channel = "main"
        username = "bot"
        refresh_token = "r"
        client_id = "i"
        client_secret = "s"
        expected_latency = 250
        admin_channel = "admins"

        [suspend]
        default_duration_secs = 900
    "#;
    let value: toml::Value = toml::from_str(raw).expect("parse");

    // First migration
    let patch = migrate_legacy_config(&value).expect("migrate");
    store
        .apply(patch, Actor { user_id: "system".into(), user_login: "v3-migration".into() })
        .await
        .expect("apply");
    let s = store.handle().load();
    assert_eq!(s.twitch.expected_latency, 250);
    assert_eq!(s.twitch.admin_channel.as_deref(), Some("admins"));
    assert_eq!(s.suspend.default_duration_secs, 900);

    // Second migration: same patch idempotently re-applied; resolved settings unchanged
    let patch2 = migrate_legacy_config(&value).expect("migrate");
    store
        .apply(patch2, Actor { user_id: "system".into(), user_login: "v3-migration".into() })
        .await
        .expect("apply");
    let s2 = store.handle().load();
    assert_eq!(*s, *s2);
}
```

- [ ] **Step 2: Run test**

Run: `cargo nextest run -p twitch-1337-core --test v3_migration`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/v3_migration.rs
git commit -m "test(settings): e2e v3 migration is idempotent"
```

---

## Task 27: Update `config.toml.example` and `CLAUDE.md`

**Files:**
- Modify: `crates/twitch-1337/config.toml.example`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Strip migrated keys from `config.toml.example`**

Remove (or convert to "managed in dashboard now" comments) every key migrated in this PR:
- `twitch.expected_latency`, `twitch.hidden_admins`, `twitch.viewer_allowlist`, `twitch.owner`, `twitch.admin_channel`, `twitch.ai_channel`
- `[suspend]` block (entire)
- `[aviationstack]` block — keep `api_key`, drop `enabled` / `base_url` / `timeout_secs`
- `[web]` block — keep `enabled`, `bind_addr`, `public_url`, `session_secret`; drop `session_ttl` and `mod_check_refresh`

Replace with a short note in each section: `# Managed via /settings → <card name>`.

- [ ] **Step 2: Update CLAUDE.md**

Update the "Config" section to reflect the new split. Add a short paragraph: "Following v3 schema bump (2026-05-24), `[twitch]` runtime fields, `[suspend]`, non-secret `[aviationstack]`, and non-bootstrap `[web]` keys are dashboard-managed; one-shot migration on first v3 launch is gated by `$DATA_DIR/.config_migrated_v3`."

- [ ] **Step 3: Commit**

```bash
git add crates/twitch-1337/config.toml.example CLAUDE.md
git commit -m "docs: reflect v3 settings split"
```

---

## Task 28: Open PR

- [ ] **Step 1: Push branch**

```bash
git push -u origin spec/config-to-settings-final-migration
```

- [ ] **Step 2: Open PR**

```bash
gh pr create --title "feat: migrate remaining config knobs to dashboard settings (v3)" --body "$(cat <<'EOF'
## Summary
- Bump settings schema v2 → v3.
- Move `[twitch]` runtime fields, `[suspend]`, non-secret `[aviationstack]`, and non-bootstrap `[web]` keys into `settings.ron`.
- One-shot migration on first v3 launch (sentinel `$DATA_DIR/.config_migrated_v3`); audit-logged.
- 5 new dashboard cards on `/settings`.
- `Schedules` migration deferred to a follow-up PR.

## Test plan
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo nextest run --workspace`
- [ ] Manual: legacy `config.toml` → sentinel created, settings.ron contains migrated values.
- [ ] Manual: dashboard cards save + reset round-trip.
- [ ] Manual: second startup is idempotent.

Spec: `docs/superpowers/specs/2026-05-24-config-to-settings-final-migration-design.md`
Plan: `docs/superpowers/plans/2026-05-24-config-to-settings-final-migration-pr1.md`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
