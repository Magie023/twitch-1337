# ADR-0001: On-demand emote lookup tool over a dedicated emote sub-agent

**Status:** Accepted (2026-06-18)
**Relates to:** #184

## Context

The AI persona has a large baked 7TV emote glossary (~982 entries), but only a
small per-turn window reaches the model through the prompt block built by the
emote provider (scored recent-chat/instruction matches + glossary baseline fill,
capped by `max_prompt_emotes`). The model never sees the long tail, and the
persona prompt names "reflex" emotes that aren't guaranteed to be in the window
while the block forbids inventing codes — so the persona is told to use emotes it
may not be handed.

Two ways to give the model breadth were considered:

1. A **dedicated emote-enrichment sub-agent** — a second LLM call per `!ai` turn
   (or a post-draft pass) that ranks emotes and returns the best matches.
2. An **on-demand `search_emotes(query)` tool** added to the existing single-loop
   agent, called only when the model wants an emote it wasn't handed.

## Decision

Use the tool, not the sub-agent. Breadth is delivered by:

- a **pinned core set** always present in the per-turn window (the persona
  reflexes), plus the existing scored/baseline layers, and
- a **`search_emotes(query)` tool** registered in the existing chat-turn tool list
  when the emote provider is active, reusing the existing term-scorer to rank the
  full available set.

## Consequences

- **+** No second LLM call per turn. The sub-agent would tax tokens on *every*
  `!ai` turn (a concern raised in #184: don't spend as much choosing emotes as
  writing the reply); the tool costs nothing on turns that don't call it.
- **+** Smallest diff — reuses the existing agentic loop, tool-registration
  pattern, and emote scorer rather than building and maintaining a new agent.
- **−** Long-tail emotes surface only when the model chooses to call the tool;
  there is no automatic enrichment of every reply.
- **−** The pinned core set is manually curated. It is owner-controlled config,
  not auto-derived from the persona prompt, because the persona text is rewritten
  by automated rituals — auto-derivation would make the pinned set drift. The
  owner keeps the persona's reflex emotes a subset of the pinned set by hand.

If a post-draft enrichment pass ever proves necessary, it can be added later
without unwinding this decision; the tool and pinned set stand on their own.
