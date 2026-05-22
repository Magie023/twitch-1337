use std::sync::Arc;

use async_trait::async_trait;
use eyre::{Result, WrapErr};
use tracing::{instrument, warn};
use twitch_irc::{login::LoginCredentials, message::PrivmsgMessage, transport::Transport};

use crate::commands::{Command, CommandContext};
use crate::twitch::ChatSender;
use crate::util::parse_flight_duration;

pub struct RandomFlightCommand;

#[async_trait]
impl<T, L> Command<T, L> for RandomFlightCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!fl"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        flight_command(
            ctx.privmsg,
            ctx.sender,
            ctx.args.first().copied(),
            ctx.args.get(1).copied(),
        )
        .await
    }
}

#[instrument(skip(privmsg, sender), fields(user = %privmsg.sender.login))]
pub(crate) async fn flight_command<T, L>(
    privmsg: &PrivmsgMessage,
    sender: &Arc<ChatSender<T, L>>,
    aircraft_code: Option<&str>,
    duration_str: Option<&str>,
) -> Result<()>
where
    T: Transport,
    L: LoginCredentials,
{
    const USAGE_MSG: &str = "Gib mir nen Flugzeug und ne Zeit, z.B. !fl A20N 1h FDM";

    let (Some(aircraft_code), Some(duration_str)) = (aircraft_code, duration_str) else {
        sender.reply(privmsg, USAGE_MSG).await;
        return Ok(());
    };

    let Some(aircraft) = random_flight::aircraft_by_icao_type(aircraft_code) else {
        sender
            .reply(privmsg, "Das Flugzeug kenn ich nich FDM")
            .await;
        return Ok(());
    };

    let Some(duration) = parse_flight_duration(duration_str) else {
        sender.reply(privmsg, USAGE_MSG).await;
        return Ok(());
    };

    // Can take many retries internally
    let result = tokio::task::spawn_blocking(move || {
        random_flight::generate_flight_plan(aircraft, duration, None)
    })
    .await
    .wrap_err("Flight plan generation task panicked")?;

    let fp = match result {
        Ok(fp) => fp,
        Err(e) => {
            warn!(error = ?e, "Flight plan generation failed");
            sender
                .reply(
                    privmsg,
                    "Hab keine Route gefunden, versuch mal ne andere Zeit FDM",
                )
                .await;
            return Ok(());
        }
    };

    let time_str = crate::cooldown::format_duration_hm(fp.block_time);

    let response = format!(
        "{} → {} | {:.0} nm | {} | FL{} | {}",
        fp.departure.icao,
        fp.arrival.icao,
        fp.distance_nm,
        time_str,
        fp.cruise_altitude_ft / 100,
        fp.simbrief_url(),
    );

    sender.reply(privmsg, response).await;

    Ok(())
}
