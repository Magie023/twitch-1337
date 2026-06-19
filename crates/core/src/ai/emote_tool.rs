//! On-demand `search_emotes` chat-turn tool.
//!
//! Lets the model reach the full channel 7TV set (≈982 entries) without
//! carrying the whole catalog in every prompt. Registered only when the emote
//! provider is active (mirrors the conditional web-tool registration in
//! `command.rs`).

use llm::{ToolCall, ToolDefinition, ToolResultMessage};
use serde::Deserialize;
use serde_json::json;

use crate::twitch::seventv::SevenTvEmoteProvider;

pub const SEARCH_EMOTES_TOOL_NAME: &str = "search_emotes";

#[derive(Debug, Deserialize)]
struct SearchEmotesArgs {
    query: String,
}

pub fn search_emotes_tool() -> ToolDefinition {
    ToolDefinition {
        name: SEARCH_EMOTES_TOOL_NAME.into(),
        description: "Search the channel's full 7TV emote set by meaning or by partial code and \
            return the best matches (exact code + meaning + usage). Call this when you want an \
            emote for a feeling or moment that is not among the emotes listed in the system \
            prompt — never invent emote codes. Returns up to 20 ranked matches."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Free-text description of the emote you want (e.g. \"laughing\", \"agreement\", \"sarcastic clap\") or a partial emote code.",
                }
            },
            "required": ["query"]
        }),
    }
}

/// Execute a `search_emotes` call against the live emote provider for the given
/// Twitch channel. Always returns a `ToolResultMessage` — malformed args and
/// no-match cases degrade to a plain instructional string rather than erroring.
pub async fn execute_search_emotes(
    provider: &SevenTvEmoteProvider,
    twitch_channel_id: &str,
    call: &ToolCall,
) -> ToolResultMessage {
    let args = match call.parse_args::<SearchEmotesArgs>() {
        Ok(a) => a,
        Err(e) => {
            return ToolResultMessage::for_call(
                call,
                json!({"error": "invalid_arguments", "details": e.to_string()}).to_string(),
            );
        }
    };

    let query = args.query.trim();
    if query.is_empty() {
        return ToolResultMessage::for_call(
            call,
            "Provide a non-empty query describing the emote you want.".to_string(),
        );
    }

    let block = provider.search_emotes(twitch_channel_id, query).await;
    ToolResultMessage::for_call(call, block)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_def_has_expected_name_and_required_query() {
        let t = search_emotes_tool();
        assert_eq!(t.name, SEARCH_EMOTES_TOOL_NAME);
        assert_eq!(t.parameters["required"][0], "query");
    }

    #[test]
    fn search_emotes_is_not_a_web_tool() {
        assert!(!crate::ai::content::is_web_tool(SEARCH_EMOTES_TOOL_NAME));
    }
}
