# Lifecycle announce + `!v` command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Post a startup line and a graceful-shutdown line to the admin channel, and add a `!v` command that reports the running build + uptime — all using the same `BUILD_NUM` + `GIT_SHA` identifiers as the dashboard.

**Architecture:** A new `core/build.rs` bakes `BUILD_NUM` + `GIT_SHA_SHORT` into the `core` crate (mirroring `web/build.rs`). A new `twitch::announce` module owns pure message/format helpers plus two thin `client.say` wrappers. A new `VersionCommand` (`!v`) reuses those helpers. `run_bot` captures a process-start `Instant`, threads it to the command via `SpawnDeps`/`CommandHandlerConfig`, and calls the two announces around handler startup/shutdown.

**Tech Stack:** Rust, `twitch_irc`, `async_trait`, `tokio`, `cargo nextest`.

Spec: `docs/superpowers/specs/2026-05-29-startup-admin-announce-design.md`

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/core/build.rs` (new) | Bake `GIT_SHA_SHORT` + `BUILD_NUM` env into `core` |
| `crates/core/src/twitch/announce.rs` (new) | Pure helpers (`version_line`, `format_uptime`, message builders) + `announce_startup`/`announce_shutdown` say-wrappers |
| `crates/core/src/twitch/mod.rs` (modify) | Register `pub mod announce;` |
| `crates/core/src/commands/version.rs` (new) | `VersionCommand` (`!v`) |
| `crates/core/src/commands/mod.rs` (modify) | Register `pub mod version;` |
| `crates/core/src/twitch/handlers/commands.rs` (modify) | Add `started_at` to `CommandHandlerConfig`; push `VersionCommand` into `cmd_list` |
| `crates/core/src/twitch/handlers/spawn.rs` (modify) | Add `started_at` to `SpawnDeps`; forward to `CommandHandlerConfig` |
| `crates/core/src/lib.rs` (modify) | Capture `started_at`; clone client + read `admin_channel`; call both announces |
| `crates/core/tests/announce.rs` (new) | Integration tests: startup announce + `!v` |
| `CLAUDE.md` (modify) | Add `!v` to the command list |

Conventions to follow (observed in repo): import order = std / external / crate (merged braced); commands implement `Command<T,L>` with `#[async_trait]` and reply via `ctx.sender.reply(ctx.privmsg, msg)`; never commit to `main` (we are on a worktree branch already).

---

## Task 1: Build-info baking + pure announce helpers

**Files:**
- Create: `crates/core/build.rs`
- Create: `crates/core/src/twitch/announce.rs`
- Modify: `crates/core/src/twitch/mod.rs:1`

- [ ] **Step 1: Create `crates/core/build.rs`**

Mirrors `crates/web/build.rs` resolution, without the asset hashing.

```rust
//! Bake build-identity env vars into the `core` crate so chat-facing surfaces
//! (startup/shutdown announce, `!v`) report the same BUILD_NUM + GIT_SHA the
//! web dashboard shows. Mirrors `crates/web/build.rs`.

use std::env;
use std::process::Command;

/// Commit SHA: Docker supplies `GIT_SHA` (the `.dockerignore` strips `.git/`);
/// local cargo runs fall back to `git rev-parse`, then `"unknown"`.
fn git_sha() -> String {
    if let Ok(sha) = env::var("GIT_SHA")
        && !sha.is_empty()
    {
        return sha.chars().take(7).collect();
    }
    Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// CI run number: Docker supplies `BUILD_NUM`; local cargo runs fall back to "dev".
fn build_num() -> String {
    env::var("BUILD_NUM")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_owned())
}

fn main() {
    println!("cargo:rustc-env=GIT_SHA_SHORT={}", git_sha());
    println!("cargo:rerun-if-env-changed=GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    println!("cargo:rustc-env=BUILD_NUM={}", build_num());
    println!("cargo:rerun-if-env-changed=BUILD_NUM");
}
```

- [ ] **Step 2: Create `crates/core/src/twitch/announce.rs` with the failing unit tests**

Write the file with the pure helpers AND their tests. (The helpers are tiny; writing them together with tests is the pragmatic TDD unit here — Step 3 runs the tests.)

```rust
//! Build-identity helpers and lifecycle chat announces.
//!
//! `version_line` / `format_uptime` and the three message builders are pure and
//! unit-tested here. `announce_startup` / `announce_shutdown` are thin
//! `client.say` wrappers exercised by integration tests (`tests/announce.rs`).

use std::time::Duration;

use tracing::debug;
use twitch_irc::{TwitchIRCClient, login::LoginCredentials, transport::Transport};

/// `b{BUILD_NUM} · {GIT_SHA_SHORT}` — baked at compile time by `build.rs`.
pub fn version_line() -> String {
    format!("b{} · {}", env!("BUILD_NUM"), env!("GIT_SHA_SHORT"))
}

/// Compact uptime: the first non-zero unit and up to two more non-zero units
/// (largest first), e.g. `2d 3h 14m`, `5m 12s`, `42s`. Zero → `0s`.
pub fn format_uptime(d: Duration) -> String {
    let secs = d.as_secs();
    let parts = [
        (secs / 86_400, "d"),
        ((secs % 86_400) / 3_600, "h"),
        ((secs % 3_600) / 60, "m"),
        (secs % 60, "s"),
    ];
    let mut out: Vec<String> = parts
        .iter()
        .filter(|(v, _)| *v > 0)
        .map(|(v, u)| format!("{v}{u}"))
        .collect();
    out.truncate(3);
    if out.is_empty() {
        "0s".to_owned()
    } else {
        out.join(" ")
    }
}

/// Startup announce body.
pub fn startup_message() -> String {
    format!("I'm up KOK · {}", version_line())
}

/// Graceful-shutdown announce body (static).
pub fn shutdown_message() -> &'static str {
    "Bravo 6 going dark RatgeNV"
}

/// `!v` reply body.
pub fn version_reply_message(uptime: Duration) -> String {
    format!("billyReady · {} · up {}", version_line(), format_uptime(uptime))
}

/// Post the startup line to `admin_channel`. Empty/None channel → skip + debug log.
pub async fn announce_startup<T, L>(client: &TwitchIRCClient<T, L>, admin_channel: Option<&str>)
where
    T: Transport,
    L: LoginCredentials,
{
    let Some(channel) = admin_channel.map(str::trim).filter(|c| !c.is_empty()) else {
        debug!("No admin_channel configured; skipping startup announce");
        return;
    };
    client.say(channel.to_owned(), startup_message()).await.ok();
}

/// Post the shutdown line to `admin_channel`. Empty/None channel → skip + debug log.
pub async fn announce_shutdown<T, L>(client: &TwitchIRCClient<T, L>, admin_channel: Option<&str>)
where
    T: Transport,
    L: LoginCredentials,
{
    let Some(channel) = admin_channel.map(str::trim).filter(|c| !c.is_empty()) else {
        debug!("No admin_channel configured; skipping shutdown announce");
        return;
    };
    client
        .say(channel.to_owned(), shutdown_message().to_owned())
        .await
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uptime_zero_is_seconds() {
        assert_eq!(format_uptime(Duration::from_secs(0)), "0s");
    }

    #[test]
    fn format_uptime_seconds_only() {
        assert_eq!(format_uptime(Duration::from_secs(42)), "42s");
    }

    #[test]
    fn format_uptime_minutes_seconds() {
        assert_eq!(format_uptime(Duration::from_secs(5 * 60 + 12)), "5m 12s");
    }

    #[test]
    fn format_uptime_truncates_to_three_units() {
        // 2d 3h 14m 7s -> drop the 4th (seconds)
        let d = Duration::from_secs(2 * 86_400 + 3 * 3_600 + 14 * 60 + 7);
        assert_eq!(format_uptime(d), "2d 3h 14m");
    }

    #[test]
    fn format_uptime_skips_interior_zeros() {
        // 2d 0h 0m 7s -> only non-zero units kept
        let d = Duration::from_secs(2 * 86_400 + 7);
        assert_eq!(format_uptime(d), "2d 7s");
    }

    #[test]
    fn version_line_shape() {
        let v = version_line();
        assert!(v.starts_with('b'), "got {v}");
        assert!(v.contains(" · "), "got {v}");
    }

    #[test]
    fn startup_message_shape() {
        assert!(startup_message().starts_with("I'm up KOK · b"));
    }

    #[test]
    fn shutdown_message_is_static() {
        assert_eq!(shutdown_message(), "Bravo 6 going dark RatgeNV");
    }

    #[test]
    fn version_reply_shape() {
        let r = version_reply_message(Duration::from_secs(90));
        assert!(r.starts_with("billyReady · b"), "got {r}");
        assert!(r.contains(" · up 1m 30s"), "got {r}");
    }
}
```

Note: `client.say(...).await` returns `Result`; the repo's `ChatSender` swallows errors, but here we call the raw client, so `.ok()` discards the send error (announce is best-effort — must never abort startup/shutdown).

- [ ] **Step 3: Register the module — edit `crates/core/src/twitch/mod.rs`**

Add as the first line (alphabetical):

```rust
pub mod announce;
pub mod chat_sender;
pub mod handlers;
pub mod setup;
pub mod seventv;
pub mod token_storage;
pub mod whisper;
```

- [ ] **Step 4: Run the unit tests — verify they pass**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-core announce`
Expected: PASS (all `announce::tests::*`). If the package name differs, use the name from `crates/core/Cargo.toml` `[package] name`.

- [ ] **Step 5: Commit**

```bash
git add crates/core/build.rs crates/core/src/twitch/announce.rs crates/core/src/twitch/mod.rs
git commit -m "feat: bake build info into core + announce helpers"
```

---

## Task 2: `!v` command

**Files:**
- Create: `crates/core/src/commands/version.rs`
- Modify: `crates/core/src/commands/mod.rs:11-20`

- [ ] **Step 1: Create `crates/core/src/commands/version.rs`**

```rust
use std::time::Instant;

use async_trait::async_trait;
use eyre::Result;
use twitch_irc::{login::LoginCredentials, transport::Transport};

use super::{Command, CommandContext};
use crate::twitch::announce;

/// `!v` — report the running build (BUILD_NUM + GIT_SHA) and process uptime.
pub struct VersionCommand {
    started_at: Instant,
}

impl VersionCommand {
    pub fn new(started_at: Instant) -> Self {
        Self { started_at }
    }
}

#[async_trait]
impl<T, L> Command<T, L> for VersionCommand
where
    T: Transport,
    L: LoginCredentials,
{
    fn name(&self) -> &str {
        "!v"
    }

    async fn execute(&self, ctx: CommandContext<'_, T, L>) -> Result<()> {
        let msg = announce::version_reply_message(self.started_at.elapsed());
        ctx.sender.reply(ctx.privmsg, msg).await;
        Ok(())
    }
}
```

- [ ] **Step 2: Register the module — edit `crates/core/src/commands/mod.rs`**

Add `pub mod version;` to the module list (alphabetical, after `pub mod suspend;`):

```rust
pub mod suspend;
pub mod version;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p twitch-1337-core`
Expected: PASS (no errors). `VersionCommand` is not yet referenced; that is fine — `cargo check` still type-checks the module.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/commands/version.rs crates/core/src/commands/mod.rs
git commit -m "feat: add !v version command"
```

---

## Task 3: Thread `started_at` and register `!v` in the dispatcher

**Files:**
- Modify: `crates/core/src/twitch/handlers/commands.rs` (struct field ~27-58, destructure ~68-90, `cmd_list` ~158)
- Modify: `crates/core/src/twitch/handlers/spawn.rs` (`SpawnDeps` ~63-120, destructure ~130-158, `CommandHandlerConfig` literal ~263-285)

- [ ] **Step 1: Add `started_at` to `CommandHandlerConfig`**

In `crates/core/src/twitch/handlers/commands.rs`, add a field to the struct (after `pub primary_history_tap: ...` at line 57, before the closing brace at 58):

```rust
    pub primary_history_tap: Option<Arc<tokio::sync::Mutex<Option<ChatHistory>>>>,
    /// Process start instant, captured in `run_bot`. Used by `!v` for uptime.
    pub started_at: std::time::Instant,
}
```

- [ ] **Step 2: Destructure it in the handler**

In `run_generic_command_handler`, add `started_at,` to the `CommandHandlerConfig { ... }` destructure (after `primary_history_tap,` at line 89):

```rust
        primary_history_tap,
        started_at,
    } = cfg;
```

- [ ] **Step 3: Push `VersionCommand` into `cmd_list`**

In the `let mut cmd_list: Vec<...> = vec![` literal at line 158, add `VersionCommand` as the first element (it has a unique `!v` trigger; ordering is not critical, but keep it among the always-on commands):

```rust
    let mut cmd_list: Vec<Box<dyn commands::Command<T, L>>> = vec![
        Box::new(commands::version::VersionCommand::new(started_at)),
        Box::new(commands::ping_admin::PingAdminCommand::new(
            ping_handle.clone(),
            settings.clone(),
        )),
```

- [ ] **Step 4: Add `started_at` to `SpawnDeps`**

In `crates/core/src/twitch/handlers/spawn.rs`, add a field to the `SpawnDeps` struct (after `pub telemetry: ...` at line 119, before the closing brace):

```rust
    pub telemetry: Arc<crate::schedule::TelemetryStore>,
    /// Process start instant, captured in `run_bot`; forwarded to `!v`.
    pub started_at: std::time::Instant,
}
```

- [ ] **Step 5: Destructure it in `spawn_handlers`**

Add `started_at,` to the `SpawnDeps { ... }` destructure (after `telemetry,` at line 157):

```rust
        primary_history_tap,
        telemetry,
        started_at,
    } = deps;
```

- [ ] **Step 6: Forward it into `CommandHandlerConfig`**

In the `generic_commands` spawn block, add `started_at,` to the `CommandHandlerConfig { ... }` literal (after `primary_history_tap,` at line 284):

```rust
                emote_provider,
                primary_history_tap,
                started_at,
            })
            .await;
```

- [ ] **Step 7: Verify it compiles (expect a known error in `lib.rs`)**

Run: `cargo check -p twitch-1337-core`
Expected: FAIL — `lib.rs` constructs `SpawnDeps` without the new `started_at` field (`missing field 'started_at'`). This is the next task. Do NOT add it here.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/twitch/handlers/commands.rs crates/core/src/twitch/handlers/spawn.rs
git commit -m "feat: thread started_at to !v via SpawnDeps"
```

---

## Task 4: Wire announces + `started_at` into `run_bot`

**Files:**
- Modify: `crates/core/src/lib.rs` (`run_bot` body ~206-345)

- [ ] **Step 1: Capture start, client clone, and admin_channel before spawning handlers**

In `crates/core/src/lib.rs`, immediately after the `let aviation_for_commands = aviation.clone();` line (≈214), add:

```rust
    // Process start for `!v` uptime + the value passed to SpawnDeps below.
    let started_at = std::time::Instant::now();
    // Cloned Arc + admin channel retained for the startup/shutdown announces;
    // `client` itself is moved into spawn_handlers.
    let client_for_announce = client.clone();
    let admin_channel = settings.load().twitch.admin_channel.clone();
```

- [ ] **Step 2: Pass `started_at` into `SpawnDeps`**

In the `spawn_handlers(SpawnDeps { ... })` call, add `started_at,` to the literal (after `telemetry: telemetry.clone(),` at line 282):

```rust
        primary_history_tap,
        telemetry: telemetry.clone(),
        started_at,
    });
```

- [ ] **Step 3: Call `announce_startup` after spawn, before the "Bot running" log**

Immediately before the `info!("Bot running with continuous connection. ...")` block (line 307), add:

```rust
    crate::twitch::announce::announce_startup(&client_for_announce, admin_channel.as_deref())
        .await;

```

- [ ] **Step 4: Call `announce_shutdown` after `await_shutdown` returns**

The line `let ping_actor_handle = crate::twitch::handlers::spawn::await_shutdown(handlers, shutdown).await;` (≈316) is where shutdown has been initiated. Immediately after it (before `drop(ping_actor_tx);` at 321), add:

```rust
    // Shutdown initiated; best-effort offline line before draining web/ping actor.
    crate::twitch::announce::announce_shutdown(&client_for_announce, admin_channel.as_deref())
        .await;

```

- [ ] **Step 5: Verify the whole crate compiles**

Run: `cargo check -p twitch-1337-core`
Expected: PASS (no errors).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/lib.rs
git commit -m "feat: announce startup + shutdown to admin channel"
```

---

## Task 5: Integration tests (startup announce + `!v`)

**Files:**
- Create: `crates/core/tests/announce.rs`

Reference harness API (from `crates/core/tests/common/`): `TestBotBuilder::new().with_settings(|o| ...).build().await`; `bot.send(user, text)`; `bot.expect_say_full(timeout) -> (channel, body)`; `bot.expect_reply(timeout) -> body`. The settings override field is `o.twitch.admin_channel: Option<Option<String>>`.

- [ ] **Step 1: Write the integration tests**

```rust
mod common;

use std::time::Duration;

use common::TestBotBuilder;

const TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::test]
async fn startup_announce_posts_to_admin_channel() {
    let mut bot = TestBotBuilder::new()
        .with_settings(|o| o.twitch.admin_channel = Some(Some("adminchan".into())))
        .build()
        .await;

    let (channel, body) = bot.expect_say_full(TIMEOUT).await;
    assert_eq!(channel, "adminchan", "startup announce went to wrong channel");
    assert!(
        body.starts_with("I'm up KOK · b"),
        "unexpected startup body: {body}"
    );
}

#[tokio::test]
async fn v_command_replies_with_build_and_uptime() {
    // No admin_channel configured -> no startup announce to race with.
    let mut bot = TestBotBuilder::new().build().await;

    bot.send("someviewer", "!v").await;

    let body = bot.expect_reply(TIMEOUT).await;
    assert!(
        body.starts_with("billyReady · b"),
        "unexpected !v body: {body}"
    );
    assert!(body.contains(" · up "), "missing uptime in !v body: {body}");
}
```

- [ ] **Step 2: Run the new tests — verify they pass**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail -p twitch-1337-core --test announce`
Expected: PASS (2 tests). If `expect_say_full` for the startup test times out, confirm the announce is placed *after* `spawn_handlers` in `run_bot` (Task 4 Step 3) and that the builder applied the admin_channel override.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/announce.rs
git commit -m "test: startup announce + !v integration tests"
```

---

## Task 6: Docs + full CI gate

**Files:**
- Modify: `CLAUDE.md` (Project feature list)

- [ ] **Step 1: Add `!v` to the command list in `CLAUDE.md`**

In the Project section's feature sentence, add `version (`!v`)` to the list. Locate the line listing features (`1337 tracker ... feedback (`!fb`)`) and insert `, version (\`!v\`)` before the closing period.

- [ ] **Step 2: Run the full required gate (in order)**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail
```
Expected: `fmt` clean, `clippy` no warnings, all tests pass.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document !v command"
```

---

## Self-Review notes

- **Spec coverage:** startup announce (Task 4), shutdown announce (Task 4), `!v` (Tasks 2–3), build.rs (Task 1), skip-when-no-admin-channel (Task 1 `announce_*` guards + Task 5 `!v` test runs with no admin channel), uptime format (Task 1 unit tests), CLAUDE.md (Task 6). All covered.
- **Type consistency:** `started_at: std::time::Instant` is the same type at every hop (`run_bot` → `SpawnDeps` → `CommandHandlerConfig` → `VersionCommand::new`). Message builders `startup_message`/`shutdown_message`/`version_reply_message` and `version_line`/`format_uptime` names match between `announce.rs`, `version.rs`, and `lib.rs`.
- **Deliberate intermediate failure:** Task 3 Step 7 leaves the crate uncompilable (`SpawnDeps` missing `started_at` at the `lib.rs` call site); Task 4 fixes it. Tasks 1, 2, 5, 6 each end green.
- **best-effort sends:** raw `client.say(...).await.ok()` in `announce_*` — a send error never propagates.
