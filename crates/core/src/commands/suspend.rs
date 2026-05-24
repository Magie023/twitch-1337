//! Admin commands for transiently suspending other bot commands.
//!
//! `!suspend <command> [duration]` silences another command for the given
//! duration (defaults to [`SuspendConfig::default_duration_secs`]).
//! `!unsuspend <command>` lifts an active suspension.
//!
//! Both commands are gated to broadcaster/moderator badges or user ids listed
//! in `twitch.hidden_admins`. The commands `suspend`, `unsuspend`, and `p`
//! cannot be suspended (enforced in [`SuspendCommand`]).
//!
//! ## Suspension key contract
//!
//! `SuspendCommand` normalizes the user-supplied command name via
//! [`super::normalize_command_name`] (strip leading `!`s, ASCII-lowercase) and
//! stores that string in the [`SuspensionManager`]. The dispatcher looks up
//! suspensions by [`super::Command::suspend_key`], which must produce the
//! same string. The default implementation normalizes the dispatched trigger
//! word, which matches what users type for single-trigger commands. Commands
//! with multiple triggers (e.g. `AiCommand` matches both `!ai` and `@grok`)
//! override `suspend_key` to expose a single canonical key — looking up by
//! raw trigger word would miss the alias.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{ADMIN_DENIED_MSG, Command, CommandContext, is_admin, normalize_command_name};
use crate::cooldown::format_cooldown_remaining;
use crate::settings::SettingsHandle;
use crate::suspend::{ParseDurationError, SuspensionManager, parse_duration};

/// Command names that must never be suspendable. Kept lowercase; the key
/// passed from user input is normalized the same way before comparison.
const EXEMPT_COMMANDS: &[&str] = &["suspend", "unsuspend", "p"];

/// Map a [`ParseDurationError`] to a user-facing German message.
fn duration_error_message(err: &ParseDurationError) -> String {
    match err {
        ParseDurationError::Empty => "Dauer fehlt. Nutze z.B. 30s, 10m, 2h, 1d FDM".to_string(),
        ParseDurationError::InvalidNumber => {
            "Ungültige Zahl. Nutze z.B. 30s, 10m, 2h, 1d FDM".to_string()
        }
        ParseDurationError::UnknownUnit => {
            "Unbekannte Einheit. Erlaubt: s, m, h, d FDM".to_string()
        }
        ParseDurationError::Zero => "Dauer muss größer als 0 sein FDM".to_string(),
        ParseDurationError::TooLong => "Dauer zu lang (max 7 Tage) FDM".to_string(),
    }
}

pub struct SuspendCommand {
    manager: Arc<SuspensionManager>,
    settings: SettingsHandle,
}

impl SuspendCommand {
    pub fn new(manager: Arc<SuspensionManager>, settings: SettingsHandle) -> Self {
        Self { manager, settings }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for SuspendCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!suspend"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let s = self.settings.load();
        let is_admin_user = is_admin(ctx.privmsg, &s.twitch.hidden_admins);
        let default_duration_secs = s.suspend.default_duration_secs;
        drop(s);

        if !is_admin_user {
            ctx.sender.reply(ctx.privmsg, ADMIN_DENIED_MSG).await;
            return Ok(());
        }

        let raw_cmd = match ctx.args.first() {
            Some(c) => *c,
            None => {
                ctx.sender
                    .reply(ctx.privmsg, "Nutze: !suspend <command> [dauer]")
                    .await;
                return Ok(());
            }
        };

        let cmd = normalize_command_name(raw_cmd);

        if EXEMPT_COMMANDS.contains(&cmd.as_str()) {
            ctx.sender
                .reply(ctx.privmsg, "Das kann nicht gesperrt werden FDM")
                .await;
            return Ok(());
        }

        let duration = match ctx.args.get(1) {
            None => Duration::from_secs(default_duration_secs),
            Some(s) => match parse_duration(s) {
                Ok(d) => d,
                Err(err) => {
                    ctx.sender
                        .reply(ctx.privmsg, duration_error_message(&err))
                        .await;
                    return Ok(());
                }
            },
        };

        self.manager.suspend(&cmd, duration).await;

        ctx.sender
            .reply(
                ctx.privmsg,
                format!(
                    "!{cmd} gesperrt für {}",
                    format_cooldown_remaining(duration)
                ),
            )
            .await;

        Ok(())
    }
}

pub struct UnsuspendCommand {
    manager: Arc<SuspensionManager>,
    settings: SettingsHandle,
}

impl UnsuspendCommand {
    pub fn new(manager: Arc<SuspensionManager>, settings: SettingsHandle) -> Self {
        Self { manager, settings }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for UnsuspendCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!unsuspend"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let s = self.settings.load();
        if !is_admin(ctx.privmsg, &s.twitch.hidden_admins) {
            ctx.sender.reply(ctx.privmsg, ADMIN_DENIED_MSG).await;
            return Ok(());
        }
        drop(s);

        let raw_cmd = match ctx.args.first() {
            Some(c) => *c,
            None => {
                ctx.sender
                    .reply(ctx.privmsg, "Nutze: !unsuspend <command>")
                    .await;
                return Ok(());
            }
        };

        let cmd = normalize_command_name(raw_cmd);

        let reply = if self.manager.unsuspend(&cmd).await {
            format!("!{cmd} entsperrt Okayge")
        } else {
            format!("!{cmd} war nicht gesperrt FDM")
        };

        ctx.sender.reply(ctx.privmsg, reply).await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exempt_list_covers_required_commands() {
        assert!(EXEMPT_COMMANDS.contains(&"suspend"));
        assert!(EXEMPT_COMMANDS.contains(&"unsuspend"));
        assert!(EXEMPT_COMMANDS.contains(&"p"));
    }

    #[test]
    fn duration_error_messages_end_in_fdm() {
        for err in [
            ParseDurationError::Empty,
            ParseDurationError::InvalidNumber,
            ParseDurationError::UnknownUnit,
            ParseDurationError::Zero,
            ParseDurationError::TooLong,
        ] {
            let msg = duration_error_message(&err);
            assert!(msg.ends_with("FDM"), "expected FDM suffix, got: {msg}");
        }
    }
}
