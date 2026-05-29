# Lifecycle announce + `!v` command

**Date:** 2026-05-29
**Status:** Approved (design)

## Goal

Make the running build visible from chat:

1. **Startup announce** — on boot, post one line to the admin channel saying the
   bot is online and which build/commit is running.
2. **Shutdown announce** — on graceful shutdown, post one line saying the bot is
   going offline (it will be back after a redeploy/restart) plus how long it was
   up. This doubles as the "restart" signal: shutdown line out now, startup line
   in when it returns.
3. **`!v` command** — anyone, in any joined channel, can ask for the current
   build/commit and the process uptime.

All three use the same `BUILD_NUM` + short `GIT_SHA` identifiers as the
dashboard sidebar and the `Build info` startup log line, so every surface
agrees on "what is live right now".

## Decisions (from brainstorming)

- **Delivery:** plain chat messages via `client.say(...)` / command reply, *not*
  the `!ping` system.
- **Version identifiers:** `BUILD_NUM` + short `GIT_SHA`.
- **No admin channel configured:** startup/shutdown announces skip silently, log
  at `debug`. No fallback to the primary channel. (`!v` is unaffected — it
  replies in whatever channel invoked it.)
- **Startup timing:** announce *after* handlers are spawned, right before the
  "Bot running" `info!` in `run_bot`.
- **Restart semantics:** no separate restart detection. The graceful-shutdown
  message *is* the restart signal ("offline, brb"); the next startup message is
  the "back online".
- **`!v` scope:** any joined channel, replies to the invoking user (like `!lb`).
  Note: the existing dispatcher already restricts *all* commands in
  `admin_channel` to the broadcaster — `!v` inherits that, no extra work.
- **Uptime format:** compact, largest two–three non-zero units, e.g.
  `2d 3h 14m`, `5m 12s`, `42s`.

### Message text

- Startup: `I'm up KOK · b{BUILD_NUM} · {GIT_SHA_SHORT}`
  (e.g. `I'm up KOK · b1234 · a1b2c3d`).
- Shutdown: `Bravo 6 going dark RatgeNV` (static — no build/sha/uptime).
- `!v` reply: `billyReady · b{BUILD_NUM} · {GIT_SHA_SHORT} · up {uptime}`
  (e.g. `billyReady · b1234 · a1b2c3d · up 2d 3h 14m`).

## Architecture

Everything lives in the `core` crate so integration tests exercise the real
paths through `run_bot` / the command dispatcher without the bin crate.

### 1. Build-info env in `core` — `crates/core/build.rs` (new)

`core` has no build script today. Add one mirroring `git_sha()` / `build_num()`
from `crates/web/build.rs` (omit web's asset hashing):

- `GIT_SHA_SHORT`: `GIT_SHA` env (first 7 chars) if set & non-empty, else
  `git rev-parse --short=7 HEAD`, else `"unknown"`.
- `BUILD_NUM`: `BUILD_NUM` env if set & non-empty, else `"dev"`.

Emit `cargo:rustc-env=GIT_SHA_SHORT=…`, `cargo:rustc-env=BUILD_NUM=…`, plus
`rerun-if-env-changed` for both and `rerun-if-changed=../../.git/HEAD`.

The Dockerfile already promotes the `GIT_SHA` / `BUILD_NUM` build-args to `ENV`
(Dockerfile lines 34–41) before `cargo build`, so this script sees them; local
`cargo` runs fall back to git / `"dev"`. `core` reads the values via `env!()`;
nothing is duplicated into `Services`.

### 2. Announce module — `crates/core/src/twitch/announce.rs` (new)

Shared helpers + the two lifecycle announce fns:

```rust
/// `b{BUILD_NUM} · {GIT_SHA_SHORT}` — baked at compile time.
pub fn version_line() -> String {
    format!("b{} · {}", env!("BUILD_NUM"), env!("GIT_SHA_SHORT"))
}

/// Compact uptime: largest two–three non-zero units (d/h/m/s).
pub fn format_uptime(d: Duration) -> String { /* … */ }

pub async fn announce_startup<T, L>(client: &TwitchIRCClient<T, L>, admin_channel: Option<&str>)
where T: Transport, L: LoginCredentials {
    let Some(ch) = admin_channel.map(str::trim).filter(|c| !c.is_empty()) else {
        debug!("No admin_channel configured; skipping startup announce");
        return;
    };
    client.say(ch.to_owned(), format!("I'm up KOK · {}", version_line())).await;
}

pub async fn announce_shutdown<T, L>(client: &TwitchIRCClient<T, L>, admin_channel: Option<&str>)
where T: Transport, L: LoginCredentials {
    let Some(ch) = admin_channel.map(str::trim).filter(|c| !c.is_empty()) else {
        debug!("No admin_channel configured; skipping shutdown announce");
        return;
    };
    client.say(ch.to_owned(), "Bravo 6 going dark RatgeNV".to_owned()).await;
}
```

- Register `pub mod announce;` in `crates/core/src/twitch/mod.rs`.
- `format_uptime` is a pure fn → unit-tested directly (no IRC needed).

### 3. `!v` command — `crates/core/src/commands/version.rs` (new)

```rust
pub struct VersionCommand { started_at: Instant }

#[async_trait]
impl<T, L> Command<T, L> for VersionCommand {
    fn name(&self) -> &str { "!v" }
    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let msg = format!(
            "billyReady · {} · up {}",
            announce::version_line(),
            announce::format_uptime(self.started_at.elapsed()),
        );
        ctx.sender.reply(ctx.privmsg, &msg).await;
        Ok(())
    }
}
```

- Register `pub mod version;` in `crates/core/src/commands/mod.rs`.
- Push `VersionCommand` into `cmd_list` in `commands.rs` among the always-on
  (non-AI-gated) commands. Always enabled; default exact-match on `!v`.

### 4. Process start + wiring

Capture the start instant once at the top of `run_bot`
(`let started_at = Instant::now();`) — close enough to process start that the
sub-second skew vs. `main` is irrelevant for an uptime readout.

Thread it to two consumers:

- **`!v`:** add `started_at: Instant` to `SpawnDeps`
  (`crates/core/src/twitch/handlers/spawn.rs`) → forward into
  `CommandHandlerConfig` (`commands.rs`) → `VersionCommand::new(started_at)`.
- **shutdown announce:** `run_bot` keeps a cloned `client` `Arc` + the
  `admin_channel` `Option<String>` (read from
  `settings.load().twitch.admin_channel`) for use after `await_shutdown`. The
  static shutdown line needs no `started_at`.

In `run_bot`:

```rust
let started_at = Instant::now();
let client_for_announce = client.clone();              // Arc clone before move
let admin_channel = settings.load().twitch.admin_channel.clone();
// … spawn_handlers(SpawnDeps { started_at, client, … }) …
announce::announce_startup(&client_for_announce, admin_channel.as_deref()).await; // after spawn, before "Bot running" info!
// … existing info!("Bot running …") …
let ping_actor_handle = await_shutdown(handlers, shutdown).await;
announce::announce_shutdown(&client_for_announce, admin_channel.as_deref()).await;
drop(ping_actor_tx);
// … existing web/ping-actor drain …
```

The connection is verified and `admin_channel` joined in
`setup_and_verify_twitch_client` (called by the bin before `run_bot`), so both
`say`s land in a joined channel.

## Data flow

```
build-args GIT_SHA/BUILD_NUM ─► ENV ─► core/build.rs ─► env!() ─► announce::version_line()
                                                                       ├─ announce_startup  (run_bot, after spawn)
                                                                       ├─ announce_shutdown (run_bot, after await_shutdown)
                                                                       └─ VersionCommand    (!v reply)
Instant::now() @ run_bot ─► started_at ─► SpawnDeps ─► CommandHandlerConfig ─► VersionCommand (!v uptime)
settings.load().twitch.admin_channel ─► admin_channel ─► both announces (startup + shutdown)
```

## Error handling

- No admin channel → `debug!`, no send (startup & shutdown).
- `client.say` is fire-and-forget like every other call site; the IRC client
  logs send errors. A failed announce must never abort startup or block
  shutdown.
- **Shutdown flush:** the announce runs *before* the existing web/ping-actor
  drain (up to ~5.5s), giving the client's sender task time to flush the line
  before the process exits. Best-effort — a dropped final message on a crashing
  process is acceptable.

## Testing

Integration tests via the existing `TestBot` harness (`crates/core/tests/`):

- **Startup announce** (`announce.rs` or extend an existing file): `SettingsStore`
  with `twitch.admin_channel = "adminchan"` → `run_bot` →
  `expect_say_full(timeout)`; assert channel == `adminchan` and body starts with
  `I'm up KOK · b`. (Version values are `dev`/`unknown`/local-git in the test
  build — assert on the stable prefix/shape, not exact numbers.)
- **`!v` command:** inject a `!v` PRIVMSG from a normal user in the primary
  channel → `expect_say`/`expect_reply`; assert body starts with
  `billyReady · b` and contains ` · up `.
- **`format_uptime` unit tests** (in `announce.rs`): table of
  `Duration → expected string` covering seconds-only, minutes, hours, multi-day,
  and the two/three-unit truncation rule.
- **Shutdown announce:** if the harness can trigger graceful shutdown and still
  read the outgoing line, assert the body == `Bravo 6 going dark RatgeNV` on
  `adminchan`. If shutdown teardown makes the capture racy, note the gap rather
  than adding a flaky test (the line is a static string, low risk).

## Files

| File | Change |
|---|---|
| `crates/core/build.rs` | **new** — bake `GIT_SHA_SHORT` + `BUILD_NUM` |
| `crates/core/src/twitch/announce.rs` | **new** — `version_line`, `format_uptime`, `announce_startup`, `announce_shutdown` + unit tests |
| `crates/core/src/twitch/mod.rs` | add `pub mod announce;` |
| `crates/core/src/commands/version.rs` | **new** — `VersionCommand` (`!v`) |
| `crates/core/src/commands/mod.rs` | add `pub mod version;` |
| `crates/core/src/twitch/handlers/commands.rs` | add `started_at` to `CommandHandlerConfig`; push `VersionCommand` into `cmd_list` |
| `crates/core/src/twitch/handlers/spawn.rs` | add `started_at: Instant` to `SpawnDeps`; forward to `CommandHandlerConfig` |
| `crates/core/src/lib.rs` | capture `started_at`, clone client + read `admin_channel`, call `announce_startup` (after spawn) and `announce_shutdown` (after `await_shutdown`) |
| `crates/core/tests/announce.rs` | **new** — startup + `!v` integration tests |
| `CLAUDE.md` | add `!v` to the command list in the Project section |

## Out of scope

- Persisted restart detection (cold-start vs redeploy) — the shutdown→startup
  pair covers it.
- Sharing build.rs logic between `core` and `web` (small duplication accepted).
- Any change to the `!ping` system or ping templates.
- Real uptime in `/healthz` (it currently only reports the `irc_connected`
  flag; out of scope here).
