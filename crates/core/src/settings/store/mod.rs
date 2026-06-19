//! Owner of `$DATA_DIR/settings.ron`. Serializes writes, validates, swaps
//! the shared `SettingsHandle`, and appends an audit log entry per apply.
//!
//! The override patch-merge lives in `merge.rs`; the load/migrate/quarantine
//! family lives in `load.rs`; the semantic audit-diff lives in the `audit`
//! submodule alongside the `AuditChange` types it produces.

mod load;
mod merge;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::Utc;
use tokio::sync::{Mutex, Notify};
use tracing::{error, info, warn};

use super::audit::{AuditEntry, AuditLog, berlin_now, diff_changes};
use super::overrides::SettingsOverrides;
use super::{Settings, SettingsError, SettingsHandle, SettingsSection};
use load::{load_or_quarantine, load_overrides_async, quarantine};
use merge::merge_into;

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
}
