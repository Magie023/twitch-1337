use std::path::PathBuf;

use async_trait::async_trait;
use eyre::Result;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::time::Duration;
use tracing::{error, info};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext};
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::settings::{Settings, SettingsHandle};

const FEEDBACK_FILENAME: &str = "feedback.txt";

fn feedback_cooldown_duration(s: &Settings) -> Duration {
    Duration::from_secs(s.cooldowns.feedback)
}

pub struct FeedbackCommand {
    data_dir: PathBuf,
    cooldown: PerUserCooldown,
}

impl FeedbackCommand {
    pub fn new(data_dir: PathBuf, settings: SettingsHandle) -> Self {
        Self {
            data_dir,
            cooldown: PerUserCooldown::live(settings, feedback_cooldown_duration),
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for FeedbackCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!fb"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;
        let message: String = ctx.args.join(" ");

        // Check for empty message
        if message.trim().is_empty() {
            ctx.sender
                .reply(ctx.privmsg, "Benutzung: !fb <nachricht>")
                .await;
            return Ok(());
        }

        // Check cooldown
        if let Some(remaining) = self.cooldown.check(user).await {
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

        self.cooldown.record(user).await;

        // Write feedback to file
        let now = chrono::Utc::now()
            .with_timezone(&chrono_tz::Europe::Berlin)
            .format("%Y-%m-%dT%H:%M:%S");
        let line = format!("{now} {user}: {message}\n");

        let path = self.data_dir.join(FEEDBACK_FILENAME);
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(line.as_bytes()).await {
                    error!(error = ?e, "Failed to write feedback to file");
                    ctx.sender
                        .reply(ctx.privmsg, "Da ist was schiefgelaufen FDM")
                        .await;
                    return Ok(());
                }
            }
            Err(e) => {
                error!(error = ?e, "Failed to open feedback file");
                ctx.sender
                    .reply(ctx.privmsg, "Da ist was schiefgelaufen FDM")
                    .await;
                return Ok(());
            }
        }

        info!(user = %user, "Feedback saved");

        ctx.sender
            .reply(ctx.privmsg, "Feedback gespeichert Okayge")
            .await;

        Ok(())
    }
}
