use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eyre::Result;
use tracing::{debug, error, instrument};
use twitch_irc::{login::LoginCredentials, transport::Transport};

use llm::{ChatCompletionRequest, LlmClient, Message, TraceIds};

use crate::ai::command::ChatContext;
use crate::cooldown::{PerUserCooldown, format_cooldown_remaining};
use crate::settings::SettingsHandle;
use crate::util::MAX_RESPONSE_LENGTH;

use super::{Command, CommandContext};

const HAIKU_CONTEXT_MESSAGES: usize = 80;
const HAIKU_LINE_MAX_CHARS: usize = 80;
const HAIKU_PARTS: usize = 3;
const HAIKU_SYSTEM_PROMPT: &str = "Du liest einen Twitch-Chat-Auszug und schreibst genau ein Haiku. Wähle intern ein salientes Thema aus dem Verlauf (erkläre die Wahl nicht). Antworte mit genau einer Zeile für Twitch: drei kurze Teile im 5-7-5-Silbenschema, getrennt durch \" / \" (Leerzeichen-Slash-Leerzeichen). Bei Deutsch ist die Silbenzählung näherungsweise. Kein Markdown, keine Anführungszeichen, keine erfundenen Chat-Ereignisse, keine Zusammenfassung oder Prosa.";
const EMPTY_HISTORY_MESSAGE: &str = "Ich habe noch keinen Chat-Verlauf für ein Haiku FDM";
const LLM_ERROR_MESSAGE: &str = "Da ist was schiefgelaufen FDM";
const LLM_TIMEOUT_MESSAGE: &str = "Das hat zu lange gedauert Waiting";

pub struct HaikuCommand {
    llm_client: Arc<dyn LlmClient>,
    settings: SettingsHandle,
    cooldown: Arc<PerUserCooldown>,
    chat_ctx: Option<ChatContext>,
}

impl HaikuCommand {
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        settings: SettingsHandle,
        chat_ctx: Option<ChatContext>,
        cooldown: Arc<PerUserCooldown>,
    ) -> Self {
        Self {
            llm_client,
            settings,
            cooldown,
            chat_ctx,
        }
    }

    async fn relevant_history(&self, user: &str, current_message: &str) -> Option<Vec<String>> {
        let chat = self.chat_ctx.as_ref()?;
        let mut snapshot = {
            let buf = chat.primary_history.lock().await;
            buf.snapshot()
        };

        if snapshot.last().is_some_and(|entry| {
            entry.username.eq_ignore_ascii_case(user)
                && entry.text.eq_ignore_ascii_case(current_message)
        }) {
            snapshot.pop();
        }

        if snapshot.is_empty() {
            return None;
        }

        let start = snapshot.len().saturating_sub(HAIKU_CONTEXT_MESSAGES);
        let messages = snapshot[start..]
            .iter()
            .map(|entry| format!("{}: {}", entry.username, entry.text))
            .collect();

        Some(messages)
    }
}

/// Split model output into haiku parts and join for a single Twitch line.
fn format_haiku_for_chat(raw: &str) -> Option<String> {
    let parts: Vec<String> = if raw.contains(" / ") {
        raw.split(" / ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        raw.lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };

    if parts.len() != HAIKU_PARTS {
        return None;
    }

    if !parts.iter().all(|p| is_valid_haiku_part(p)) {
        return None;
    }

    Some(truncate_plain(&parts.join(" / "), MAX_RESPONSE_LENGTH))
}

fn is_valid_haiku_part(part: &str) -> bool {
    if part.is_empty() || part.chars().count() > HAIKU_LINE_MAX_CHARS {
        return false;
    }
    let lower = part.to_ascii_lowercase();
    if part.contains("```")
        || part.contains("**")
        || part.contains('#')
        || lower.starts_with("icymi:")
        || lower.contains("in den letzten 24h:")
    {
        return false;
    }
    true
}

fn truncate_plain(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

#[async_trait]
impl<T, L> Command<T, L> for HaikuCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!haiku"
    }

    #[instrument(skip(self, ctx))]
    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let user = &ctx.privmsg.sender.login;

        if let Some(remaining) = self.cooldown.check(user).await {
            debug!(user = %user, remaining_secs = remaining.as_secs(), "Haiku command on cooldown");
            if let Err(e) = ctx
                .client
                .say_in_reply_to(
                    ctx.privmsg,
                    format!(
                        "Bitte warte noch {} Waiting",
                        format_cooldown_remaining(remaining)
                    ),
                )
                .await
            {
                error!(error = ?e, "Failed to send cooldown message");
            }
            return Ok(());
        }

        let Some(history_lines) = self.relevant_history(user, &ctx.privmsg.message_text).await
        else {
            if let Err(e) = ctx
                .client
                .say_in_reply_to(ctx.privmsg, EMPTY_HISTORY_MESSAGE.to_string())
                .await
            {
                error!(error = ?e, "Failed to send empty-history message");
            }
            return Ok(());
        };

        let snap = self.settings.load();
        let model = snap.ai.connection.model.clone();
        let timeout = Duration::from_secs(snap.ai.connection.timeout);
        drop(snap);

        let user_message = format!(
            "Schreibe ein Haiku zu einem Thema aus diesem Twitch-Chat.\n{}",
            history_lines.join("\n"),
        );

        let request = ChatCompletionRequest {
            model,
            messages: vec![
                Message::system(HAIKU_SYSTEM_PROMPT),
                Message::user(user_message),
            ],
            reasoning_effort: None,
            service_tier: None,
            trace: TraceIds {
                user: Some(user.clone()),
                session_id: Some(crate::ai::session::new_session_id()),
            },
        };

        let result = tokio::time::timeout(timeout, self.llm_client.chat_completion(request)).await;

        let (response, record_cooldown) = match result {
            Ok(Ok(text)) => match format_haiku_for_chat(text.trim()) {
                Some(haiku) => (haiku, true),
                None => {
                    error!("Haiku AI returned invalid format");
                    (LLM_ERROR_MESSAGE.to_string(), false)
                }
            },
            Ok(Err(e)) => {
                error!(error = ?e, "Haiku AI execution failed");
                (LLM_ERROR_MESSAGE.to_string(), false)
            }
            Err(_) => {
                error!("Haiku AI execution timed out");
                (LLM_TIMEOUT_MESSAGE.to_string(), false)
            }
        };

        if record_cooldown {
            self.cooldown.record(user).await;
        }

        if let Err(e) = ctx.client.say_in_reply_to(ctx.privmsg, response).await {
            error!(error = ?e, "Failed to send haiku response");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_haiku_joins_slash_parts() {
        let out = format_haiku_for_chat("a / b / c").unwrap();
        assert_eq!(out, "a / b / c");
    }

    #[test]
    fn format_haiku_joins_newlines() {
        let out = format_haiku_for_chat("a\nb\nc").unwrap();
        assert_eq!(out, "a / b / c");
    }

    #[test]
    fn format_haiku_rejects_markdown() {
        assert!(format_haiku_for_chat("```\na\nb\nc").is_none());
    }

    #[test]
    fn format_haiku_rejects_wrong_part_count() {
        assert!(format_haiku_for_chat("only one line").is_none());
    }
}
