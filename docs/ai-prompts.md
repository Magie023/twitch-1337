# AI Prompts

The bot's prompts live as Markdown files under `$DATA_DIR/prompts/`. Edit them live — every invocation reads from disk, so changes are picked up immediately. Defaults are bundled in `crates/twitch-1337/data/prompts/` and seeded on first run if the file is missing.

## Files

| File | Used by | Role |
|---|---|---|
| `system.md` | `!ai` chat-turn loop | System prompt for the per-turn LLM session. |
| `ai_instructions.md` | `!ai` chat-turn loop | Preamble prepended to the user message before chat history + the new message. |
| `dreamer.md` | nightly ritual | System prompt for the dreamer LLM. |

The chat turn injects `system.md` as the system prompt, then `ai_instructions.md` + speaker metadata + chat history + the new message as the user message. The ritual injects `dreamer.md` as the system prompt, then memory + transcript as the user message.

## Substitution tokens

The loader runs a simple `str::replace` pass before sending. Available tokens:

| Token | Meaning | Available in |
|---|---|---|
| `{speaker_username}` | Twitch login (lowercase) of the speaker | `system.md`, `ai_instructions.md` |
| `{speaker_display}` | Display name (falls back to login) | `system.md`, `ai_instructions.md` |
| `{speaker_user_id}` | Twitch numeric user id of the speaker | `system.md`, `ai_instructions.md` |
| `{speaker_role}` | `regular`, `moderator`, `broadcaster` | `system.md`, `ai_instructions.md` |
| `{channel}` | Channel name (without `#`) | all |
| `{date}` | Today's Berlin-local date, `YYYY-MM-DD` | all |

Unknown tokens (e.g. typos like `{user_name}`) are left as literal text — no error, no warning. Check spelling.

## Authoring guidelines

**Length**. The chat-turn system prompt is sent on every `!ai` invocation, so every byte counts. Aim for ≤2 KiB. The dreamer prompt fires once per day; it can be longer (≤4 KiB).

**Voice**. Write to the model in second person ("you are Aurora"). Describe behavior, not rules. Models follow narrative tone better than bullet lists of "MUST" / "DO NOT".

**Memory model**. Files round-trip as opaque bodies after the YAML frontmatter — the store doesn't enforce any internal structure. The system prompt is the only place that teaches the model what to put in `SOUL.md`, `LORE.md`, `user/<id>.md`, and `state/<slug>.md`. Suggest informal section conventions in prose; don't expect them to be policed.

**Replies**. The model's final assistant text (returned when it makes no more tool calls) is sent to chat verbatim. Newlines collapse into a single chat line. There is no `say` tool — encourage the model to do memory updates first, then end the turn with the reply text.

**Length nudge**. The final reply is truncated app-side at `MAX_RESPONSE_LENGTH` chars. Asking for "≤3 sentences" in the prompt keeps lines tidy.

**Refusal**. The bot refuses by returning empty final text — nothing is sent to chat. Encourage the model to stay silent on harassment, off-topic, or low-signal prompts rather than producing a defensive reply.

**Slugs**. State file slugs match `^[a-z0-9][a-z0-9-]{0,63}$`. The prompt should mention this so the model produces valid slugs on the first try.

## Editing flow

1. Edit the file under `$DATA_DIR/prompts/` (e.g. `/var/lib/twitch-1337/prompts/system.md` on the production host).
2. Trigger `!ai` (or wait for the ritual) and observe.
3. To roll back, copy the bundled default from `crates/twitch-1337/data/prompts/` in the repo.

To restore a default: delete the file under `$DATA_DIR/prompts/` and restart. The seed-on-startup logic rewrites the bundled default. (Editing in place and never deleting means the bundled default is never re-applied — owner edits always win.)

## Caps and byte budgets

Memory file caps (SOUL 4 KiB, LORE 12 KiB, user 4 KiB, state 2 KiB) are enforced by the store. The auto-injected context (every memory + state file body) is bounded by `inject_byte_budget` (default 24 KiB ≈ 6k tokens) — oldest user/state files drop first. Prompt files are *additional* on top of that — keep them tight.

## See also

- `docs/superpowers/specs/2026-04-28-ai-memory-rework-v2-design.md` — full design.
- `crates/twitch-1337/data/prompts/*.md` — bundled defaults for the three prompt files.
