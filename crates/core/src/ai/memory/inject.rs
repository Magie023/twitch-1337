//! Prompt composition: nonce-fenced inject of every memory + state file body,
//! plus prompt-file substitution.

use std::sync::Arc;

use chrono_tz::Europe::Berlin;
use eyre::Result;
use rand::Rng as _;
use tokio::sync::Mutex;

use crate::ai::chat_history::{ChatHistoryBuffer, ChatHistoryEntry, ChatHistorySource};
use crate::ai::memory::store::MemoryStore;
use crate::ai::memory::types::FileKind;

const FENCE_OPEN: &str = "<<<FILE";
const FENCE_CLOSE: &str = "<<<ENDFILE";

/// Identifies what a fenced inject block represents. Renders into the FILE
/// header attrs so the model can map a block to its subject without parsing
/// a path. Path-style addressing only re-appears in the `write_file` tool's
/// `path` argument, where it's the canonical way to address a write target.
#[derive(Debug, Clone)]
pub enum FenceLabel<'a> {
    Soul,
    Lore,
    User {
        id: &'a str,
        login: Option<&'a str>,
        display_name: Option<&'a str>,
    },
    State {
        slug: &'a str,
    },
    Transcript {
        date: &'a str,
    },
}

/// Per-section byte caps for rolling chat injected into the v2 prompt.
/// Independent of `inject_byte_budget`, which covers SOUL/LORE/users/state.
const RECENT_CHAT_PRIMARY_BYTES: usize = 2048;
const RECENT_CHAT_AI_CHANNEL_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationChannel {
    Primary,
    AiChannel,
}

/// Generate a 16-hex-char nonce for one prompt build.
pub fn fresh_nonce() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn fence_block(label: FenceLabel<'_>, nonce: &str, body: &str) -> String {
    let safe = scrub_for_inject(body);
    let attrs = render_label_attrs(&label);
    format!("<<<FILE {attrs} nonce={nonce}>>>\n{safe}\n<<<ENDFILE nonce={nonce}>>>")
}

fn render_label_attrs(label: &FenceLabel<'_>) -> String {
    match label {
        FenceLabel::Soul => "kind=soul".into(),
        FenceLabel::Lore => "kind=lore".into(),
        FenceLabel::State { slug } => format!("kind=state slug={slug}"),
        FenceLabel::Transcript { date } => format!("kind=transcript date={date}"),
        FenceLabel::User {
            id,
            login,
            display_name,
        } => {
            let mut s = format!("kind=user id={id}");
            if let Some(l) = login.filter(|l| !l.is_empty()) {
                s.push_str(&format!(" login={l}"));
            }
            if let Some(n) = display_name.filter(|n| !n.is_empty()) {
                s.push_str(&format!(" name=\"{}\"", sanitize_attr_value(n)));
            }
            s
        }
    }
}

/// Strip characters that would break the FILE header (quotes, newlines, `>`).
/// Display names are already control-stripped at write time, but we apply
/// the same hardening at render time to keep the marker syntactically clean.
fn sanitize_attr_value(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '>' && *c != '<')
        .collect()
}

/// If a body contains either fence sentinel, replace it wholesale.
pub fn scrub_for_inject(body: &str) -> String {
    if body.contains(FENCE_OPEN) || body.contains(FENCE_CLOSE) {
        tracing::error!("memory body contained fence sentinel at inject time, scrubbed");
        return "<corrupt: rejected>".to_string();
    }
    body.to_string()
}

#[derive(Clone, Copy)]
pub struct SubstitutionVars<'a> {
    pub speaker_username: &'a str,
    pub speaker_display: &'a str,
    pub speaker_user_id: &'a str,
    pub speaker_role: &'a str,
    pub channel: &'a str,
    pub date: &'a str,
}

pub fn substitute(template: &str, v: SubstitutionVars<'_>) -> String {
    template
        .replace("{speaker_username}", v.speaker_username)
        .replace("{speaker_display}", v.speaker_display)
        .replace("{speaker_user_id}", v.speaker_user_id)
        .replace("{speaker_role}", v.speaker_role)
        .replace("{channel}", v.channel)
        .replace("{date}", v.date)
}

pub struct BuildOpts {
    pub inject_byte_budget: usize,
    pub nonce: String,
    pub primary_history: Option<Arc<Mutex<ChatHistoryBuffer>>>,
    pub primary_login: String,
    pub ai_channel_history: Option<Arc<Mutex<ChatHistoryBuffer>>>,
    pub ai_channel_login: Option<String>,
    pub invocation_channel: InvocationChannel,
    pub bot_login: String,
    pub persona_name: String,
    /// Lowercased Twitch login of the user that triggered this turn (i.e.
    /// `!ai` invoker). Empty string for the dreamer ritual (no speaker).
    /// Used by [`build_chat_turn_context`] to scope the injected user-file
    /// set down to chat-window users plus the speaker.
    pub speaker_login: String,
}

/// Result of [`build_chat_turn_context`].
///
/// The three fields exist so callers can route blocks to whichever message
/// maximises prompt-cache hits:
/// - `durable_memory` holds SOUL/LORE/users (changes on dreamer runs only).
///   Lives in the system message so consecutive turns hit the prompt cache.
/// - `volatile_state` holds the state/<slug> blocks (every `write_state` /
///   `delete_state` mutates them). Lives in the user message so it can change
///   freely without invalidating the system-message cache.
/// - `recent_chat` holds the rolling per-turn chat history (volatile by
///   definition). Lives in the user message.
///
/// Any field may be empty. Callers that want the legacy combined memory blob
/// (dreamer) concatenate `durable_memory` + `volatile_state` themselves.
pub struct ChatTurnContext {
    pub recent_chat: String,
    pub durable_memory: String,
    pub volatile_state: String,
}

/// Build the chat-turn injected context split into a recent-chat section and a
/// memory section. Callers place each in the message that maximizes prompt
/// caching: memory in the system message, recent chat in the user message.
pub async fn build_chat_turn_context(
    store: &MemoryStore,
    opts: BuildOpts,
) -> Result<ChatTurnContext> {
    let primary_rendered = render_recent_section(
        opts.primary_history.as_ref(),
        &opts.primary_login,
        RECENT_CHAT_PRIMARY_BYTES,
        &opts.bot_login,
        &opts.persona_name,
    )
    .await;
    let ai_rendered = match (
        opts.ai_channel_history.as_ref(),
        opts.ai_channel_login.as_ref(),
    ) {
        (Some(buf), Some(login)) => {
            render_recent_section(
                Some(buf),
                login,
                RECENT_CHAT_AI_CHANNEL_BYTES,
                &opts.bot_login,
                &opts.persona_name,
            )
            .await
        }
        _ => None,
    };

    let (first, second) = match opts.invocation_channel {
        InvocationChannel::AiChannel => (ai_rendered, primary_rendered),
        InvocationChannel::Primary => (primary_rendered, ai_rendered),
    };
    let mut mentioned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut recent_sections: Vec<String> = Vec::with_capacity(2);
    for section in [first, second].into_iter().flatten() {
        for u in section.usernames {
            mentioned.insert(u);
        }
        recent_sections.push(section.body);
    }

    let soul = store.read_kind(&FileKind::Soul).await?;
    let lore = store.read_kind(&FileKind::Lore).await?;
    let mut users = store.list_users().await?;
    let mut states = store.list_state().await?;

    let mut durable_blocks: Vec<String> = Vec::new();
    durable_blocks.push(fence_block(FenceLabel::Soul, &opts.nonce, &soul.body));
    durable_blocks.push(fence_block(FenceLabel::Lore, &opts.nonce, &lore.body));

    // Scope user files to logins present in the chat window plus the speaker.
    // Users who didn't appear in either are excluded outright: the model only
    // needs character sheets for people it's actually about to talk to or
    // about. Without this filter, lurkers with recent `updated_at` would
    // crowd out the speaker's own file when the budget is tight.
    //
    // Short-circuit for the dreamer: no history buffers AND no speaker means
    // this isn't a chat turn, it's the dreamer ritual, which wants every user
    // file in its system prompt. Skipping the filter in that case keeps the
    // dreamer's pre-existing "every memory file in one shot" contract intact.
    let speaker_lc = opts.speaker_login.to_ascii_lowercase();
    let is_chat_turn = opts.primary_history.is_some()
        || opts.ai_channel_history.is_some()
        || !speaker_lc.is_empty();
    if is_chat_turn {
        let mut scope: std::collections::BTreeSet<String> = mentioned.clone();
        if !speaker_lc.is_empty() {
            scope.insert(speaker_lc.clone());
        }
        users.retain(|f| {
            let Some(login) = f.frontmatter.username.as_deref() else {
                return false;
            };
            scope.contains(&login.to_ascii_lowercase())
        });
    }

    // Speaker first, then newest-first. Ensures the speaker's file always
    // survives the byte-budget packing loop below. When speaker_login is
    // empty (dreamer), this collapses to a plain updated_at DESC sort.
    users.sort_by(|a, b| {
        let a_is_speaker = !speaker_lc.is_empty()
            && a.frontmatter
                .username
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(&speaker_lc));
        let b_is_speaker = !speaker_lc.is_empty()
            && b.frontmatter
                .username
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(&speaker_lc));
        b_is_speaker
            .cmp(&a_is_speaker)
            .then_with(|| b.frontmatter.updated_at.cmp(&a.frontmatter.updated_at))
    });
    states.sort_by_key(|f| std::cmp::Reverse(f.frontmatter.updated_at));

    // Pack user blocks until the durable-memory budget is hit. State blocks get
    // whatever budget is left over so heavy LORE/user nights don't crowd them
    // out entirely; remaining state still pops as oldest-first drops.
    let mut total: usize = durable_blocks.iter().map(String::len).sum();
    for f in users.drain(..) {
        let FileKind::User { user_id } = &f.kind else {
            tracing::error!(?f.kind, "non-user file in user list, skipping");
            continue;
        };
        let label = FenceLabel::User {
            id: user_id,
            login: f.frontmatter.username.as_deref(),
            display_name: f.frontmatter.display_name.as_deref(),
        };
        let block = fence_block(label, &opts.nonce, &f.body);
        if total + block.len() + 1 > opts.inject_byte_budget {
            break;
        }
        total += block.len() + 1;
        durable_blocks.push(block);
    }

    let mut state_blocks: Vec<String> = Vec::new();
    for f in states.drain(..) {
        let FileKind::State { slug } = &f.kind else {
            tracing::error!(?f.kind, "non-state file in state list, skipping");
            continue;
        };
        let block = fence_block(FenceLabel::State { slug }, &opts.nonce, &f.body);
        if total + block.len() + 1 > opts.inject_byte_budget {
            break;
        }
        total += block.len() + 1;
        state_blocks.push(block);
    }

    let durable_memory = durable_blocks.join("\n");
    let volatile_state = state_blocks.join("\n");
    let recent_chat = recent_sections.join("\n\n");
    Ok(ChatTurnContext {
        recent_chat,
        durable_memory,
        volatile_state,
    })
}

struct RenderedRecentSection {
    body: String,
    usernames: Vec<String>,
}

/// Render one `## Recent chat (#login)` section, newest-first up to `cap` bytes,
/// then reverse to chronological order. Also returns the lowercased usernames
/// of every line that survived the byte cap. Returns `None` for missing or
/// empty buffers.
async fn render_recent_section(
    buf: Option<&Arc<Mutex<ChatHistoryBuffer>>>,
    login: &str,
    cap: usize,
    bot_login: &str,
    persona_name: &str,
) -> Option<RenderedRecentSection> {
    let buf = buf?;
    let snapshot: Vec<ChatHistoryEntry> = buf.lock().await.snapshot();
    if snapshot.is_empty() {
        return None;
    }

    let mut chosen: Vec<String> = Vec::new();
    let mut usernames: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    for entry in snapshot.iter().rev() {
        let line = format_entry_line(entry, bot_login, persona_name);
        let line_bytes = line.len() + 1;
        if bytes + line_bytes > cap {
            break;
        }
        bytes += line_bytes;
        chosen.push(line);
        usernames.push(entry.username.to_ascii_lowercase());
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.reverse();

    let mut body = format!("## Recent chat (#{login})\n");
    body.push_str(&chosen.join("\n"));
    Some(RenderedRecentSection { body, usernames })
}

fn format_entry_line(entry: &ChatHistoryEntry, bot_login: &str, persona_name: &str) -> String {
    let ts = entry.timestamp.with_timezone(&Berlin).format("%H:%M");
    let is_self =
        entry.source == ChatHistorySource::Bot || entry.username.eq_ignore_ascii_case(bot_login);
    if is_self {
        return format!("[{ts}] {persona_name} (self): {}", entry.text);
    }
    let name = entry
        .display_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(entry.username.as_str());
    format!("[{ts}] {name}: {}", entry.text)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arc_swap::ArcSwap;

    use super::*;
    use crate::ai::memory::store::MemoryStore;
    use crate::ai::memory::types::FileKind;
    use crate::settings::Settings;

    #[test]
    fn format_entry_line_self_uses_persona_and_self_tag() {
        let entry = ChatHistoryEntry {
            seq: 1,
            username: "chronophylosbot".into(),
            display_name: Some("Aurora".into()),
            user_id: None,
            text: "gemerkt".into(),
            source: ChatHistorySource::Bot,
            timestamp: chrono::Utc::now(),
        };
        let line = format_entry_line(&entry, "chronophylosbot", "Aurora");
        assert!(line.ends_with(" Aurora (self): gemerkt"), "got: {line}");
    }

    #[test]
    fn format_entry_line_other_uses_display_name() {
        let entry = ChatHistoryEntry {
            seq: 1,
            username: "magie_023".into(),
            display_name: Some("MagieDisplay".into()),
            user_id: Some("141690010".into()),
            text: "hi".into(),
            source: ChatHistorySource::User,
            timestamp: chrono::Utc::now(),
        };
        let line = format_entry_line(&entry, "chronophylosbot", "Aurora");
        assert!(line.ends_with(" MagieDisplay: hi"), "got: {line}");
    }

    #[test]
    fn format_entry_line_other_falls_back_to_username_when_no_display() {
        let entry = ChatHistoryEntry {
            seq: 1,
            username: "lurker42".into(),
            display_name: None,
            user_id: None,
            text: "?".into(),
            source: ChatHistorySource::User,
            timestamp: chrono::Utc::now(),
        };
        let line = format_entry_line(&entry, "chronophylosbot", "Aurora");
        assert!(line.ends_with(" lurker42: ?"), "got: {line}");
    }

    fn test_handle() -> crate::settings::SettingsHandle {
        Arc::new(ArcSwap::from_pointee(Settings::compiled_defaults()))
    }

    #[test]
    fn fence_block_user_renders_identity_attrs() {
        let s = fence_block(
            FenceLabel::User {
                id: "12",
                login: Some("alicepleb"),
                display_name: Some("Alice Pleb"),
            },
            "abc123abc123abc1",
            "body\n",
        );
        assert!(s.starts_with(
            "<<<FILE kind=user id=12 login=alicepleb name=\"Alice Pleb\" nonce=abc123abc123abc1>>>"
        ));
        assert!(s.ends_with("<<<ENDFILE nonce=abc123abc123abc1>>>"));
        assert!(s.contains("body\n"));
    }

    #[test]
    fn fence_block_user_omits_missing_identity_attrs() {
        let s = fence_block(
            FenceLabel::User {
                id: "12",
                login: None,
                display_name: None,
            },
            "n",
            "x",
        );
        assert!(s.starts_with("<<<FILE kind=user id=12 nonce=n>>>"));
    }

    #[test]
    fn fence_block_soul_lore_state_transcript_use_kind_attrs() {
        assert!(
            fence_block(FenceLabel::Soul, "n", "x").starts_with("<<<FILE kind=soul nonce=n>>>")
        );
        assert!(
            fence_block(FenceLabel::Lore, "n", "x").starts_with("<<<FILE kind=lore nonce=n>>>")
        );
        assert!(
            fence_block(FenceLabel::State { slug: "quiz" }, "n", "x")
                .starts_with("<<<FILE kind=state slug=quiz nonce=n>>>")
        );
        assert!(
            fence_block(FenceLabel::Transcript { date: "2026-05-05" }, "n", "x",)
                .starts_with("<<<FILE kind=transcript date=2026-05-05 nonce=n>>>")
        );
    }

    #[test]
    fn fence_block_strips_quote_breakers_from_display_name() {
        let s = fence_block(
            FenceLabel::User {
                id: "1",
                login: Some("a"),
                display_name: Some("we\"ird>guy"),
            },
            "n",
            "x",
        );
        assert!(s.contains("name=\"weirdguy\""), "got: {s}");
    }

    #[test]
    fn substitute_only_replaces_known_tokens() {
        let s = substitute(
            "hi {speaker_username} on {channel} {date} {speaker_role} {unknown}",
            SubstitutionVars {
                speaker_username: "alice",
                speaker_display: "Alice",
                speaker_user_id: "42",
                speaker_role: "regular",
                channel: "ch",
                date: "2026-04-30",
            },
        );
        assert_eq!(s, "hi alice on ch 2026-04-30 regular {unknown}");
    }

    #[test]
    fn substitute_renders_speaker_marker_block() {
        // Mirror the shape of the bundled `ai_instructions.md` marker line so
        // this test fails if the prompt format drifts from substitute()'s tokens.
        let tmpl = ">>> Antwort auf {speaker_display} (login={speaker_username}, id={speaker_user_id}, role={speaker_role}):\n";
        let out = substitute(
            tmpl,
            SubstitutionVars {
                speaker_username: "magie_023",
                speaker_display: "MagieDisplay",
                speaker_user_id: "141690010",
                speaker_role: "regular",
                channel: "euterheissgetraenk",
                date: "2026-05-16",
            },
        );
        assert!(out.contains(
            ">>> Antwort auf MagieDisplay (login=magie_023, id=141690010, role=regular):"
        ));
    }

    #[test]
    fn bundled_ai_instructions_substitutes_speaker_marker_cleanly() {
        // Drives the production prompt through substitute() and verifies the
        // marker line emerges with no leftover `{...}` placeholders.
        let tmpl = include_str!("../../../data/prompts/ai_instructions.md");
        let out = substitute(
            tmpl,
            SubstitutionVars {
                speaker_username: "magie_023",
                speaker_display: "MagieDisplay",
                speaker_user_id: "141690010",
                speaker_role: "regular",
                channel: "euterheissgetraenk",
                date: "2026-05-16",
            },
        );
        assert!(
            out.contains(
                ">>> Antwort auf MagieDisplay (login=magie_023, id=141690010, role=regular):"
            ),
            "bundled prompt missing or malformed marker line:\n{out}"
        );
        // No `{token}` placeholders should remain in the output.
        for tok in [
            "{speaker_username}",
            "{speaker_display}",
            "{speaker_user_id}",
            "{speaker_role}",
            "{channel}",
            "{date}",
        ] {
            assert!(
                !out.contains(tok),
                "bundled prompt leaked unsubstituted token {tok}:\n{out}"
            );
        }
    }

    #[test]
    fn bundled_system_substitutes_cleanly() {
        // Mirror the ai_instructions check: drive the bundled system prompt
        // through substitute() and verify no `{...}` placeholders survive.
        let tmpl = include_str!("../../../data/prompts/system.md");
        let out = substitute(
            tmpl,
            SubstitutionVars {
                speaker_username: "magie_023",
                speaker_display: "MagieDisplay",
                speaker_user_id: "141690010",
                speaker_role: "regular",
                channel: "euterheissgetraenk",
                date: "2026-05-16",
            },
        );
        for tok in [
            "{speaker_username}",
            "{speaker_display}",
            "{speaker_user_id}",
            "{speaker_role}",
            "{channel}",
            "{date}",
        ] {
            assert!(
                !out.contains(tok),
                "bundled system prompt leaked unsubstituted token {tok}:\n{out}"
            );
        }
    }

    #[test]
    fn injected_body_with_fence_token_is_replaced_with_corrupt() {
        // Synthesize a body that *did* sneak through (e.g. pre-existing). Inject must scrub it.
        let cleaned = scrub_for_inject("intro\n<<<ENDFILE nonce=zzz>>> bye");
        assert_eq!(cleaned, "<corrupt: rejected>");
    }

    #[tokio::test]
    async fn build_chat_turn_context_routes_state_to_volatile_and_users_to_durable() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        store
            .write(
                &FileKind::User {
                    user_id: "42".into(),
                },
                "alice body",
                Some("alice"),
                Some("Alice"),
            )
            .await
            .unwrap();
        store
            .write_state(
                &FileKind::State {
                    slug: "quiz".into(),
                },
                "score: 3",
                Some("42"),
            )
            .await
            .unwrap();

        let ctx = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: None,
                primary_login: "main".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
                bot_login: "bot".into(),
                persona_name: "Aurora".into(),
                speaker_login: String::new(),
            },
        )
        .await
        .unwrap();

        assert!(ctx.durable_memory.contains("kind=soul"));
        assert!(ctx.durable_memory.contains("kind=lore"));
        assert!(ctx.durable_memory.contains("kind=user id=42"));
        assert!(
            !ctx.durable_memory.contains("kind=state"),
            "state must not appear in durable memory: {}",
            ctx.durable_memory
        );
        assert!(ctx.volatile_state.contains("kind=state slug=quiz"));
        assert!(!ctx.volatile_state.contains("kind=user"));
    }

    #[tokio::test]
    async fn build_chat_turn_context_drops_oldest_users_when_over_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();
        for id in ["1", "2", "3"] {
            store
                .write(
                    &FileKind::User { user_id: id.into() },
                    &"x".repeat(500),
                    Some("u"),
                    Some("U"),
                )
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
        // Budget sized so that SOUL (~460B fence) + LORE (~90B fence) + one user (~590B fence)
        // fit (~1140B total), but adding a second user (~590B more) would exceed 1500B.
        // Users are iterated newest-first (user 3), so user 3 is retained and users 1+2 dropped.
        let ctx = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 1500,
                nonce: "n00000000000000nn".into(),
                primary_history: None,
                primary_login: "main".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
                bot_login: "bot".into(),
                persona_name: "Aurora".into(),
                // Empty speaker + None history: dreamer-style path, no
                // chat-window scope filter is applied, so the byte-budget
                // ordering this test exercises remains in force.
                speaker_login: String::new(),
            },
        )
        .await
        .unwrap();
        // Newest user retained, oldest dropped.
        assert!(ctx.durable_memory.contains("kind=user id=3"));
        assert!(!ctx.durable_memory.contains("kind=user id=1"));
        assert!(ctx.recent_chat.is_empty());
    }

    #[tokio::test]
    async fn build_chat_turn_context_renders_two_history_sections_invocation_first() {
        use crate::ai::chat_history::{ChatHistoryBuffer, primary_history_capacity};
        use crate::settings::test_handle;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();

        let primary = Arc::new(Mutex::new(ChatHistoryBuffer::new(
            test_handle(),
            primary_history_capacity,
        )));
        primary.lock().await.push_user("alice", "hello primary");
        let ai = Arc::new(Mutex::new(ChatHistoryBuffer::new(
            test_handle(),
            primary_history_capacity,
        )));
        ai.lock().await.push_user("bob", "hello ai");

        let body = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(primary.clone()),
                primary_login: "main".into(),
                ai_channel_history: Some(ai.clone()),
                ai_channel_login: Some("ai_chan".into()),
                invocation_channel: InvocationChannel::AiChannel,
                bot_login: "bot".into(),
                persona_name: "Aurora".into(),
                speaker_login: String::new(),
            },
        )
        .await
        .unwrap();

        let pri_idx = body
            .recent_chat
            .find("Recent chat (#main)")
            .expect("primary header");
        let ai_idx = body
            .recent_chat
            .find("Recent chat (#ai_chan)")
            .expect("ai header");
        assert!(ai_idx < pri_idx, "invocation channel must come first");
        assert!(body.recent_chat.contains("alice: hello primary"));
        assert!(body.recent_chat.contains("bob: hello ai"));
        assert!(!body.durable_memory.contains("Recent chat"));
        assert!(!body.volatile_state.contains("Recent chat"));
    }

    #[tokio::test]
    async fn build_chat_turn_context_omits_empty_history_sections() {
        use crate::ai::chat_history::{ChatHistoryBuffer, primary_history_capacity};
        use crate::settings::test_handle;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();

        let primary = Arc::new(Mutex::new(ChatHistoryBuffer::new(
            test_handle(),
            primary_history_capacity,
        )));
        primary.lock().await.push_user("alice", "hello");
        let ai = Arc::new(Mutex::new(ChatHistoryBuffer::new(
            test_handle(),
            primary_history_capacity,
        ))); // empty

        let body = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(primary),
                primary_login: "main".into(),
                ai_channel_history: Some(ai),
                ai_channel_login: Some("ai_chan".into()),
                invocation_channel: InvocationChannel::Primary,
                bot_login: "bot".into(),
                persona_name: "Aurora".into(),
                speaker_login: String::new(),
            },
        )
        .await
        .unwrap();

        assert!(body.recent_chat.contains("Recent chat (#main)"));
        assert!(!body.recent_chat.contains("Recent chat (#ai_chan)"));
    }

    #[tokio::test]
    async fn build_chat_turn_context_drops_oldest_lines_over_per_section_cap() {
        use crate::ai::chat_history::{ChatHistoryBuffer, primary_history_capacity};
        use crate::settings::test_handle;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();

        let primary = Arc::new(Mutex::new(ChatHistoryBuffer::new(
            test_handle(),
            primary_history_capacity,
        )));
        {
            let mut p = primary.lock().await;
            for _ in 0..200 {
                p.push_user("u", "x".repeat(100));
            }
        }

        let body = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 24576,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(primary),
                primary_login: "main".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
                bot_login: "bot".into(),
                persona_name: "Aurora".into(),
                speaker_login: String::new(),
            },
        )
        .await
        .unwrap();

        let primary_section_bytes = body
            .recent_chat
            .split("Recent chat (#main)")
            .nth(1)
            .unwrap_or("")
            .len();
        assert!(
            primary_section_bytes <= RECENT_CHAT_PRIMARY_BYTES + 256, // slack for header
            "primary section over cap: {primary_section_bytes}"
        );
    }

    /// Seed a user/<id>.md directly with a hand-rolled frontmatter so the test
    /// can pin `updated_at` precisely. `MemoryStore::write` always stamps
    /// `Utc::now()`, which is not enough control to verify that the newest
    /// user (`lurker`) is dropped by the scope filter rather than by recency.
    async fn seed_user_file(
        dir: &std::path::Path,
        id: &str,
        login: &str,
        display: &str,
        updated_at: chrono::DateTime<chrono::Utc>,
        body: &str,
    ) {
        let users_dir = dir.join("memories/users");
        tokio::fs::create_dir_all(&users_dir).await.unwrap();
        let raw = format!(
            "---\nupdated_at: {}\nusername: {login}\ndisplay_name: {display}\n---\n{body}",
            updated_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        tokio::fs::write(users_dir.join(format!("{id}.md")), raw)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn build_chat_turn_context_no_longer_emits_mention_table() {
        use crate::ai::chat_history::{ChatHistoryBuffer, primary_history_capacity};

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();

        // Seed a user file so that the previous code WOULD have emitted a
        // mention table row for `alice`. We assert no table appears.
        seed_user_file(
            dir.path(),
            "111",
            "alice",
            "Alice",
            chrono::Utc::now(),
            "alice body",
        )
        .await;

        let history = Arc::new(Mutex::new(ChatHistoryBuffer::new(
            test_handle(),
            primary_history_capacity,
        )));
        history.lock().await.push_user("alice", "hi");

        let ctx = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 16 * 1024,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(history),
                primary_login: "chan".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
                bot_login: "bot".into(),
                persona_name: "Aurora".into(),
                speaker_login: "alice".into(),
            },
        )
        .await
        .unwrap();

        assert!(
            !ctx.recent_chat.contains("## Mentioned users"),
            "mention table should be dropped, got:\n{}",
            ctx.recent_chat
        );
    }

    #[tokio::test]
    async fn build_chat_turn_context_scopes_users_to_chat_window_plus_speaker() {
        use crate::ai::chat_history::{ChatHistoryBuffer, primary_history_capacity};

        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path(), test_handle()).await.unwrap();

        // Seed four user files. `lurker` has the newest `updated_at` and
        // would normally win the recency scan; we expect it to be excluded
        // because it's neither the speaker nor present in the chat window.
        let now = chrono::Utc::now();
        let backdate = chrono::Duration::days(7);
        for (id, login, display) in [
            ("11", "alice", "Alice"),
            ("22", "bob", "Bob"),
            ("33", "carol", "Carol"),
        ] {
            seed_user_file(dir.path(), id, login, display, now - backdate, "body").await;
        }
        seed_user_file(dir.path(), "99", "lurker", "Lurker", now, "body").await;

        let history = Arc::new(Mutex::new(ChatHistoryBuffer::new(
            test_handle(),
            primary_history_capacity,
        )));
        {
            let mut h = history.lock().await;
            h.push_user("alice", "hi");
            h.push_user("bob", "yo");
        }

        let ctx = build_chat_turn_context(
            &store,
            BuildOpts {
                inject_byte_budget: 16 * 1024,
                nonce: "n00000000000000nn".into(),
                primary_history: Some(history),
                primary_login: "chan".into(),
                ai_channel_history: None,
                ai_channel_login: None,
                invocation_channel: InvocationChannel::Primary,
                bot_login: "bot".into(),
                persona_name: "Aurora".into(),
                speaker_login: "carol".into(),
            },
        )
        .await
        .unwrap();

        assert!(
            ctx.durable_memory.contains("login=alice"),
            "alice (chat-window) missing:\n{}",
            ctx.durable_memory
        );
        assert!(
            ctx.durable_memory.contains("login=bob"),
            "bob (chat-window) missing:\n{}",
            ctx.durable_memory
        );
        assert!(
            ctx.durable_memory.contains("login=carol"),
            "carol (speaker) missing:\n{}",
            ctx.durable_memory
        );
        assert!(
            !ctx.durable_memory.contains("login=lurker"),
            "lurker is neither speaker nor in chat window, must be excluded:\n{}",
            ctx.durable_memory
        );

        // Speaker (carol) must appear before the chat-window users so the
        // byte-budget packing loop can never drop the speaker's own file.
        let carol_idx = ctx.durable_memory.find("login=carol").unwrap();
        let alice_idx = ctx.durable_memory.find("login=alice").unwrap();
        let bob_idx = ctx.durable_memory.find("login=bob").unwrap();
        assert!(
            carol_idx < alice_idx && carol_idx < bob_idx,
            "speaker must come first:\n{}",
            ctx.durable_memory
        );
    }
}
