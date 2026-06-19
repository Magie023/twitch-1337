# ADR-0002: Prompt templates as code, not on-disk overrides

**Status:** Accepted (2026-06-19)
**Relates to:** #321, #101

## Context

The three prompt templates (`system.md`, `ai_instructions.md`, `dreamer.md`) were
`include_str!`'d into the binary, seeded to `$DATA_DIR/prompts/` on first run only
if absent, then always read back from disk at runtime. After first boot the baked-in
constants were dead: the on-disk copy won, the repo copy was neither authoritative
nor synced, and the two drifted silently. Nothing in CI pinned prompt content, and
"what does the bot actually say" was un-answerable from the repo. The supposedly-static
prompt contract was really a mutable file, which also muddied caching reasoning (#101).

## Decision

Treat the prompt templates as code. Use the `include_str!` constants directly at
runtime in both the `!ai` chat-turn loop and the dreamer ritual. Drop the
seed-to-disk loop, the runtime disk reads, and the `$DATA_DIR/prompts/` directory.
The repo (`crates/core/data/prompts/`) is the single source of truth; edits ship via
the rolling deploy on merge.

`{speaker_*}` / `{date}` / `{model}` / `{channel}` substitution is unchanged — that's
injected data, not the template.

## Consequences

- **+** Single source of truth. The repo copy is authoritative; no silent drift, and
  the prompt content is reviewable in PRs and pinned in `git log`.
- **+** Clears the way for #101 — relocating the `## Speaker` block in `system.md`
  becomes a plain code edit with one source of truth.
- **−** Loses live, no-restart prompt editing (templates were re-read per turn).
  Acceptable: this is a single-operator bot with auto-deploy on merge (minutes); the
  live loop bought little against permanent dual-source confusion.
- **n/a** `SOUL.md` is unaffected — it's a runtime-owned memory file
  (`memories/SOUL.md`), not a static prompt, and keeps its dreamer-driven rewriting.
