# CLAUDE.md

Guide Claude Code work repo.

## Project

Rust Twitch IRC bot. Features: 1337 tracker (13:37 Berlin), leaderboard (`!lb`), ping system (`!p`, `!<ping>`), scheduled messages (config.toml), latency monitor, AI (`!ai`, OpenAI/Ollama), flight tracker (`!track`, `!untrack`, `!flight`, `!flights`), aviation lookups (`!up`, `!fl`), feedback (`!fb`), version (`!v`).

Single persistent IRC connection, broadcast channel routes to independent handler tasks.

## Commands

```bash
# Dev
cargo build | cargo run | cargo check | cargo test
RUST_LOG=debug cargo run   # all IRC + handler activity
RUST_LOG=trace cargo run   # every ServerMessage

# Pre-commit (CI gate — required, in order)
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo audit                # optional locally; CI job fails on advisories

# Deploy (Justfile, podman local → docker.homelab SSH)
just build | just push | just restart | just deploy
```

## CI & branch policy

`main` is branch-protected (admin enforced). Direct pushes rejected — use a PR.
Linear history required, force-push + delete blocked, conversations must resolve.

**Branch naming:** `feature/` features, `spec/` specs, `fix/` bug fixes, `refactor/` refactors, `build/` CI / dependencies / Docker. Slug after slash, kebab-case.

**Required status checks (9, must all pass to merge):**

| Check | Workflow | Purpose |
|---|---|---|
| `fmt` | ci.yml | cargo fmt --check |
| `clippy` | ci.yml | cargo clippy --workspace --all-targets -- -D warnings |
| `test` | ci.yml | cargo llvm-cov nextest --workspace (lcov artifact; no threshold gate) |
| `cargo audit` | ci.yml | RustSec CVE scan of Cargo.lock via rustsec/audit-check |
| `hadolint (Dockerfile)` | sast.yml | Dockerfile lint, SARIF → Security tab |
| `trivy config (IaC)` | sast.yml | IaC misconfig scan, HIGH/CRITICAL only |
| `actionlint (workflows)` | sast.yml | Workflow YAML + shell lint |
| `zizmor (workflows)` | sast.yml | Workflow security (injection, perms, pinning) |
| `gitleaks (secrets)` | sast.yml | Full-history secret scan |

`Docker` (build + push `ghcr.io/chronophylos/twitch-1337:latest` and
`:b<run_number>`) triggers on push to `main` with paths-ignore for
docs. Not a required check.
`GHCR retention` runs Sundays 04:00 UTC; prunes old image versions
keeping the last 30 plus `:latest`. Not a required check.
`Data refresh` runs Sundays 03:00 UTC; opens a `chore/data-refresh` PR.

**Native GitHub security (repo settings):** secret_scanning, push_protection,
dependabot_security_updates — all enabled. Push-protection blocks commits
containing known provider tokens at the server.

**Dependabot** (`.github/dependabot.yml`): weekly PRs for `cargo`, `github-actions`,
`docker`. Cargo minor+patch grouped as `rust-minor-patch`; GitHub Actions minor+patch
grouped as `actions-minor-patch`. Docker ecosystem bumps both tag AND sha256 digest in
Dockerfile.

**Action pinning:** security-critical actions pinned to **commit SHA** with version
comment: `rustsec/audit-check`, `gitleaks/gitleaks-action`, `zizmorcore/zizmor-action`,
`actions/delete-package-versions`, `aquasecurity/trivy-action` (Mar 2026 supply-chain
incident — always SHA-pin trivy). Others pinned to major tags; Dependabot keeps them
current.

**Typical PR flow:**
1. branch → commit → push → `gh pr create`
2. wait for 9 checks green; rebase on main if `strict` blocks merge
3. `gh pr merge --squash`

**Release flow (rolling):**
1. Merge a PR into `main`. `docker.yml` triggers on every push to `main`;
   a `dorny/paths-filter` gate (`.github/path-filters.yml`) skips the build
   when only docs, workflows, or other non-image paths changed. Use
   **Actions → Docker → Run workflow** (`workflow_dispatch`) to force a
   rebuild when needed (rollback verification, cache bust).
2. CI builds the musl static binary and pushes the image to
   `ghcr.io/chronophylos/twitch-1337` with tags `latest` and
   `b<github.run_number>` (e.g. `b1234`).
3. The homelab webhook fires on GHCR push; the server pulls `:latest`
   and `docker compose up -d` restarts the bot.
4. The dashboard sidebar and the startup log line `Build info` show
   `BUILD_NUM` + `GIT_SHA` — that is the source of truth for "what is
   running right now".
5. Rollback: `ssh docker.homelab`, edit the compose file to pin
   `:b<N-1>`, `docker compose up -d`. The previous image is still
   present until the weekly GHCR retention prune (keeps last 30).

There are no `vX.Y.Z` tags, no GitHub Releases, no `CHANGELOG.md`. `git
log` on `main` is the release history. The workspace version stays at
`0.0.0` permanently.

**When `cargo audit` fails:**
- Check open Dependabot PRs first (weekly); a bump may already be queued.
- Transitive vulns: `cargo tree -i <crate>` to find the parent; bump the parent.
- If two majors of one crate coexist in Cargo.lock (e.g. rustls-webpki 0.101 + 0.103),
  both must be resolved — usually by bumping the dep pulling in the old major.
- Last resort: ignore in `.cargo/audit.toml` with a written-down reason.

**When Dependabot cargo PRs break `clippy` or `test`:**
- Breaking-API bump; adapt code on the dependabot branch and force-push
  (`git push origin dependabot/cargo/<name>-<ver>`). CI reruns, then merge.
- Example: rand 0.10 moved `.random::<T>()` from `Rng` to `RngExt` — generic bounds
  become `impl rand::RngExt`.

## Config

`config.toml` (copy `config.toml.example`). Bootstrap sections only: `[twitch]` (secrets + channel/username), `[ai]` (api_key only, optional), `[aviationstack]` (api_key only, optional), `[web]` (bootstrap fields only). Schema + defaults in `config.toml.example` — treat as source of truth. Everything else lives in `settings.ron` managed via the dashboard.

`[twitch]` holds OAuth credentials (`refresh_token`, `client_id`, `client_secret`), `channel`, `username`, and `owner` (optional Twitch user ID with full dashboard access). `owner` is bootstrap-only and lives here because it gates the settings page — it must not be editable from the thing it controls. Permission lists (`hidden_admins`, `viewer_allowlist`), channel pointers (`admin_channel`, `ai_channel`), and `expected_latency` live in `settings.ron`, managed via `/settings → Twitch · Permissions` and `Twitch · Channels`. `admin_channel` and `ai_channel` changes require a bot restart. Mods always pass the dashboard auth check; `viewer_allowlist` grants read-only access to non-mods.

`[ai]` carries only the API key. Backend, model, base URL, memory caps,
dreamer schedule, web/emote/media tool toggles, history caps, and every
other runtime knob live in the dashboard (`/settings`, owner only) and
persist to `$DATA_DIR/settings.ron` (schema v2). On first v2 launch any
legacy hoisted `[ai]` keys in `config.toml` are migrated into
`settings.ron` once (sentinel: `$DATA_DIR/.ai_migrated_v2`); subsequent
edits to those legacy keys are ignored.

`[aviationstack]` keeps only `api_key` in config.toml. `enabled`, `base_url`,
and `timeout_secs` live in `settings.ron` (restart-required for connection
changes), managed via `/settings → Aviationstack`.

`[suspend]` is removed from config.toml entirely. `default_duration_secs`
lives in `settings.ron`, managed via `/settings → Suspend`.

`[web]` bootstrap fields (`enabled`, `bind_addr`, `public_url`, `session_secret`)
stay in config.toml; `session_ttl` and `mod_check_refresh` live in `settings.ron`,
managed via `/settings → Web · Sessions`.

On first v3 launch, `[twitch]` permission/channel keys and `[aviationstack]`/`[suspend]`/`[web]`
non-secret keys are migrated into `settings.ron` once (sentinel: `$DATA_DIR/.config_migrated_v3`);
subsequent edits to those legacy keys in config.toml are ignored.

Schedules live in `settings.ron`, managed via `/schedules` (mod-gated). Each
row carries name, message, hh:mm interval, optional ISO 8601 date range, optional
HH:MM active-time window, and an enabled toggle. Validation runs on save
(unique non-blank names, parseable interval, paired active-time fields).
Changes apply within ~30s via a `SettingsHandle` change-`Notify` signal that
the `ScheduleCache` sync task subscribes to. On first launch any legacy
`[[schedules]]` in config.toml are migrated into `settings.ron` once
(separate sentinel: `$DATA_DIR/.schedules_migrated_v3`); subsequent edits to
the legacy section are ignored.

Backend and connection `base_url` changes from the dashboard require a
bot restart (UI shows a "restart required" badge). Everything else
applies live via `SettingsHandle` (model, timeout, reasoning_effort,
behavior limits, history caps, memory byte budgets, dreamer schedule,
web tools, emotes, media). The `GET /settings/ai/models` endpoint
proxies upstream `/v1/models` (OpenAI) or `/api/tags` (Ollama) with a
5-minute TTL cache so the model picker can autocomplete.

OAuth credentials + AI API key wrapped in `SecretString` (secrecy crate). Config structs are `Deserialize`-only; do NOT add `Serialize` derive (closes credential-leak via debug dump).

## Data dir

All runtime-persistent files live under `$DATA_DIR` (default `/var/lib/twitch-1337`, Docker sets `/data`). Use `get_data_dir().join(...)` — never hardcode relative paths. Files: `token.ron`, `pings.ron`, `leaderboard.ron`, `flights.ron`, `feedback.txt`. Memory tree: `memories/SOUL.md`, `memories/LORE.md`, `memories/users/<id>.md`, `memories/state/<slug>.md`, `memories/transcripts/today.md` (line-buffered, rotated nightly), `transcripts/<YYYY-MM-DD>.md`. Prompt overrides: `prompts/system.md`, `prompts/ai_instructions.md`, `prompts/dreamer.md`.

Atomic persistence pattern: write tmp + rename. See `ping.rs`, `memory.rs`, `flight_tracker.rs`.

## Embedded data

`data/plz.csv`, `data/airports.csv`, `data/airlines.csv` baked in via `include_str!`. Zero runtime data-file dep (important for `FROM scratch` musl image).

## Architecture invariants

- All time ops use `Europe/Berlin` (chrono-tz with `CHRONO_TZ_TIMEZONE_FILTER=Europe/Berlin` in `.cargo/config.toml` — only Berlin data compiled in).
- 1337 tracker: messages containing "1337" or "DANKIES" at exactly 13:37:xx. Ignore "supibot", "potatbotat". Dedupe via HashSet (max 10k). Leaderboard = fastest sub-1s PB.
- Broadcast channel capacity 100. Lagging handlers get `RecvError::Lagged`, continue.
- Handlers independent. Errors in one don't crash others (`bail!` on startup only).
- Ping templates: reject control chars on create/edit (CR/LF would split PRIVMSG).
- Aviation client init failure: log + disable `!up`/`!fl`/flight tracker + track commands. Don't abort.
- Latency monitor: PING/PONG every 5min, EMA alpha=0.2, shared `Arc<AtomicU32>`. Read by 1337 handler for precise wake-up.
- Flight tracker: `Arc<mpsc::Sender<TrackerCommand>>` from commands to long task. Adaptive poll 30/60/120s based on phase mix. adsb.lol v2; fallback aggregators in memory `reference_adsb_aggregators.md`.
- AI memory (v2): per-user character sheets + chat LORE + bot SOUL as markdown under $DATA_DIR/memories/. Single-loop !ai turn drives the model with write_file/write_state/delete_state tools (run_agent in the llm crate); the model's final assistant text is sent to chat verbatim. Daily dreamer ritual rewrites files from yesterday's transcript at the dashboard-configured `ai.dreamer.run_at` (Berlin local). Memory bodies are byte-capped (SOUL/user 4 KiB, LORE 12 KiB, state 2 KiB by default; tunable via the dashboard `AI · Memory` card).
- Scheduled messages: list loaded from `settings.ron` into `ScheduleCache`; the `run_schedule_settings_sync` task subscribes to `SettingsStore`'s change-`Notify` and updates the cache when the dashboard saves. The message handler polls the cache every 30s. On Ctrl+C, main notifies `Arc<Notify>`; children finish in-flight `say()` then exit; main awaits handler with 5s timeout.

## Gotchas

- `twitch_irc::irc!` macro needs standalone `use twitch_irc::irc;` — cannot be in braced `use twitch_irc::{...}`.
- Request/response on broadcast (e.g. PING/PONG): subscribe BEFORE send to avoid race.
- Clippy strict (`-D warnings`) + extra lints in `Cargo.toml [lints.clippy]`. Don't `#[allow]` without one-line reason.
- Log errors with `?error` not `%error` to include backtrace.

## Adding handler

Handlers live in `src/twitch/handlers/` and are spawned from `src/lib.rs::run_bot`. Each handler is generic over `<T: Transport, L: LoginCredentials>` so integration tests can swap in a fake transport. Shared deps (clock, data_dir, llm, aviation) come from `Services` in `src/lib.rs`.

```rust
// src/twitch/handlers/my_handler.rs
pub async fn run_my_handler<T, L>(
    broadcast_tx: broadcast::Sender<ServerMessage>,
    client: Arc<TwitchIRCClient<T, L>>,
    /* other deps from Services as needed */
) where T: Transport, L: LoginCredentials { /* subscribe, filter, act */ }

// src/lib.rs::run_bot: spawn + add to the final tokio::select! exit arm
```

Integration-testable via `TestBotBuilder` in `tests/common/`.

## Key constants

`src/twitch/handlers/tracker_1337.rs`: `TARGET_HOUR=13`, `TARGET_MINUTE=37`, `MAX_USERS=10_000`.

`src/twitch/handlers/latency.rs`: `LATENCY_PING_INTERVAL=300s`, `LATENCY_EMA_ALPHA=0.2`.

`src/aviation/tracker.rs`: `MAX_TRACKED_FLIGHTS=12`, `MAX_FLIGHTS_PER_USER=3`, `TRACKING_LOST_THRESHOLD=300s`, `TRACKING_LOST_REMOVAL=1800s`, `POLL_FAST/NORMAL/SLOW=30/60/120s`.

`src/aviation/tracker/debug_journal.rs`: `DEBUG_JOURNAL_KEEP_FILES=30` (daily JSONL files retained under `$DATA_DIR/flight-tracker-debug/`; older pruned on tracker startup + date-rollover).

## Binary / Docker

Musl static build: `cargo build --release --target x86_64-unknown-linux-musl` (~6MB, works on Alpine/busybox, FROM scratch image). rustls not OpenSSL. Multi-stage Dockerfile with cargo-chef.

Verify static: `ldd target/x86_64-unknown-linux-musl/release/twitch-1337` → "statically linked".
