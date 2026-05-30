# Web E2E Testing — Design

**Date:** 2026-05-30
**Status:** Approved (brainstorming) — pending implementation plan
**Branch:** `feature/web-e2e-testing`

## Problem

The dashboard (`crates/web`) has ~124 HTTP-level integration tests
(`tower::oneshot` against the built `axum::Router`, asserting on rendered
HTML). Route logic and server-rendered markup are well covered. Nothing
above HTTP is exercised:

- htmx attribute wiring (`hx-target` / `hx-swap` / `hx-post`) — a template
  with a wrong target compiles, renders, passes every HTTP test, and is
  still broken in the browser.
- client-side JS (`app.js`) — never executed.
- real DOM swaps, form submit flows, redirect-then-render.
- CSS / visual state (out of scope here; see Non-Goals).

We want **browser-level e2e tests** that drive a real browser against a
running dashboard and assert on post-interaction DOM state.

## Goals

- Real browser drives the dashboard: navigate, click, submit, observe htmx
  swaps and JS effects.
- Cover four representative flows (below), prioritising the htmx swap
  mechanics that HTTP tests cannot see.
- Stay in the Rust toolchain (`cargo nextest`) — no Node sidecar.
- Run in CI as a **non-blocking** job (visible, not merge-gating) while the
  suite matures.
- Reuse the existing test auth path — no new production code, no OAuth.

## Non-Goals

- Visual / screenshot regression testing (separate concern, future work).
- Mirroring every route at the browser level — HTTP tests stay the
  workhorse; e2e covers interaction mechanics on representative flows.
- Making e2e a required status check (day one). Promote later once stable.
- Testing the full bot (IRC/Helix) — dashboard only, `StubHelix`.

## Driver Choice

**fantoccini** `0.22` (latest `0.22.1`, 2026-02-28) — pure-Rust WebDriver
client — driving **chromedriver + Chrome for Testing**.

- Rust-native: e2e tests are ordinary `cargo` integration tests, reuse the
  existing `tests/helpers` fixtures, no second toolchain in a musl/Rust
  repo.
- Chrome (not Firefox/geckodriver) to match the existing
  `chromium --headless` pattern already in the `Justfile` screenshot recipe.
- Rejected: Playwright (drags Node + separate runner into a pure-Rust
  repo); raw CDP/headless-chromium (hand-rolled waits, brittle for
  interaction testing).

## Authentication — Key Decision

No `/_dev/login` route and **no `dev-login` feature** are needed.

`crates/web/tests/helpers/mod.rs` already exposes `sign_for_tests(state,
name, value)`, which round-trips a value through a real `tower_cookies`
signed jar using `state.signed_key` (fixed `[0x42; 64]` in tests) and
returns the exact signed cookie string the production `Set-Cookie` path
emits. `insert_session_as(state, ...)` already mints a `Mod` session in the
`SessionTable`.

The browser is authenticated by injecting those signed cookies directly via
WebDriver `add_cookie`:

1. Insert a `Mod` session → get `(signed_sid, signed_csrf, bare_csrf)`.
2. Navigate the browser to the origin once (cookies require being on the
   domain).
3. `add_cookie("tw1337_sid", signed_sid)` and
   `add_cookie("tw1337_csrf", signed_csrf)`.
4. Navigate to the target page — handlers see valid signed cookies and the
   sliding mod-recheck admits the session (`StubHelix::is_moderator` is true
   for the dev user).

This reuses the exact path the existing auth tests rely on; if signing ever
changes, both break together.

## Architecture

### Harness — `crates/web/tests/e2e/harness.rs`

A single module the e2e test target includes. Responsibilities:

- **Spawn server.** Build `WebState` via the existing helper builder
  (StubHelix, `tempfile::TempDir` data dir, fixed `signed_key`). Call
  `build_router(state)`, bind `TcpListener` to `127.0.0.1:0` (ephemeral),
  spawn `serve_app(listener, app, shutdown)` on a tokio task. Return the
  bound `http://127.0.0.1:<port>` base URL and a shutdown `Arc<Notify>`.
- **Connect browser.** `fantoccini::ClientBuilder` against `$WEBDRIVER_URL`
  (default `http://localhost:9515`). Chrome caps:
  `--headless=new`, `--no-sandbox`, `--disable-gpu`,
  `--window-size=1400,900`.
- **Authenticate.** As above. Expose
  `async fn session() -> E2eSession { client, base_url, _shutdown,
  _tempdir }`. `Drop` / explicit teardown closes the fantoccini session and
  notifies shutdown.
- **Wait helpers.** Thin wrappers over `client.wait().for_element(Locator)`
  / `for_element_gone` so flows never `sleep`.

### Test target — `crates/web/tests/e2e.rs`

One file, four `#[tokio::test]` flows, each: build `E2eSession`, drive the
flow, assert DOM, teardown. The test target is gated (see Gating) so it is
neither compiled nor run by the default workspace test command.

### Gating — `crates/web/Cargo.toml`

```toml
[features]
e2e = []           # compile-gate for the e2e test target only

[dev-dependencies]
fantoccini = "0.22"

[[test]]
name = "e2e"
path = "tests/e2e.rs"
required-features = ["e2e"]
```

`required-features` makes cargo **skip** the e2e target unless `--features
e2e` is passed. Because `fantoccini` is used only by that target, the
default `cargo nextest run --workspace` (the required `test` CI job) does
not build fantoccini or the e2e tests — keeping the required gate fast and
driver-free. The e2e job opts in with `--features e2e`.

`helpers/mod.rs` already has `#![allow(dead_code)]`; the e2e module includes
it the same way the other test binaries do (`mod helpers;`). Any helper the
e2e module needs that is currently private gets `pub`.

## Data Flow (per test)

```
helper builder ──► WebState {StubHelix, TempDir, signed_key=[0x42;64]}
                        │
                  build_router(state)
                        │
        TcpListener 127.0.0.1:0  ──►  serve_app (tokio task)
                        │                       ▲ shutdown Notify
                   base_url ──────────┐         │
                                      ▼         │
   fantoccini Client ($WEBDRIVER_URL) ─ navigate base_url
        │                                       │
   add_cookie(signed sid + csrf)  ◄── sign_for_tests + insert_session_as
        │
   navigate flow ─► click / fill / submit ─► wait_for DOM ─► assert
        │
   teardown: client.close() + shutdown.notify()
```

### Mechanism note (from template audit)

Most dashboard mutations are **plain `<form method="post">` → redirect**, not
htmx swaps. Only **ping delete** and **member remove** are htmx
(`hx-post` + `hx-swap="outerHTML"`, gated by `hx-confirm` → `window.confirm`).
Schedule add/toggle/delete and settings save are full form POSTs. The e2e
value still holds — a real browser exercises form submit → redirect →
re-render, the JS layer (`app.js`, htmx), and the two htmx swaps — but tests
are written as form-nav + dialog handling, not swap-watching everywhere. The
browser carries CSRF automatically: the server renders the `_csrf` token (and
htmx `X-Csrf-Token` header) into each page, so the harness injects only the
two session cookies. `unhandledPromptBehavior: "accept"` in the WebDriver
caps auto-accepts the confirm dialogs.

## Flows (4)

1. **Auth + nav smoke.** Inject cookies → load `/` (or `/pings`) →
   dashboard renders, sidebar present → click each sidebar nav link →
   target page loads (no auth bounce). Proves harness + cookie-auth +
   routing end-to-end in a real browser.
2. **Pings CRUD swaps.** `/pings`: submit new-ping form → new row appears
   via htmx swap; edit → row updates in place; add member / remove member →
   partial swaps reflect membership; delete → row removed (outerHTML swap to
   empty). The richest htmx surface.
3. **Schedules toggle/CRUD.** `/schedules`: add schedule, toggle `enabled`
   (htmx) and confirm state flips, edit, delete. Exercises toggle swap +
   inline validation feedback.
4. **Settings save.** `/settings`: change a field, submit, confirm the
   persisted value renders back and a "restart required" badge appears for a
   restart-gated field.

## Error Handling & Flake Control

- **No sleeps.** All synchronisation via fantoccini explicit waits
  (`for_element` / `for_element_gone`) with a bounded timeout.
- **Isolation.** Each test gets its own server on an ephemeral port and its
  own browser session + `TempDir` data dir. No shared state, no port
  collisions.
- **Driver absence.** The e2e target only builds under `--features e2e`; the
  CI e2e job and `just e2e` guarantee a chromedriver is up. If
  `$WEBDRIVER_URL` is unreachable when the suite runs, fail fast with a
  clear message (not a hang).
- **Teardown.** Always close the fantoccini session and notify server
  shutdown, even on assertion failure (RAII guard in `E2eSession`).

## CI — `.github/workflows/ci.yml`

New **non-blocking** `e2e` job (NOT added to required status checks),
mirroring the `test` job's toolchain/cache setup, plus browser setup via
`browser-actions/setup-chrome` (latest `v2.1.2`, 2026-05-06). That single
action installs Chrome **and** a matched chromedriver (Chrome for Testing),
eliminating the version-mismatch `SessionNotCreatedException` class of
failure:

```yaml
  e2e:
    name: e2e
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      # ... checkout + read-toolchain + dtolnay/rust-toolchain + rust-cache
      #     (copied verbatim from the `test` job) ...
      - id: chrome
        uses: browser-actions/setup-chrome@<pin-to-v2.1.2-SHA> # zizmor: ignore[unpinned-uses] if tag-pinned
        with:
          chrome-version: '135'          # pin: exact M.m.b.p for full determinism, or major
          install-chromedriver: true
          install-dependencies: true
      - uses: taiki-e/install-action@nextest # zizmor: ignore[unpinned-uses]
      - name: Start chromedriver
        run: |
          "${{ steps.chrome.outputs.chromedriver-path }}" --port=9515 &
          # brief readiness wait on :9515 before tests connect
      - name: cargo nextest (e2e)
        env:
          WEBDRIVER_URL: http://localhost:9515
        run: cargo nextest run -p twitch-1337-web --features e2e
```

Pinning notes:
- `chrome-version` accepts an exact build (`135.0.7049.84`) or a major
  (`135`). Prefer an exact build for full reproducibility; the matched
  chromedriver is guaranteed either way by `install-chromedriver: true`.
- `browser-actions/setup-chrome` downloads + executes browser binaries.
  It is not in the CLAUDE.md security-critical SHA-pin list, but pinning it
  to the `v2.1.2` commit SHA (with a version comment) is the safer default
  for an action that fetches executables. The plan pins the concrete SHA and
  fills the exact `chrome-version` build current at implementation time.
- `chromedriver-path` output feeds the start step; `WEBDRIVER_URL` points
  fantoccini at it.

## Local — `Justfile`

`just e2e` recipe: ensure chromedriver running on `:9515`, then
`cargo nextest run -p twitch-1337-web --features e2e`.

## Files Touched

| File | Change |
|---|---|
| `crates/web/tests/e2e.rs` | new — 4 flow tests |
| `crates/web/tests/e2e/harness.rs` | new — server spawn + browser + auth |
| `crates/web/Cargo.toml` | `e2e` feature, `fantoccini` dev-dep, `[[test]]` target |
| `crates/web/tests/helpers/mod.rs` | expose any needed private helper as `pub` |
| `.github/workflows/ci.yml` | non-blocking `e2e` job |
| `Justfile` | `just e2e` recipe |

No production source in `crates/web/src/` changes.

## Resolved (research, 2026-05-30)

- **fantoccini version** → `0.22` (latest `0.22.1`, 2026-02-28). Speaks
  WebDriver; works with chromedriver.
- **CI chromedriver pinning** → `browser-actions/setup-chrome@v2.1.2` with
  `chrome-version` pinned + `install-chromedriver: true`. The action installs
  Chrome and a matched chromedriver together (Chrome for Testing), so the
  match is guaranteed and the `SessionNotCreatedException` mismatch class is
  designed out. `chromedriver-path` output launches the driver on `:9515`.

## Open Questions / Risks

- **Exact pins at implementation time.** Plan fills the concrete
  `browser-actions/setup-chrome` commit SHA and the exact `chrome-version`
  build current then.
- **htmx swap timing.** `for_element_gone` after a delete-swap is the
  reliable signal; verify each flow's swap emits a DOM change waitable
  without a fixed delay.
