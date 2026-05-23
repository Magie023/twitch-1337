use async_trait::async_trait;
use eyre::Result;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use crate::ping::PingHandle;

use super::{ADMIN_DENIED_MSG, Command, CommandContext, is_admin, normalize_username};

fn normalize_ping_name(name: &str) -> String {
    name.to_lowercase()
}

pub struct PingAdminCommand {
    ping: PingHandle,
    hidden_admin_ids: Vec<String>,
}

impl PingAdminCommand {
    pub fn new(ping: PingHandle, hidden_admin_ids: Vec<String>) -> Self {
        Self {
            ping,
            hidden_admin_ids,
        }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for PingAdminCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!p"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let subcommand = ctx.args.first().copied().unwrap_or("");
        match subcommand {
            "create" | "delete" | "edit" | "add" | "remove" => {
                if !is_admin(ctx.privmsg, &self.hidden_admin_ids) {
                    ctx.sender.reply(ctx.privmsg, ADMIN_DENIED_MSG).await;
                    return Ok(());
                }
                match subcommand {
                    "create" => self.handle_create(&ctx).await,
                    "delete" => self.handle_delete(&ctx).await,
                    "edit" => self.handle_edit(&ctx).await,
                    "add" => self.handle_member_op(&ctx, "add").await,
                    "remove" => self.handle_member_op(&ctx, "remove").await,
                    _ => unreachable!(),
                }
            }
            "join" => self.handle_self_op(&ctx, "join").await,
            "leave" => self.handle_self_op(&ctx, "leave").await,
            "list" => self.handle_list(&ctx).await,
            _ => {
                ctx.sender
                    .reply(
                        ctx.privmsg,
                        "Nutze: join, leave, list (oder create, delete, edit, add, remove als Mod)",
                    )
                    .await;
                Ok(())
            }
        }
    }
}

impl PingAdminCommand {
    async fn handle_create<T, L>(&self, ctx: &CommandContext<'_, T, L>) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        if ctx.args.len() < 3 {
            ctx.sender
                .reply(ctx.privmsg, "Nutze: !p create <name> <template>")
                .await;
            return Ok(());
        }
        let name = normalize_ping_name(ctx.args[1]);
        let template = ctx.args[2..].join(" ");
        match self
            .ping
            .create_ping(
                name.clone(),
                template,
                ctx.privmsg.sender.login.clone(),
                None,
            )
            .await
        {
            Ok(()) => {
                ctx.sender
                    .reply(ctx.privmsg, format!("Ping \"{name}\" erstellt Okayge"))
                    .await;
            }
            Err(e) => {
                ctx.sender.reply(ctx.privmsg, format!("{e} FDM")).await;
            }
        }
        Ok(())
    }

    async fn handle_delete<T, L>(&self, ctx: &CommandContext<'_, T, L>) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        let name = match ctx.args.get(1) {
            Some(n) => normalize_ping_name(n),
            None => {
                ctx.sender
                    .reply(ctx.privmsg, "Nutze: !p delete <name>")
                    .await;
                return Ok(());
            }
        };
        match self.ping.delete_ping(name.clone()).await {
            Ok(()) => {
                ctx.sender
                    .reply(ctx.privmsg, format!("Ping \"{name}\" gelöscht Okayge"))
                    .await;
            }
            Err(e) => {
                ctx.sender.reply(ctx.privmsg, format!("{e} FDM")).await;
            }
        }
        Ok(())
    }

    async fn handle_edit<T, L>(&self, ctx: &CommandContext<'_, T, L>) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        if ctx.args.len() < 3 {
            ctx.sender
                .reply(ctx.privmsg, "Nutze: !p edit <name> <template>")
                .await;
            return Ok(());
        }
        let name = normalize_ping_name(ctx.args[1]);
        let template = ctx.args[2..].join(" ");
        match self.ping.edit_template(name.clone(), template).await {
            Ok(()) => {
                ctx.sender
                    .reply(ctx.privmsg, format!("Ping \"{name}\" updated SeemsGood"))
                    .await;
            }
            Err(e) => {
                ctx.sender.reply(ctx.privmsg, format!("{e} FDM")).await;
            }
        }
        Ok(())
    }

    async fn handle_member_op<T, L>(&self, ctx: &CommandContext<'_, T, L>, op: &str) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        if ctx.args.len() < 3 {
            ctx.sender
                .reply(ctx.privmsg, format!("Nutze: !p {op} <name> <user>"))
                .await;
            return Ok(());
        }
        let name = normalize_ping_name(ctx.args[1]);
        let user = normalize_username(ctx.args[2]);
        let result = match op {
            "add" => self.ping.add_member(name.clone(), user.clone()).await,
            "remove" => self.ping.remove_member(name.clone(), user.clone()).await,
            _ => unreachable!(),
        };
        match result {
            Ok(()) => {
                let msg = match op {
                    "add" => format!("{user} zu \"{name}\" hinzugefügt Okayge"),
                    "remove" => format!("{user} aus \"{name}\" entfernt Okayge"),
                    _ => unreachable!(),
                };
                ctx.sender.reply(ctx.privmsg, msg).await;
            }
            Err(e) => {
                ctx.sender.reply(ctx.privmsg, format!("{e} FDM")).await;
            }
        }
        Ok(())
    }

    async fn handle_self_op<T, L>(&self, ctx: &CommandContext<'_, T, L>, op: &str) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        let name = match ctx.args.get(1) {
            Some(n) => normalize_ping_name(n),
            None => {
                ctx.sender
                    .reply(ctx.privmsg, format!("Nutze: !p {op} <name>"))
                    .await;
                return Ok(());
            }
        };
        let result = match op {
            "join" => {
                self.ping
                    .add_member(name.clone(), ctx.privmsg.sender.login.clone())
                    .await
            }
            "leave" => {
                self.ping
                    .remove_member(name.clone(), ctx.privmsg.sender.login.clone())
                    .await
            }
            _ => unreachable!(),
        };
        match result {
            Ok(()) => {
                ctx.sender
                    .reply(ctx.privmsg, "Hab ich gemacht Okayge")
                    .await;
            }
            Err(e) => {
                let err_str = e.to_string();
                let msg = if err_str.contains("gibt es nicht") {
                    format!("{err_str} FDM")
                } else {
                    match op {
                        "join" => "Bist du schon FDM".to_string(),
                        "leave" => "Bist du nicht drin FDM".to_string(),
                        _ => unreachable!(),
                    }
                };
                ctx.sender.reply(ctx.privmsg, msg).await;
            }
        }
        Ok(())
    }

    async fn handle_list<T, L>(&self, ctx: &CommandContext<'_, T, L>) -> Result<()>
    where
        T: Transport,
        L: LoginCredentials,
    {
        let pings = self
            .ping
            .list_for_user(ctx.privmsg.sender.login.clone())
            .await;
        let response = if pings.is_empty() {
            "Keine Pings".to_string()
        } else {
            pings.join(" ")
        };
        ctx.sender.reply(ctx.privmsg, response).await;
        Ok(())
    }
}
