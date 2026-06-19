# AI Prompts

The bot's prompts live as Markdown files in the repo at `crates/core/data/prompts/`, baked into the binary via `include_str!` and used directly at runtime. They are code: the repo is the single source of truth. There is no on-disk override under `$DATA_DIR` — that path was dropped in #321 to end the silent drift between the repo copy and a mutable on-disk copy.

## Files

| File | Used by | Role |
|---|---|---|
| `system.md` | `!ai` chat-turn loop | System prompt for the per-turn LLM session. |
| `ai_instructions.md` | `!ai` chat-turn loop | Preamble prepended to the user message before the relevant-emotes block, volatile state, recent chat, and the new message. |
| `dreamer.md` | nightly ritual | System prompt for the dreamer LLM. |

The chat turn injects `system.md` as the system prompt, then `ai_instructions.md` + speaker metadata + relevant-emotes block + volatile state + recent chat + the new message as the user message. The ritual injects `dreamer.md` as the system prompt, then memory + transcript as the user message.

## Substitution tokens

The loader runs a simple `str::replace` pass before sending. Available tokens:

| Token | Meaning | Available in |
|---|---|---|
| `{speaker_username}` | Twitch login (lowercase) of the speaker | `system.md`, `ai_instructions.md` |
| `{speaker_display}` | Display name (falls back to login) | `system.md`, `ai_instructions.md` |
| `{speaker_user_id}` | Twitch numeric user id of the speaker | `system.md`, `ai_instructions.md` |
| `{speaker_role}` | `regular`, `moderator`, `broadcaster` | `ai_instructions.md` |
| `{channel}` | Channel name (without `#`) | all |
| `{date}` | Today's Berlin-local date, `YYYY-MM-DD` | all |
| `{model}` | Display name. OpenRouter: from provider catalog (normalized). Otherwise: same as `{model_id}`. | `system.md` |
| `{model_id}` | Raw model id sent to the API (`ai.connection.model`). | `system.md` |

Unknown tokens (e.g. typos like `{user_name}`) are left as literal text — no error, no warning. Check spelling.

**Cache prefix constraint.** `{speaker_*}` tokens must not appear in `system.md`: interpolating them would split the stable cache prefix once per speaker role. They belong in `ai_instructions.md` (the user message). `{model}` and `{model_id}` in `system.md` are intentional; model identity changes rarely and is not per-user.

## Authoring guidelines

**Length**. The chat-turn system prompt is sent on every `!ai` invocation, so every byte counts. Aim for ≤2 KiB. The dreamer prompt fires once per day; it can be longer (≤4 KiB).

**Voice**. Write to the model in second person ("you are Aurora"). Describe behavior, not rules. Models follow narrative tone better than bullet lists of "MUST" / "DO NOT".

**Memory model**. Files round-trip as opaque bodies after the YAML frontmatter — the store doesn't enforce any internal structure. The system prompt is the only place that teaches the model what to put in `SOUL.md`, `LORE.md`, `user/<id>.md`, and `state/<slug>.md`. Suggest informal section conventions in prose; don't expect them to be policed.

**Replies**. The model's final assistant text (returned when it makes no more tool calls) is sent to chat verbatim. Newlines collapse into a single chat line. There is no `say` tool — encourage the model to do memory updates first, then end the turn with the reply text.

**Length nudge**. The final reply is truncated app-side at `MAX_RESPONSE_LENGTH` chars. Asking for "≤3 sentences" in the prompt keeps lines tidy.

**Prefix stability (caching).** The chat-turn system message is built to be a byte-stable cache prefix so Gemini's implicit prompt cache hits (cached tokens bill at 0.25x). `build_chat_turn_messages` in `inject.rs` enforces the split: stable content (substituted `system.md`, the tool appendices, and durable SOUL/LORE/user memory) goes in the system message; everything per-turn (the relevant-emotes block, volatile state, recent chat, the instruction) goes in the user message. Two rules keep the prefix stable: do not interpolate per-speaker tokens into `system.md`, and do not append per-turn content to the system message. Both reintroduce a cache miss every turn.

**Refusal**. The bot refuses by returning empty final text — nothing is sent to chat. Encourage the model to stay silent on harassment, off-topic, or low-signal prompts rather than producing a defensive reply.

**Slugs**. State file slugs match `^[a-z0-9][a-z0-9-]{0,63}$`. The prompt should mention this so the model produces valid slugs on the first try.

## Editing flow

1. Edit the file in the repo under `crates/core/data/prompts/`.
2. Commit on a branch, open a PR, merge. The rolling deploy ships it within minutes (see `CLAUDE.md` → Release flow).
3. To roll back, revert the commit and merge again.

There is no live, no-restart edit: the running binary always reflects whatever was last merged. `SOUL.md` is unaffected — it's a runtime-owned memory file under `$DATA_DIR/memories/`, not a prompt template, and keeps its dreamer-driven rewriting.

## Caps and byte budgets

Memory file caps (SOUL 4 KiB, LORE 12 KiB, user 4 KiB, state 2 KiB) are enforced by the store. The auto-injected context (every memory + state file body) is bounded by `inject_byte_budget` (default 24 KiB ≈ 6k tokens) — oldest user/state files drop first. Prompt files are *additional* on top of that — keep them tight.

## See also

- `docs/superpowers/specs/2026-04-28-ai-memory-rework-v2-design.md` — full design.
- `crates/core/data/prompts/*.md` — the prompt templates (source of truth).
