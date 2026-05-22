//! Single sanitizing wrapper around outbound chat sends.
//!
//! Every PRIVMSG the bot emits goes through [`ChatSender`]. The sanitizer
//! strips control characters (so a stray `\r\n` cannot split the IRC line),
//! collapses the resulting whitespace runs, and byte-clamps the payload
//! below Twitch's 500-byte limit with a `…` suffix when truncated.
//! Send errors are logged and swallowed: a chat hiccup must not abort the
//! caller's task.

use std::sync::Arc;

use tracing::{error, warn};
use twitch_irc::{
    TwitchIRCClient, login::LoginCredentials, message::PrivmsgMessage, transport::Transport,
};

/// Maximum byte length of the post-sanitize payload. Twitch enforces a
/// 500-byte PRIVMSG limit; the 480-byte ceiling leaves headroom for IRC
/// framing tags injected by `say_in_reply_to`.
const MAX_OUTBOUND_BYTES: usize = 480;

/// Strip control characters, collapse whitespace runs, and byte-clamp to
/// [`MAX_OUTBOUND_BYTES`] with a `…` (U+2026) suffix on truncation.
///
/// Idempotent: `sanitize_outbound(sanitize_outbound(x)) == sanitize_outbound(x)`.
/// Always truncates on a UTF-8 char boundary. Does not trim leading/trailing
/// whitespace — callers handle that semantically; only collapse is performed here.
pub fn sanitize_outbound(input: &str) -> String {
    let mut collapsed = String::with_capacity(input.len());
    let mut prev_space = false;
    for ch in input.chars() {
        let is_space = ch.is_whitespace() || ch.is_control();
        if is_space {
            if !prev_space {
                collapsed.push(' ');
            }
            prev_space = true;
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }

    if collapsed.len() <= MAX_OUTBOUND_BYTES {
        return collapsed;
    }

    // Truncate on a char boundary, reserving 3 bytes for the U+2026 ellipsis.
    const ELLIPSIS: char = '…';
    let ellipsis_len = ELLIPSIS.len_utf8();
    let budget = MAX_OUTBOUND_BYTES - ellipsis_len;

    let mut end = 0;
    for (idx, ch) in collapsed.char_indices() {
        let next = idx + ch.len_utf8();
        if next > budget {
            break;
        }
        end = next;
    }

    let mut out = String::with_capacity(end + ellipsis_len);
    out.push_str(&collapsed[..end]);
    out.push(ELLIPSIS);
    out
}

/// Sanitizing wrapper around [`TwitchIRCClient`]. The only chat-send API
/// the rest of the bot is allowed to call.
pub struct ChatSender<T, L>
where
    T: Transport,
    L: LoginCredentials,
{
    client: Arc<TwitchIRCClient<T, L>>,
}

impl<T, L> ChatSender<T, L>
where
    T: Transport,
    L: LoginCredentials,
{
    pub fn new(client: Arc<TwitchIRCClient<T, L>>) -> Arc<Self> {
        Arc::new(Self { client })
    }

    /// Send `message` as a PRIVMSG to `channel`. Sanitizes the payload,
    /// drops empty results, and logs (does not propagate) IRC send errors.
    pub async fn say(&self, channel: String, message: impl Into<String>) {
        let cleaned = sanitize_outbound(&message.into());
        if cleaned.is_empty() {
            warn!(%channel, "outbound message empty after sanitize, dropping");
            return;
        }
        if let Err(error) = self.client.say(channel, cleaned).await {
            error!(?error, "chat send failed");
        }
    }

    /// Reply to `privmsg` with `message`. Sanitizes the payload, drops empty
    /// results, and logs (does not propagate) IRC send errors.
    pub async fn reply(&self, privmsg: &PrivmsgMessage, message: impl Into<String>) {
        let cleaned = sanitize_outbound(&message.into());
        if cleaned.is_empty() {
            warn!(channel = %privmsg.channel_login, "outbound reply empty after sanitize, dropping");
            return;
        }
        if let Err(error) = self.client.say_in_reply_to(privmsg, cleaned).await {
            error!(?error, "chat reply failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_passthrough_unchanged() {
        assert_eq!(sanitize_outbound("hello world"), "hello world");
        assert_eq!(sanitize_outbound("!lb top10"), "!lb top10");
    }

    #[test]
    fn crlf_collapsed_to_single_space() {
        assert_eq!(sanitize_outbound("line1\r\nline2"), "line1 line2");
        assert_eq!(sanitize_outbound("a\rb\nc"), "a b c");
    }

    #[test]
    fn nul_only_collapses_to_single_space() {
        // Three NULs collapse to one space (no trim — left to callers).
        assert_eq!(sanitize_outbound("\0\0\0"), " ");
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(sanitize_outbound(""), "");
    }

    #[test]
    fn long_ascii_input_clamped_with_ellipsis() {
        let input: String = std::iter::repeat_n('a', 600).collect();
        let out = sanitize_outbound(&input);
        assert!(out.len() <= MAX_OUTBOUND_BYTES);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn multibyte_input_clamped_on_char_boundary() {
        // "ä" is 2 bytes UTF-8; 300 of them = 600 bytes. The real check is
        // that slicing inside a multibyte char doesn't panic.
        let input: String = "ä".repeat(300);
        let out = sanitize_outbound(&input);
        assert!(out.len() <= MAX_OUTBOUND_BYTES);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn sanitize_is_idempotent() {
        let messy = "hello\r\n\tworld\0  foo\n\n\nbar";
        let once = sanitize_outbound(messy);
        let twice = sanitize_outbound(&once);
        assert_eq!(once, twice);

        let truncated = sanitize_outbound(&"x".repeat(600));
        assert_eq!(sanitize_outbound(&truncated), truncated);
    }

    #[test]
    fn ellipsis_fits_within_byte_budget() {
        // Final byte length must include the 3-byte ellipsis and stay ≤ 480.
        let input = "y".repeat(MAX_OUTBOUND_BYTES * 2);
        let out = sanitize_outbound(&input);
        assert_eq!(out.len(), MAX_OUTBOUND_BYTES);
        assert!(out.ends_with('…'));
    }
}
