//! Owner of `$DATA_DIR/settings.ron`. Serializes writes, validates, swaps
//! the shared `SettingsHandle`, and appends an audit log entry per apply.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::Utc;
use tokio::sync::{Mutex, Notify};
use tracing::{error, info, warn};

use super::audit::{AuditChange, AuditEntry, AuditLog, berlin_now};
use super::overrides::SettingsOverrides;
use super::{Schedule, Settings, SettingsError, SettingsHandle, SettingsSection};

const FILE_NAME: &str = "settings.ron";

#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: String,
    pub user_login: String,
}

pub struct SettingsStore {
    path: PathBuf,
    defaults: Settings,
    handle: SettingsHandle,
    audit: Arc<dyn AuditLog>,
    write_lock: Mutex<()>,
    change_notify: Arc<Notify>,
    boot_channel: String,
}

impl SettingsStore {
    /// Open the store from `$DATA_DIR`. Reads the existing `settings.ron`
    /// (if any), resolves against compile-time defaults, validates, and
    /// returns the `(store, handle)` pair. A corrupt or out-of-bound file
    /// is renamed to `settings.ron.quarantine-<unix_ts>` and the load
    /// falls back to compile defaults so the bot can still boot.
    pub fn open(
        data_dir: &Path,
        audit: Arc<dyn AuditLog>,
        boot_channel: &str,
    ) -> Result<(Arc<Self>, SettingsHandle), SettingsError> {
        let path = data_dir.join(FILE_NAME);
        let defaults = Settings::compiled_defaults();
        let overrides = load_or_quarantine(&path)?;
        let resolved = Settings::resolve(&defaults, &overrides);
        let ctx = super::ValidationContext {
            channel: boot_channel.to_owned(),
        };
        if let Err(errs) = resolved.validate(&ctx) {
            warn!(
                ?errs,
                "settings.ron failed validation; falling back to compile defaults"
            );
            quarantine(&path)?;
            let handle = Arc::new(ArcSwap::from_pointee(defaults.clone()));
            let store = Arc::new(Self {
                path,
                defaults,
                handle: handle.clone(),
                audit,
                write_lock: Mutex::new(()),
                change_notify: Arc::new(Notify::new()),
                boot_channel: boot_channel.to_owned(),
            });
            return Ok((store, handle));
        }
        let handle = Arc::new(ArcSwap::from_pointee(resolved));
        let store = Arc::new(Self {
            path,
            defaults,
            handle: handle.clone(),
            audit,
            write_lock: Mutex::new(()),
            change_notify: Arc::new(Notify::new()),
            boot_channel: boot_channel.to_owned(),
        });
        info!("settings store opened");
        Ok((store, handle))
    }

    pub fn handle(&self) -> &SettingsHandle {
        &self.handle
    }

    pub fn change_notify(&self) -> Arc<Notify> {
        self.change_notify.clone()
    }

    pub fn defaults(&self) -> &Settings {
        &self.defaults
    }

    pub async fn apply(
        &self,
        patch: SettingsOverrides,
        actor: Actor,
    ) -> Result<Settings, SettingsError> {
        self.apply_with(move |current| merge_into(current, &patch), actor)
            .await
    }

    /// Shared tail of every mutating path: persist the overrides atomically,
    /// swap the live handle, append an audit entry for any diff, and signal
    /// change waiters. Callers resolve (and, where appropriate, validate)
    /// before handing the resolved snapshot here.
    async fn commit(
        &self,
        current: &SettingsOverrides,
        prior_resolved: &Settings,
        resolved: Settings,
        actor: Actor,
    ) -> Result<Settings, SettingsError> {
        crate::util::persist::atomic_save_ron_async(current, &self.path).await?;
        self.handle.store(Arc::new(resolved.clone()));
        let changes = diff_changes(prior_resolved, &resolved);
        if !changes.is_empty() {
            let entry = AuditEntry {
                ts: berlin_now(Utc::now()),
                actor_id: actor.user_id,
                actor_login: actor.user_login,
                changes,
            };
            if let Err(e) = self.audit.append(&entry) {
                error!(error = ?e, "audit append failed");
            }
        }
        // Notify unconditionally even on a no-op write: idempotent reload is
        // cheap, and skipping based on diff_changes would couple change
        // notifications to the audit-log diff logic.
        self.change_notify.notify_waiters();
        Ok(resolved)
    }

    /// Read-modify-write inside the store's write_lock. The closure receives
    /// `&mut SettingsOverrides` (the freshly loaded overrides) and may mutate
    /// any section. The store re-resolves, validates, persists, and notifies
    /// exactly as `apply` does — but the snapshot the closure sees cannot be
    /// stale (PR #227 review F4: prevents lost-update races on concurrent
    /// /schedules CRUD POSTs).
    pub async fn apply_with<F>(&self, mutate: F, actor: Actor) -> Result<Settings, SettingsError>
    where
        F: FnOnce(&mut SettingsOverrides),
    {
        let _g = self.write_lock.lock().await;
        let mut current = load_overrides_async(&self.path).await?.unwrap_or_default();
        let prior_resolved = Settings::resolve(&self.defaults, &current);
        mutate(&mut current);
        let resolved = Settings::resolve(&self.defaults, &current);
        let ctx = super::ValidationContext {
            channel: self.boot_channel.clone(),
        };
        if let Err(errs) = resolved.validate(&ctx) {
            return Err(SettingsError::Validation(errs));
        }
        self.commit(&current, &prior_resolved, resolved, actor)
            .await
    }

    pub async fn reset(
        &self,
        section: SettingsSection,
        actor: Actor,
    ) -> Result<Settings, SettingsError> {
        let _g = self.write_lock.lock().await;
        let mut current = load_overrides_async(&self.path).await?.unwrap_or_default();
        let prior_resolved = Settings::resolve(&self.defaults, &current);
        match section {
            SettingsSection::Cooldowns => current.cooldowns = Default::default(),
            SettingsSection::Pings => current.pings = Default::default(),
            SettingsSection::AiConnection => current.ai.connection = Default::default(),
            SettingsSection::AiBehavior => current.ai.behavior = Default::default(),
            SettingsSection::AiHistory => current.ai.history = Default::default(),
            SettingsSection::AiMemory => current.ai.memory = Default::default(),
            SettingsSection::AiDreamer => current.ai.dreamer = Default::default(),
            SettingsSection::AiPrefill => current.ai.prefill = Default::default(),
            SettingsSection::AiWeb => current.ai.web = Default::default(),
            SettingsSection::AiEmotes => current.ai.emotes = Default::default(),
            SettingsSection::AiMedia => current.ai.media = Default::default(),
            SettingsSection::TwitchPermissions => {
                current.twitch.hidden_admins = None;
                current.twitch.viewer_allowlist = None;
            }
            SettingsSection::TwitchChannels => {
                current.twitch.admin_channel = None;
                current.twitch.ai_channel = None;
                // expected_latency is a network-tuning knob; intentionally not
                // reset here so channel resets don't clobber latency tweaks.
                // Users can clear it by setting the field to the default value.
            }
            SettingsSection::Aviationstack => current.aviationstack = Default::default(),
            SettingsSection::Suspend => current.suspend = Default::default(),
            SettingsSection::WebRuntime => current.web = Default::default(),
            SettingsSection::Schedules => current.schedules = None,
        }
        let resolved = Settings::resolve(&self.defaults, &current);
        self.commit(&current, &prior_resolved, resolved, actor)
            .await
    }
}

fn load_or_quarantine(path: &Path) -> Result<SettingsOverrides, SettingsError> {
    match load_overrides(path) {
        Ok(Some(o)) => Ok(o),
        Ok(None) => Ok(SettingsOverrides::default()),
        Err(e) => {
            warn!(error = ?e, "settings.ron is corrupt; quarantining");
            quarantine(path)?;
            Ok(SettingsOverrides::default())
        }
    }
}

fn migrate_then_parse(body: &str) -> Result<SettingsOverrides, ron::error::SpannedError> {
    let mut raw: ron::Value = ron::from_str(body)?;
    if !super::migrate::migrate_schedules_v3_to_v4(&mut raw) {
        // No v3→v4 migration was needed. Parse the original body directly —
        // avoiding the Value→string→typed round-trip that breaks in RON 0.12
        // because `ron::Value::Map` re-serializes as `{...}` map syntax while
        // `SettingsOverrides` is a named struct that requires `(...)` or
        // `StructName(...)` syntax.
        return ron::from_str(body);
    }
    tracing::info!("migrated schedules v3 → v4 in-memory");
    // Migration was applied. Deserialize directly from the mutated ron::Value
    // (which acts as a serde source) rather than re-serializing to a string.
    // ron::Value uses `forward_to_deserialize_any` so serde struct/map visitors
    // work correctly, including internally-tagged Trigger enums.
    raw.into_rust::<SettingsOverrides>().map_err(|e| {
        // ron::Error → ron::error::SpannedError: attach a dummy span so the
        // call-site error type is satisfied. Migration failures are fatal at
        // startup, so the exact span is informational only.
        ron::error::SpannedError {
            code: e,
            span: ron::error::Span {
                start: ron::error::Position { line: 0, col: 0 },
                end: ron::error::Position { line: 0, col: 0 },
            },
        }
    })
}

fn load_overrides(path: &Path) -> Result<Option<SettingsOverrides>, SettingsError> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path)?;
    let parsed: SettingsOverrides = migrate_then_parse(&body)?;
    Ok(Some(parsed))
}

async fn load_overrides_async(path: &Path) -> Result<Option<SettingsOverrides>, SettingsError> {
    if !tokio::fs::try_exists(path).await? {
        return Ok(None);
    }
    let body = tokio::fs::read_to_string(path).await?;
    let parsed: SettingsOverrides = migrate_then_parse(&body)?;
    Ok(Some(parsed))
}

fn quarantine(path: &Path) -> Result<(), SettingsError> {
    if !path.exists() {
        return Ok(());
    }
    let ts = chrono::Utc::now().timestamp();
    let target = path.with_extension(format!("ron.quarantine-{ts}"));
    std::fs::rename(path, &target)?;
    warn!(target = ?target, "settings.ron quarantined");
    Ok(())
}

fn merge_into(into: &mut SettingsOverrides, patch: &SettingsOverrides) {
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

fn diff_changes(prior: &Settings, next: &Settings) -> Vec<AuditChange> {
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
    use crate::settings::audit::MemoryAuditLog;
    use crate::settings::overrides::CooldownsOverrides;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<SettingsStore>,
        SettingsHandle,
        Arc<MemoryAuditLog>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = Arc::new(MemoryAuditLog::new());
        let (store, handle) =
            SettingsStore::open(dir.path(), log.clone(), "test_chan").expect("open empty store");
        (dir, store, handle, log)
    }

    #[tokio::test]
    async fn empty_dir_yields_compile_defaults() {
        let (_dir, _store, handle, _log) = fixture();
        assert_eq!(**handle.load(), Settings::compiled_defaults());
    }

    #[tokio::test]
    async fn apply_persists_writes_handle_and_audits() {
        let (_dir, store, handle, log) = fixture();
        let patch = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(15),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let actor = Actor {
            user_id: "1".into(),
            user_login: "tester".into(),
        };
        store.apply(patch, actor).await.expect("apply");
        assert_eq!(handle.load().cooldowns.ai, 15);
        // round-trip from disk
        let dropped_handle =
            SettingsStore::open(store.path.parent().unwrap(), log.clone(), "test_chan")
                .expect("reopen")
                .1;
        assert_eq!(dropped_handle.load().cooldowns.ai, 15);
        let entries = log.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].changes.len(), 1);
        assert_eq!(entries[0].changes[0].key, "cooldowns.ai");
    }

    #[tokio::test]
    async fn apply_rejects_out_of_bound_with_validation_error() {
        let (_dir, store, _handle, _log) = fixture();
        let patch = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(0),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let actor = Actor {
            user_id: "1".into(),
            user_login: "tester".into(),
        };
        match store.apply(patch, actor).await {
            Err(SettingsError::Validation(errs)) => {
                assert_eq!(errs[0].field, "cooldowns.ai");
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reset_clears_section_back_to_defaults() {
        let (_dir, store, handle, _log) = fixture();
        let actor = Actor {
            user_id: "1".into(),
            user_login: "tester".into(),
        };
        let patch = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(15),
                news: Some(45),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        store.apply(patch, actor.clone()).await.expect("apply");
        assert_eq!(handle.load().cooldowns.ai, 15);
        store
            .reset(SettingsSection::Cooldowns, actor)
            .await
            .expect("reset");
        let s = handle.load();
        assert_eq!(s.cooldowns.ai, Settings::compiled_defaults().cooldowns.ai);
        assert_eq!(
            s.cooldowns.news,
            Settings::compiled_defaults().cooldowns.news
        );
    }

    #[tokio::test]
    async fn v2_round_trip_persists_ai_overrides() {
        let (_dir, store, handle, _log) = fixture();
        let patch = SettingsOverrides {
            ai: crate::settings::overrides::AiOverrides {
                connection: crate::settings::overrides::AiConnectionOverrides {
                    model: Some("o5-pro".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        let actor = Actor {
            user_id: "1".into(),
            user_login: "tester".into(),
        };
        store.apply(patch, actor).await.expect("apply");
        assert_eq!(handle.load().ai.connection.model, "o5-pro");
        let reopened = SettingsStore::open(
            store.path.parent().unwrap(),
            Arc::new(crate::settings::audit::MemoryAuditLog::new()),
            "test_chan",
        )
        .expect("reopen")
        .1;
        assert_eq!(reopened.load().ai.connection.model, "o5-pro");
        let _ = handle;
    }

    #[tokio::test]
    async fn corrupt_ron_falls_back_to_defaults_and_quarantines() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(FILE_NAME), "not valid ron").expect("write garbage");
        let log = Arc::new(MemoryAuditLog::new());
        let (_store, handle) =
            SettingsStore::open(dir.path(), log, "test_chan").expect("open should not fail");
        assert_eq!(**handle.load(), Settings::compiled_defaults());
        // settings.ron has been renamed away
        assert!(!dir.path().join(FILE_NAME).exists());
        let quarantined = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("settings.ron.quarantine-")
            });
        assert!(quarantined, "quarantine file must exist");
    }

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

    #[tokio::test]
    async fn reset_schedules_clears_override() {
        use crate::schedule::{Schedule, Trigger, WeekdaySet};
        let dir = tempfile::tempdir().expect("tmp");
        let audit = std::sync::Arc::new(crate::settings::audit::MemoryAuditLog::default());
        let (store, _h) = SettingsStore::open(dir.path(), audit, "main").expect("open");
        store
            .apply(
                SettingsOverrides {
                    schedules: Some(vec![Schedule {
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
                        enabled: true,
                    }]),
                    ..Default::default()
                },
                Actor {
                    user_id: "owner".into(),
                    user_login: "owner".into(),
                },
            )
            .await
            .expect("apply");
        let s = store
            .reset(
                SettingsSection::Schedules,
                Actor {
                    user_id: "owner".into(),
                    user_login: "owner".into(),
                },
            )
            .await
            .expect("reset");
        assert!(s.schedules.is_empty());
    }

    #[tokio::test]
    async fn apply_signals_change_notify() {
        let (_dir, store, _handle, _log) = fixture();
        let notify = store.change_notify();
        let mut notified = Box::pin(notify.notified());
        // `enable()` registers the waker before the trigger so the wake-up
        // cannot be lost to a race with notify_waiters().
        notified.as_mut().enable();
        let patch = SettingsOverrides {
            cooldowns: CooldownsOverrides {
                ai: Some(15),
                ..Default::default()
            },
            ..SettingsOverrides::default()
        };
        store
            .apply(
                patch,
                Actor {
                    user_id: "1".into(),
                    user_login: "tester".into(),
                },
            )
            .await
            .expect("apply");
        tokio::time::timeout(std::time::Duration::from_millis(500), notified)
            .await
            .expect("apply must notify change waiters within 500ms");
    }

    #[tokio::test]
    async fn reset_signals_change_notify() {
        let (_dir, store, _handle, _log) = fixture();
        // Seed a non-default cooldowns override so reset has something to clear.
        store
            .apply(
                SettingsOverrides {
                    cooldowns: CooldownsOverrides {
                        ai: Some(15),
                        ..Default::default()
                    },
                    ..SettingsOverrides::default()
                },
                Actor {
                    user_id: "1".into(),
                    user_login: "tester".into(),
                },
            )
            .await
            .expect("apply");
        let notify = store.change_notify();
        let mut notified = Box::pin(notify.notified());
        notified.as_mut().enable();
        store
            .reset(
                SettingsSection::Cooldowns,
                Actor {
                    user_id: "1".into(),
                    user_login: "tester".into(),
                },
            )
            .await
            .expect("reset");
        tokio::time::timeout(std::time::Duration::from_millis(500), notified)
            .await
            .expect("reset must notify change waiters within 500ms");
    }

    #[tokio::test]
    async fn concurrent_apply_with_does_not_lose_updates() {
        use crate::settings::Actor;
        use crate::settings::audit::MemoryAuditLog;
        let dir = tempfile::tempdir().expect("tmp");
        let audit = Arc::new(MemoryAuditLog::default());
        let (store, _h) = SettingsStore::open(dir.path(), audit, "main").expect("open");

        let store_a = store.clone();
        let store_b = store.clone();

        let task_a = tokio::spawn(async move {
            store_a
                .apply_with(
                    |o| {
                        let mut next = o.schedules.clone().unwrap_or_default();
                        next.push(crate::schedule::Schedule {
                            name: "alpha".into(),
                            message: "a".into(),
                            trigger: crate::schedule::Trigger::Interval {
                                every: std::time::Duration::from_secs(3600),
                                days: crate::schedule::WeekdaySet::default(),
                                active_from: None,
                                active_to: None,
                            },
                            start_date: None,
                            end_date: None,
                            enabled: true,
                        });
                        o.schedules = Some(next);
                    },
                    Actor {
                        user_id: "1".into(),
                        user_login: "a".into(),
                    },
                )
                .await
                .expect("apply_with a");
        });
        let task_b = tokio::spawn(async move {
            store_b
                .apply_with(
                    |o| {
                        let mut next = o.schedules.clone().unwrap_or_default();
                        next.push(crate::schedule::Schedule {
                            name: "bravo".into(),
                            message: "b".into(),
                            trigger: crate::schedule::Trigger::Interval {
                                every: std::time::Duration::from_secs(3600),
                                days: crate::schedule::WeekdaySet::default(),
                                active_from: None,
                                active_to: None,
                            },
                            start_date: None,
                            end_date: None,
                            enabled: true,
                        });
                        o.schedules = Some(next);
                    },
                    Actor {
                        user_id: "2".into(),
                        user_login: "b".into(),
                    },
                )
                .await
                .expect("apply_with b");
        });
        let _ = tokio::join!(task_a, task_b);

        let final_resolved = store.handle().load();
        let names: std::collections::BTreeSet<&str> = final_resolved
            .schedules
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            names,
            std::collections::BTreeSet::from(["alpha", "bravo"]),
            "both concurrent adds must survive — neither clobbers the other"
        );
    }

    #[tokio::test]
    async fn v3_settings_file_migrates_to_v4_on_open() {
        // A v3-shaped settings.ron with a legacy schedule row (interval string,
        // no trigger field). SettingsOverrides allows all fields to be absent
        // via #[serde(default)], so only the schedules key is needed.
        let v3 = r#"(
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
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join(FILE_NAME), v3).expect("write");
        let audit = Arc::new(MemoryAuditLog::new());
        let (_, handle) = SettingsStore::open(dir.path(), audit, "main").expect("open v3 file");
        let s = handle.load();
        assert_eq!(s.schedules.len(), 1, "schedule row must survive migration");
        assert_eq!(s.schedules[0].name, "noon");
        match &s.schedules[0].trigger {
            crate::schedule::Trigger::Interval { every, .. } => {
                assert_eq!(every.as_secs(), 3600, "01:00 → 3600s");
            }
            other => panic!("expected Trigger::Interval, got {other:?}"),
        }
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
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use crate::schedule::{Schedule, Trigger, WeekdaySet};
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

    #[tokio::test]
    async fn reset_twitch_permissions_clears_only_perm_fields() {
        let dir = tempfile::tempdir().expect("tmp");
        let audit = std::sync::Arc::new(crate::settings::audit::MemoryAuditLog::default());
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
                Actor {
                    user_id: "owner".into(),
                    user_login: "owner".into(),
                },
            )
            .await
            .expect("apply");
        let s = store
            .reset(
                SettingsSection::TwitchPermissions,
                Actor {
                    user_id: "owner".into(),
                    user_login: "owner".into(),
                },
            )
            .await
            .expect("reset");
        assert!(s.twitch.hidden_admins.is_empty());
        assert_eq!(s.twitch.expected_latency, 250); // perm reset does not touch channels
    }

    #[tokio::test]
    async fn reset_twitch_channels_preserves_expected_latency() {
        let dir = tempfile::tempdir().expect("tmp");
        let audit = std::sync::Arc::new(crate::settings::audit::MemoryAuditLog::default());
        let (store, _h) = SettingsStore::open(dir.path(), audit, "main").expect("open");
        store
            .apply(
                SettingsOverrides {
                    twitch: crate::settings::overrides::TwitchOverrides {
                        expected_latency: Some(300),
                        admin_channel: Some(Some("admins".into())),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Actor {
                    user_id: "owner".into(),
                    user_login: "owner".into(),
                },
            )
            .await
            .expect("apply");
        let s = store
            .reset(
                SettingsSection::TwitchChannels,
                Actor {
                    user_id: "owner".into(),
                    user_login: "owner".into(),
                },
            )
            .await
            .expect("reset");
        // Channels reset clears admin_channel/ai_channel but not expected_latency.
        assert!(s.twitch.admin_channel.is_none());
        assert_eq!(
            s.twitch.expected_latency, 300,
            "TwitchChannels reset must not wipe expected_latency"
        );
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
