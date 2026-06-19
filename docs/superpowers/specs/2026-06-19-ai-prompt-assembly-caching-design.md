# AI prompt assembly + Gemini prefix caching

Issue: #101 ("review caching of ai"). Date: 2026-06-19.

## Problem

Prod routes `!ai` to **Gemini 3.0+**. Gemini 2.5/3.x do **implicit** prompt
caching automatically (no `cache_control`, 0.25× cost on the cached prefix;
min 1024 tok Flash / 4096 tok Pro). Our system message (system.md ~2 KB +
~24 KB durable memory ≈ 6k tokens) clears the threshold, so the discount is
free *if the prefix is byte-stable across requests*. It isn't, so today we get
zero benefit while paying full price for a 6k-token system message every turn —
including on every tool round within a turn (`run_agent` is a multi-round loop).

Three things break the prefix, only one of which #101 names (the issue predates
the emote feature and the current `system.md`):

1. **Per-turn emote block in the *middle* of the system message.**
   `command.rs:445` does `system_prompt_head.push_str(&emote_block)` where the
   block is computed per-turn from this instruction + recent chat, *before*
   `durable_memory`. Every turn the prefix diverges mid-message, invalidating
   the cache for everything after it (appendices + all durable memory). Biggest
   killer; a pure ordering bug.
2. **Per-turn nonce in every fence** (`command.rs:420` → `inject.rs:70`).
   SOUL/LORE/user fence headers carry `nonce=fresh_nonce()`, so the durable
   block's bytes change every turn even when the content is identical.
3. **`{speaker_role}` substituted into `system.md:86`** → a 3-way prefix split
   (regular/mod/broadcaster). #101's "killer 1" is otherwise already fixed —
   `{speaker_username}` is gone from `system.md`.

Root cause of class (1): the system/user messages are assembled by ~90 lines of
ad-hoc `push_str` in the command handler with no notion of stable-vs-volatile,
so volatile content can land anywhere. That's the thing to revise.

## Goals

- Make the `!ai` system-message prefix byte-stable across turns (and across tool
  rounds within a turn) so Gemini implicit caching engages.
- Consolidate chat-turn message assembly into one tested seam where
  "stable → system message, volatile → user message" holds by construction.

## Non-goals (explicitly out of scope)

- **`cache_control` breakpoints / `llm::Message` changes.** Gemini does implicit
  caching for free; explicit breakpoints would need surgery on `llm::Message`
  (plain `String` content today) for no gain on this provider. Revisit only if
  prod ever routes to a provider without implicit caching (e.g. Anthropic).
- **A general prompt-segment framework.** There are two message slots; the
  invariant is a one-line rule enforced by the builder + a test, not an
  abstraction.
- **The dreamer.** `ritual.rs` has its own assembly and runs once/day with
  nothing to cache. Untouched. It keeps `fresh_nonce()`.

## Design

### 1. `build_chat_turn_messages` (new, in `inject.rs`)

Lift the system/user assembly out of `command.rs` into one function that owns
both messages. The system message accepts **only stable inputs**; volatile
inputs can only reach the user message — the emote block physically cannot land
in the system prefix.

```rust
/// Stable parts of the chat-turn system message. Byte-identical across turns
/// for a given config + memory snapshot, so Gemini implicit caching engages.
pub struct SystemParts {
    pub head: String,            // substituted system.md (no speaker vars)
    pub appendices: Vec<String>, // emote-tools / grok / web — stable per mode
    pub durable_memory: String,  // SOUL/LORE/users, stable-nonce fences
}

/// Per-turn (volatile) parts. All go in the user message.
pub struct UserParts {
    pub emote_block: Option<String>, // relevant-emotes list for this turn
    pub volatile_state: String,      // state/<slug> fences
    pub recent_chat: String,
    pub instructions: String,        // substituted ai_instructions.md
    pub instruction: String,         // user instruction (+ reply context)
}

/// system = head + appendices + durable_memory (stable order).
/// user   = emote_block? + volatile_state? + recent_chat? + instructions + instruction.
pub fn build_chat_turn_messages(sys: SystemParts, user: UserParts) -> (Message, Message);
```

`command.rs` shrinks to: substitute templates, gather appendices, call
`build_chat_turn_context`, hand the pieces to `build_chat_turn_messages`. The
emote block moves from `sys` to `user`.

Per-mode note: the grok/web/emote-tools appendices differ between `!ai`, `@grok`,
and web-on/off. Each mode is its own stable prefix and caches within itself; the
common case (`!ai`, web on) is one stable prefix. Not worth unifying.

### 2. Emote block → user message + appendix reword

The per-turn emote block becomes `UserParts.emote_block`. Reword
`EMOTE_TOOLS_SYSTEM_APPENDIX` in `command.rs`: it says emotes are "listed in the
system prompt," which stops being true — change to refer to the emotes provided
in the turn context.

### 3. Per-process stable nonce

Add a `prompt_nonce: String` field to `AiCommand`, set once in `AiCommand::new`
via `fresh_nonce()`. Pass it into `BuildOpts.nonce` instead of calling
`fresh_nonce()` per turn. Effect: content-equality ⟹ byte-equality, so the
durable block's cache only breaks when memory actually changes (a write or the
nightly dreamer rewrite) — which is correct. The nonce rotates on
restart/deploy, which is fine and keeps it unpredictable across runs.

Security: low-stakes. `scrub_for_inject` (`inject.rs:106`) already rejects any
memory body containing the bare `<<<FILE` / `<<<ENDFILE` sentinel regardless of
nonce, so a body can never forge a close marker. The per-turn nonce was
belt-and-suspenders on top of that structural guard; a per-process nonce keeps
the structure intact.

### 4. `system.md` — drop the `{speaker_role}` interpolation

Reword line 86 so it points at the `role=` attribute in the user-message speaker
marker (already emitted by `ai_instructions.md`) instead of interpolating
`{speaker_role}`. After this, `system.md` interpolates only `{channel}` (constant
for the bot's single channel) → identical system prefix for every user.
`{date}` and all `speaker_*` tokens already live in `ai_instructions.md` (user
message), so nothing is lost.

## Testing

- **New regression test** (`inject.rs`): `build_chat_turn_messages` produces a
  byte-identical system `Message` across two calls with the same `SystemParts`
  and differing `UserParts` (different emote block, instruction, recent chat).
  This is the guard that keeps volatile content out of the prefix.
- **Nonce stability test**: two `build_chat_turn_context` calls with the same
  nonce + unchanged store yield byte-identical durable memory.
- Existing `inject.rs` tests (substitution, fence rendering, durable/volatile
  routing, scrub) and the `bundled_system_substitutes_cleanly` test stay green;
  the latter is updated for the dropped `{speaker_role}`.

## Verification after deploy

Implicit caching can be finicky on multi-round tool loops on some providers
(observed elsewhere). After shipping, confirm cache hits via OpenRouter usage
accounting (cached-token counts > 0 on the 2nd+ round of a turn and on
back-to-back turns). If hits don't appear, that — not more code — is the next
investigation. No `cache_control` work is pre-emptively added.

## Files touched

- `crates/core/src/ai/memory/inject.rs` — new `build_chat_turn_messages` +
  `SystemParts`/`UserParts`, tests.
- `crates/core/src/ai/command.rs` — call the builder; `prompt_nonce` field;
  emote block to user parts; appendix reword.
- `crates/core/data/prompts/system.md` — reword line 86.
- `docs/ai-prompts.md` — note the stable-prefix invariant + that `system.md` no
  longer interpolates `{speaker_role}`.
