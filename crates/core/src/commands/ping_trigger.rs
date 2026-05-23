use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::watch;
use tracing::debug;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext};
use crate::cooldown::format_cooldown_remaining;
use crate::ping::{PingHandle, TriggerDecision};

pub struct PingTriggerCommand {
    ping: PingHandle,
    settings: crate::settings::SettingsHandle,
    names: watch::Receiver<HashSet<String>>,
}

impl PingTriggerCommand {
    pub fn new(
        ping: PingHandle,
        settings: crate::settings::SettingsHandle,
        names: watch::Receiver<HashSet<String>>,
    ) -> Self {
        Self {
            ping,
            settings,
            names,
        }
    }

    fn current_cooldown(&self) -> Duration {
        Duration::from_secs(self.settings.load().pings.cooldown)
    }
    fn current_public(&self) -> bool {
        self.settings.load().pings.public
    }
}

fn parse_ping_trigger(word: &str) -> Option<String> {
    let name = word.strip_prefix('!')?;
    if name.is_empty() {
        return None;
    }
    Some(name.to_lowercase())
}

#[async_trait]
impl<T, L> Command<T, L> for PingTriggerCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!<ping>"
    }

    fn matches(&self, word: &str) -> bool {
        let Some(name) = word.strip_prefix('!') else {
            return false;
        };
        if name.is_empty() {
            return false;
        }
        let names = self.names.borrow();
        names.iter().any(|k| k.eq_ignore_ascii_case(name))
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let Some(ping_name) = parse_ping_trigger(ctx.trigger) else {
            return Ok(());
        };
        let invoker = ctx.privmsg.sender.login.clone();
        let decision = self
            .ping
            .try_record_trigger(
                ping_name.clone(),
                invoker,
                self.current_cooldown(),
                self.current_public(),
            )
            .await;
        let rendered = match decision {
            TriggerDecision::Skip => return Ok(()),
            TriggerDecision::OnCooldown(remaining) => {
                debug!(ping = %ping_name, "Ping on cooldown");
                ctx.sender
                    .reply(
                        ctx.privmsg,
                        format!(
                            "Bitte warte noch {} Waiting",
                            format_cooldown_remaining(remaining)
                        ),
                    )
                    .await;
                return Ok(());
            }
            TriggerDecision::Fire(rendered) => rendered,
        };
        ctx.sender
            .say(ctx.privmsg.channel_login.clone(), rendered)
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod settings_live_tests {
    use super::*;
    use crate::ping::{PingManager, ping_actor_channel_full, run_ping_actor};
    use crate::settings::Settings;
    use std::sync::Arc;

    #[tokio::test]
    async fn reads_cooldown_and_public_from_handle_at_call_time() {
        let initial = Settings::compiled_defaults();
        let handle: crate::settings::SettingsHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(initial));
        let (tx, rx, names_tx, names_rx) = ping_actor_channel_full();
        let mgr = PingManager::empty();
        tokio::spawn(run_ping_actor(rx, mgr, names_tx));
        let ping = crate::ping::PingHandle::new((*tx).clone());
        let cmd = PingTriggerCommand::new(ping, handle.clone(), names_rx);
        let before_cooldown = cmd.current_cooldown();
        let before_public = cmd.current_public();
        let mut next = Settings::compiled_defaults();
        next.pings.cooldown = 7;
        next.pings.public = true;
        handle.store(Arc::new(next));
        let after_cooldown = cmd.current_cooldown();
        let after_public = cmd.current_public();
        assert_ne!(before_cooldown, after_cooldown);
        assert_eq!(after_cooldown, std::time::Duration::from_secs(7));
        assert!(!before_public);
        assert!(after_public);
    }
}
