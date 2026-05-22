use async_trait::async_trait;
use eyre::Result;
use tokio::sync::mpsc;
use tracing::error;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::aviation::{FlightIdentifier, TrackerCommand};
use crate::commands::{Command, CommandContext};

pub struct TrackCommand {
    tracker_tx: mpsc::Sender<TrackerCommand>,
}

impl TrackCommand {
    pub fn new(tracker_tx: mpsc::Sender<TrackerCommand>) -> Self {
        Self { tracker_tx }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for TrackCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!track"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let input = ctx.args.join(" ");
        if input.trim().is_empty() {
            ctx.sender
                .reply(ctx.privmsg, "Benutzung: !track <callsign/hex> FDM")
                .await;
            return Ok(());
        }

        let identifier = match FlightIdentifier::parse(&input) {
            Ok(id) => id,
            Err(e) => {
                ctx.sender.reply(ctx.privmsg, format!("{e} FDM")).await;
                return Ok(());
            }
        };
        let cmd = TrackerCommand::Track {
            identifier,
            requested_by: ctx.privmsg.sender.login.clone(),
            reply_to: ctx.privmsg.clone(),
        };

        if let Err(e) = self.tracker_tx.send(cmd).await {
            error!(error = ?e, "Failed to send track command to flight tracker");
        }

        Ok(())
    }
}
