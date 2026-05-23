use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use eyre::{Result, WrapErr, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info};

const PINGS_FILENAME: &str = "pings.ron";
const PING_ACTOR_CHANNEL: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ping {
    pub template: String,
    pub members: HashSet<String>,
    pub cooldown: Option<u64>,
    pub created_by: String,
    #[serde(default)]
    pub last_fired_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub fire_count: u64,
}

#[derive(Debug, Clone)]
pub struct PingView {
    pub name: String,
    pub template: String,
    pub members: HashSet<String>,
    pub cooldown: Option<u64>,
    pub created_by: String,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub fire_count: u64,
}

impl PingView {
    fn from_name_ping(name: &str, p: &Ping) -> Self {
        Self {
            name: name.to_owned(),
            template: p.template.clone(),
            members: p.members.clone(),
            cooldown: p.cooldown,
            created_by: p.created_by.clone(),
            last_fired_at: p.last_fired_at,
            fire_count: p.fire_count,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingStore {
    pub pings: HashMap<String, Ping>,
}

#[derive(Debug)]
pub enum TriggerDecision {
    Skip,
    OnCooldown(Duration),
    Fire(String),
}

pub enum PingCommand {
    CreatePing {
        name: String,
        template: String,
        created_by: String,
        cooldown: Option<u64>,
        reply: oneshot::Sender<Result<()>>,
    },
    DeletePing {
        name: String,
        reply: oneshot::Sender<Result<()>>,
    },
    EditTemplate {
        name: String,
        template: String,
        reply: oneshot::Sender<Result<()>>,
    },
    AddMember {
        ping_name: String,
        username: String,
        reply: oneshot::Sender<Result<()>>,
    },
    RemoveMember {
        ping_name: String,
        username: String,
        reply: oneshot::Sender<Result<()>>,
    },
    TryRecordTrigger {
        ping_name: String,
        sender: String,
        default_cooldown: Duration,
        public: bool,
        reply: oneshot::Sender<TriggerDecision>,
    },
    Snapshot {
        reply: oneshot::Sender<Vec<PingView>>,
    },
    GetOne {
        name: String,
        reply: oneshot::Sender<Option<PingView>>,
    },
    ListForUser {
        username: String,
        reply: oneshot::Sender<Vec<String>>,
    },
    IsMember {
        ping_name: String,
        username: String,
        reply: oneshot::Sender<bool>,
    },
}

#[derive(Clone)]
pub struct PingHandle(Arc<mpsc::Sender<PingCommand>>);

impl PingHandle {
    pub fn new(tx: mpsc::Sender<PingCommand>) -> Self {
        Self(Arc::new(tx))
    }

    async fn send<T>(&self, make_cmd: impl FnOnce(oneshot::Sender<T>) -> PingCommand) -> T {
        let (tx, rx) = oneshot::channel();
        let _ = self.0.send(make_cmd(tx)).await;
        rx.await.expect("ping actor dropped")
    }

    pub async fn create_ping(
        &self,
        name: String,
        template: String,
        created_by: String,
        cooldown: Option<u64>,
    ) -> Result<()> {
        self.send(|reply| PingCommand::CreatePing {
            name,
            template,
            created_by,
            cooldown,
            reply,
        })
        .await
    }
    pub async fn delete_ping(&self, name: String) -> Result<()> {
        self.send(|reply| PingCommand::DeletePing { name, reply })
            .await
    }
    pub async fn edit_template(&self, name: String, template: String) -> Result<()> {
        self.send(|reply| PingCommand::EditTemplate {
            name,
            template,
            reply,
        })
        .await
    }
    pub async fn add_member(&self, ping_name: String, username: String) -> Result<()> {
        self.send(|reply| PingCommand::AddMember {
            ping_name,
            username,
            reply,
        })
        .await
    }
    pub async fn remove_member(&self, ping_name: String, username: String) -> Result<()> {
        self.send(|reply| PingCommand::RemoveMember {
            ping_name,
            username,
            reply,
        })
        .await
    }
    pub async fn try_record_trigger(
        &self,
        ping_name: String,
        sender: String,
        default_cooldown: Duration,
        public: bool,
    ) -> TriggerDecision {
        self.send(|reply| PingCommand::TryRecordTrigger {
            ping_name,
            sender,
            default_cooldown,
            public,
            reply,
        })
        .await
    }
    pub async fn snapshot(&self) -> Vec<PingView> {
        self.send(|reply| PingCommand::Snapshot { reply }).await
    }
    pub async fn get_one(&self, name: String) -> Option<PingView> {
        self.send(|reply| PingCommand::GetOne { name, reply }).await
    }
    pub async fn list_for_user(&self, username: String) -> Vec<String> {
        self.send(|reply| PingCommand::ListForUser { username, reply })
            .await
    }
    pub async fn is_member(&self, ping_name: String, username: String) -> bool {
        self.send(|reply| PingCommand::IsMember {
            ping_name,
            username,
            reply,
        })
        .await
    }
}

pub type PingActorChannel = (
    Arc<mpsc::Sender<PingCommand>>,
    mpsc::Receiver<PingCommand>,
    watch::Sender<HashSet<String>>,
    watch::Receiver<HashSet<String>>,
);

pub fn ping_actor_channel_full() -> PingActorChannel {
    let (cmd_tx, cmd_rx) = mpsc::channel(PING_ACTOR_CHANNEL);
    let (names_tx, names_rx) = watch::channel(HashSet::new());
    (Arc::new(cmd_tx), cmd_rx, names_tx, names_rx)
}

pub async fn run_ping_actor(
    mut cmd_rx: mpsc::Receiver<PingCommand>,
    mut manager: PingManager,
    names_tx: watch::Sender<HashSet<String>>,
) {
    publish_names(&manager, &names_tx);
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            PingCommand::CreatePing {
                name,
                template,
                created_by,
                cooldown,
                reply,
            } => {
                let result = manager
                    .create_ping(name, template, created_by, cooldown)
                    .await;
                let _ = reply.send(result);
                publish_names(&manager, &names_tx);
            }
            PingCommand::DeletePing { name, reply } => {
                let result = manager.delete_ping(&name).await;
                let _ = reply.send(result);
                publish_names(&manager, &names_tx);
            }
            PingCommand::EditTemplate {
                name,
                template,
                reply,
            } => {
                let _ = reply.send(manager.edit_template(&name, template).await);
            }
            PingCommand::AddMember {
                ping_name,
                username,
                reply,
            } => {
                let _ = reply.send(manager.add_member(&ping_name, &username).await);
            }
            PingCommand::RemoveMember {
                ping_name,
                username,
                reply,
            } => {
                let _ = reply.send(manager.remove_member(&ping_name, &username).await);
            }
            PingCommand::TryRecordTrigger {
                ping_name,
                sender,
                default_cooldown,
                public,
                reply,
            } => {
                let decision = manager
                    .try_record_trigger(&ping_name, &sender, default_cooldown, public)
                    .await;
                let _ = reply.send(decision);
            }
            PingCommand::Snapshot { reply } => {
                let views = manager
                    .store
                    .pings
                    .iter()
                    .map(|(n, p)| PingView::from_name_ping(n, p))
                    .collect();
                let _ = reply.send(views);
            }
            PingCommand::GetOne { name, reply } => {
                let view = manager
                    .store
                    .pings
                    .get(&name)
                    .map(|p| PingView::from_name_ping(&name, p));
                let _ = reply.send(view);
            }
            PingCommand::ListForUser { username, reply } => {
                let _ = reply.send(manager.list_pings_for_user_owned(&username));
            }
            PingCommand::IsMember {
                ping_name,
                username,
                reply,
            } => {
                let _ = reply.send(manager.is_member(&ping_name, &username));
            }
        }
    }
}

fn publish_names(manager: &PingManager, tx: &watch::Sender<HashSet<String>>) {
    let names: HashSet<String> = manager.store.pings.keys().cloned().collect();
    let _ = tx.send(names);
}

fn validate_template(template: &str) -> Result<()> {
    if template.chars().any(char::is_control) {
        bail!("Template darf keine Steuerzeichen (z.B. Zeilenumbrüche) enthalten");
    }
    Ok(())
}

pub struct PingManager {
    store: PingStore,
    last_triggered: HashMap<String, Instant>,
    path: PathBuf,
}

impl PingManager {
    #[cfg(any(test, feature = "testing"))]
    pub fn empty() -> Self {
        Self {
            store: PingStore {
                pings: HashMap::new(),
            },
            last_triggered: HashMap::new(),
            path: PathBuf::from(PINGS_FILENAME),
        }
    }

    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(PINGS_FILENAME);
        let mut store: PingStore = if path.exists() {
            let data = std::fs::read_to_string(&path).wrap_err("Failed to read pings.ron")?;
            ron::from_str(&data).wrap_err("Failed to parse pings.ron")?
        } else {
            info!("No pings.ron found, starting with empty ping store");
            PingStore {
                pings: HashMap::new(),
            }
        };
        store.pings.retain(|name, ping| match validate_template(&ping.template) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(ping = %name, error = %e, "Dropping ping with invalid template on load");
                false
            }
        });
        info!(count = store.pings.len(), "Loaded pings");
        Ok(Self {
            store,
            last_triggered: HashMap::new(),
            path,
        })
    }

    async fn save(&self) -> Result<()> {
        crate::util::persist::atomic_save_ron_async(&self.store, &self.path)
            .await
            .wrap_err("Failed to save pings.ron")?;
        debug!("Saved pings to disk");
        Ok(())
    }

    pub async fn create_ping(
        &mut self,
        name: String,
        template: String,
        created_by: String,
        cooldown: Option<u64>,
    ) -> Result<()> {
        let name = name.to_ascii_lowercase();
        if name.is_empty() {
            bail!("Ping-Name darf nicht leer sein");
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("Ping-Name darf nur Buchstaben, Zahlen, - und _ enthalten");
        }
        validate_template(&template)?;
        if self.store.pings.contains_key(&name) {
            bail!("Ping \"{}\" gibt es schon", name);
        }
        self.store.pings.insert(
            name,
            Ping {
                template,
                members: HashSet::new(),
                cooldown,
                created_by,
                last_fired_at: None,
                fire_count: 0,
            },
        );
        self.save().await
    }

    pub async fn delete_ping(&mut self, name: &str) -> Result<()> {
        if self.store.pings.remove(name).is_none() {
            bail!("Ping \"{}\" gibt es nicht", name);
        }
        self.last_triggered.remove(name);
        self.save().await
    }

    pub async fn edit_template(&mut self, name: &str, template: String) -> Result<()> {
        validate_template(&template)?;
        let ping = self
            .store
            .pings
            .get_mut(name)
            .ok_or_else(|| eyre::eyre!("Ping \"{}\" gibt es nicht", name))?;
        ping.template = template;
        self.save().await
    }

    pub async fn add_member(&mut self, ping_name: &str, username: &str) -> Result<()> {
        let ping = self
            .store
            .pings
            .get_mut(ping_name)
            .ok_or_else(|| eyre::eyre!("Ping \"{}\" gibt es nicht", ping_name))?;
        let username_lower = username.to_lowercase();
        if !ping.members.insert(username_lower) {
            bail!("{} ist schon in \"{}\"", username, ping_name);
        }
        self.save().await
    }

    pub async fn remove_member(&mut self, ping_name: &str, username: &str) -> Result<()> {
        let ping = self
            .store
            .pings
            .get_mut(ping_name)
            .ok_or_else(|| eyre::eyre!("Ping \"{}\" gibt es nicht", ping_name))?;
        let username_lower = username.to_lowercase();
        if !ping.members.remove(&username_lower) {
            bail!("{} ist nicht in \"{}\"", username, ping_name);
        }
        self.save().await
    }

    pub fn get(&self, name: &str) -> Option<&Ping> {
        self.store.pings.get(name)
    }

    pub fn is_member(&self, ping_name: &str, username: &str) -> bool {
        self.store
            .pings
            .get(ping_name)
            .map(|p| p.members.contains(username))
            .unwrap_or(false)
    }

    pub fn list_pings_for_user_owned(&self, username: &str) -> Vec<String> {
        self.store
            .pings
            .iter()
            .filter(|(_, p)| p.members.contains(username))
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn remaining_cooldown(
        &self,
        ping_name: &str,
        default_cooldown: Duration,
    ) -> Option<Duration> {
        let ping = self.store.pings.get(ping_name)?;
        let cooldown = ping.cooldown.map_or(default_cooldown, Duration::from_secs);
        match self.last_triggered.get(ping_name) {
            Some(last) => {
                let elapsed = last.elapsed();
                if elapsed < cooldown {
                    Some(cooldown - elapsed)
                } else {
                    None
                }
            }
            None => None,
        }
    }

    pub async fn record_trigger(&mut self, ping_name: &str) -> Result<()> {
        let Some(ping) = self.store.pings.get_mut(ping_name) else {
            return Ok(());
        };
        self.last_triggered
            .insert(ping_name.to_string(), Instant::now());
        ping.last_fired_at = Some(Utc::now());
        ping.fire_count = ping.fire_count.saturating_add(1);
        self.save().await
    }

    pub async fn try_record_trigger(
        &mut self,
        ping_name: &str,
        sender: &str,
        default_cooldown: Duration,
        public: bool,
    ) -> TriggerDecision {
        if !public && !self.is_member(ping_name, sender) {
            return TriggerDecision::Skip;
        }
        if let Some(remaining) = self.remaining_cooldown(ping_name, default_cooldown) {
            return TriggerDecision::OnCooldown(remaining);
        }
        let Some(rendered) = self.render_template(ping_name, sender) else {
            return TriggerDecision::Skip;
        };
        if let Err(e) = self.record_trigger(ping_name).await {
            tracing::warn!(ping = %ping_name, error = ?e, "Failed to persist fire stats");
        }
        TriggerDecision::Fire(rendered)
    }

    pub fn render_template(&self, ping_name: &str, sender: &str) -> Option<String> {
        let ping = self.store.pings.get(ping_name)?;
        let sender_in_template = ping.template.contains("{sender}");
        let mentions = ping
            .members
            .iter()
            .filter(|m| !sender_in_template || m.as_str() != sender)
            .map(|m| format!("@{m}"))
            .collect::<Vec<_>>()
            .join(" ");
        if mentions.is_empty() {
            return None;
        }
        let rendered = ping
            .template
            .replace("{mentions}", &mentions)
            .replace("{sender}", &format!("@{sender}"));
        Some(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_manager(dir: &Path) -> PingManager {
        PingManager {
            store: PingStore {
                pings: HashMap::new(),
            },
            last_triggered: HashMap::new(),
            path: dir.join(PINGS_FILENAME),
        }
    }

    async fn test_manager(dir: &Path) -> PingManager {
        let mut mgr = empty_manager(dir);
        mgr.create_ping(
            "test".into(),
            "Hey {mentions}!".into(),
            "admin".into(),
            None,
        )
        .await
        .unwrap();
        mgr
    }

    #[tokio::test]
    async fn edit_template_updates_template() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        mgr.edit_template("test", "New template {mentions}".into())
            .await
            .unwrap();
        assert_eq!(
            mgr.store.pings.get("test").unwrap().template,
            "New template {mentions}"
        );
    }

    #[tokio::test]
    async fn edit_template_preserves_members() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        mgr.add_member("test", "alice").await.unwrap();
        mgr.add_member("test", "bob").await.unwrap();
        mgr.edit_template("test", "Updated {mentions}".into())
            .await
            .unwrap();
        let ping = mgr.store.pings.get("test").unwrap();
        assert!(ping.members.contains("alice"));
        assert!(ping.members.contains("bob"));
        assert_eq!(ping.members.len(), 2);
    }

    #[tokio::test]
    async fn edit_template_nonexistent_ping_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        let result = mgr.edit_template("nope", "whatever".into()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("gibt es nicht"));
    }

    #[tokio::test]
    async fn create_ping_rejects_newline_in_template() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = empty_manager(dir.path());
        let result = mgr
            .create_ping(
                "bad".into(),
                "Hey {mentions}\r\nPRIVMSG #other :pwned".into(),
                "admin".into(),
                None,
            )
            .await;
        assert!(result.is_err());
        assert!(!mgr.store.pings.contains_key("bad"));
    }

    #[tokio::test]
    async fn edit_template_rejects_control_chars() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        let result = mgr.edit_template("test", "Hey\x00injection".into()).await;
        assert!(result.is_err());
        assert_eq!(
            mgr.store.pings.get("test").unwrap().template,
            "Hey {mentions}!"
        );
    }

    #[test]
    fn load_drops_pings_with_invalid_template_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PINGS_FILENAME);
        let raw = "(pings: {\n  \"good\": (template: \"Hey {mentions}!\", members: [], cooldown: None, created_by: \"admin\"),\n  \"bad\": (template: \"Hey {mentions}\\r\\nPRIVMSG #other :pwned\", members: [], cooldown: None, created_by: \"admin\"),\n})\n";
        std::fs::write(&path, raw).unwrap();
        let mgr = PingManager::load(dir.path()).unwrap();
        assert!(mgr.store.pings.contains_key("good"));
        assert!(
            !mgr.store.pings.contains_key("bad"),
            "ping with control chars must be dropped on load"
        );
    }

    #[tokio::test]
    async fn edit_template_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        mgr.edit_template("test", "Persisted {mentions}".into())
            .await
            .unwrap();
        let mgr2 = PingManager::load(dir.path()).unwrap();
        assert_eq!(
            mgr2.store.pings.get("test").unwrap().template,
            "Persisted {mentions}"
        );
    }

    #[tokio::test]
    async fn remaining_cooldown_returns_none_when_never_triggered() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        mgr.add_member("test", "alice").await.unwrap();
        assert!(
            mgr.remaining_cooldown("test", Duration::from_secs(300))
                .is_none()
        );
    }

    #[tokio::test]
    async fn remaining_cooldown_returns_some_when_on_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        mgr.add_member("test", "alice").await.unwrap();
        mgr.record_trigger("test").await.unwrap();
        let remaining = mgr.remaining_cooldown("test", Duration::from_secs(300));
        assert!(remaining.is_some());
        let secs = remaining.unwrap().as_secs();
        assert!(secs > 0 && secs <= 300, "expected 1..=300, got {secs}");
    }

    #[tokio::test]
    async fn remaining_cooldown_returns_none_for_nonexistent_ping() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = empty_manager(dir.path());
        assert!(
            mgr.remaining_cooldown("nope", Duration::from_secs(300))
                .is_none()
        );
    }

    #[tokio::test]
    async fn try_record_trigger_is_atomic_across_consecutive_calls() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        mgr.add_member("test", "alice").await.unwrap();
        mgr.add_member("test", "bob").await.unwrap();
        let first = mgr
            .try_record_trigger("test", "bob", Duration::from_secs(300), false)
            .await;
        match first {
            TriggerDecision::Fire(rendered) => {
                assert!(rendered.contains("@alice"));
                assert!(rendered.contains("@bob"));
            }
            other => panic!("expected Fire on first call, got {other:?}"),
        }
        let second = mgr
            .try_record_trigger("test", "bob", Duration::from_secs(300), false)
            .await;
        match second {
            TriggerDecision::OnCooldown(remaining) => {
                assert!(remaining.as_secs() <= 300);
            }
            other => panic!("expected OnCooldown on second call, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn try_record_trigger_respects_membership() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        mgr.add_member("test", "alice").await.unwrap();
        let decision = mgr
            .try_record_trigger("test", "stranger", Duration::from_secs(300), false)
            .await;
        assert!(matches!(decision, TriggerDecision::Skip));
        let decision = mgr
            .try_record_trigger("test", "alice", Duration::from_secs(300), false)
            .await;
        assert!(
            matches!(decision, TriggerDecision::Fire(_)),
            "sole member with no {{sender}} in template should fire, got {decision:?}"
        );
    }

    #[tokio::test]
    async fn try_record_trigger_public_allows_non_members() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = test_manager(dir.path()).await;
        mgr.add_member("test", "alice").await.unwrap();
        let decision = mgr
            .try_record_trigger("test", "stranger", Duration::from_secs(300), true)
            .await;
        assert!(
            matches!(decision, TriggerDecision::Fire(_)),
            "public=true should allow non-members to fire, got {decision:?}"
        );
    }

    #[tokio::test]
    async fn render_template_includes_sender_when_no_sender_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = empty_manager(dir.path());
        mgr.create_ping("grp".into(), "{mentions}".into(), "admin".into(), None)
            .await
            .unwrap();
        mgr.add_member("grp", "alice").await.unwrap();
        mgr.add_member("grp", "bob").await.unwrap();
        let result = mgr.render_template("grp", "alice").unwrap();
        assert!(result.contains("@alice"));
        assert!(result.contains("@bob"));
    }

    #[tokio::test]
    async fn render_template_excludes_sender_when_template_has_sender_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = empty_manager(dir.path());
        mgr.create_ping(
            "grp".into(),
            "{sender} pinged {mentions}".into(),
            "admin".into(),
            None,
        )
        .await
        .unwrap();
        mgr.add_member("grp", "alice").await.unwrap();
        mgr.add_member("grp", "bob").await.unwrap();
        let result = mgr.render_template("grp", "alice").unwrap();
        assert!(!result.contains("@alice @") && result.starts_with("@alice pinged"));
        assert!(result.contains("@bob"));
    }

    #[tokio::test]
    async fn render_template_fires_when_sender_is_sole_member_and_no_sender_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = empty_manager(dir.path());
        mgr.create_ping("grp".into(), "{mentions}".into(), "admin".into(), None)
            .await
            .unwrap();
        mgr.add_member("grp", "alice").await.unwrap();
        let result = mgr.render_template("grp", "alice");
        assert!(result.is_some());
        assert!(result.unwrap().contains("@alice"));
    }

    #[tokio::test]
    async fn render_template_skips_when_sender_is_sole_member_and_template_has_sender_placeholder()
    {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = empty_manager(dir.path());
        mgr.create_ping(
            "grp".into(),
            "{sender} pinged {mentions}".into(),
            "admin".into(),
            None,
        )
        .await
        .unwrap();
        mgr.add_member("grp", "alice").await.unwrap();
        let result = mgr.render_template("grp", "alice");
        assert!(result.is_none());
    }
}
