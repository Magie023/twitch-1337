# Web E2E Testing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add browser-level e2e tests that drive a real Chrome against an in-process dashboard server, covering four flows (auth/nav smoke, pings CRUD, schedules CRUD/toggle, settings save).

**Architecture:** A `fantoccini` (WebDriver) harness builds the existing test `WebState` (StubHelix + tempdirs + fixed signed key), serves `build_router` on an ephemeral port via a tokio task, connects a headless Chrome, and injects the two signed session cookies (`tw1337_sid` / `tw1337_csrf`) produced by the existing `helpers::insert_session`. The server renders CSRF tokens into each page, so the browser handles CSRF itself. Tests are compile-gated behind an `e2e` Cargo feature so the required `test` job never builds them or `fantoccini`.

**Tech Stack:** Rust, fantoccini 0.22 (rustls), chromedriver + Chrome for Testing, axum, askama, htmx, tokio, nextest.

**Spec:** `docs/superpowers/specs/2026-05-30-web-e2e-testing-design.md`

---

## Prerequisites (local execution)

The e2e tests need a running WebDriver server. Before running any e2e test locally:

```bash
# Install Chrome + matching chromedriver (Arch example; CI uses browser-actions/setup-chrome):
yay -S google-chrome chromedriver   # or: ensure `chromedriver` matches installed Chrome major

# Start chromedriver (leave running in another terminal / background):
chromedriver --port=9515

# Tests read WEBDRIVER_URL (default http://localhost:9515).
```

If `chromedriver` is not running, the e2e tests fail fast with a connect error — that is expected, not a code bug.

The standard suite is unaffected: `cargo nextest run --workspace` does NOT compile or run the e2e target (it is `required-features = ["e2e"]`).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/web/Cargo.toml` | `e2e` feature, `fantoccini` dev-dep, `[[test]]` target gating |
| `crates/web/tests/e2e.rs` | e2e test target entry: `mod harness; mod helpers;` + the 4 flow tests |
| `crates/web/tests/e2e_support/harness.rs` | server spawn + browser connect + cookie auth + wait helpers |
| `crates/web/tests/helpers/mod.rs` | (reused as-is; no change expected) |
| `.github/workflows/ci.yml` | non-blocking `e2e` job |
| `Justfile` | `just e2e` recipe |

> Note on module layout: integration test binaries can only include sibling modules. `tests/e2e.rs` includes `#[path = "e2e_support/harness.rs"] mod harness;` and `mod helpers;` (the existing `tests/helpers/mod.rs`). Using a non-`tests/`-root dir name (`e2e_support/`) avoids cargo trying to compile `harness.rs` as its own test binary.

---

## Task 1: Cargo wiring + gated empty target

**Files:**
- Modify: `crates/web/Cargo.toml`
- Create: `crates/web/tests/e2e.rs`

- [ ] **Step 1: Add the `e2e` feature, fantoccini dev-dep, and test target to `crates/web/Cargo.toml`**

In `[features]` (after the `dev-login` block), add:

```toml
# Compile-gate for the browser e2e test target (crates/web/tests/e2e.rs).
# Off by default so `cargo nextest run --workspace` never builds fantoccini
# or the browser tests. The CI `e2e` job and `just e2e` pass --features e2e.
e2e = []
```

In `[dev-dependencies]`, add (keep the block alphabetically tidy, matching the existing style):

```toml
fantoccini = { version = "0.22", default-features = false, features = ["rustls-tls"] }
```

At the end of the file, add the test target:

```toml
[[test]]
name = "e2e"
path = "tests/e2e.rs"
required-features = ["e2e"]
```

- [ ] **Step 2: Create a minimal `crates/web/tests/e2e.rs` that compiles under the feature**

```rust
//! Browser-level e2e tests for the dashboard. Compiled only under
//! `--features e2e` (see Cargo.toml `[[test]]`). Needs a running
//! chromedriver at `$WEBDRIVER_URL` (default http://localhost:9515).

#[path = "e2e_support/harness.rs"]
mod harness;
mod helpers;

#[tokio::test]
async fn placeholder_compiles() {
    // Replaced in later tasks. Asserts the target builds under the feature.
    assert_eq!(2 + 2, 4);
}
```

> `mod helpers;` pulls in `tests/helpers/mod.rs`. The harness file does not exist yet — Task 2 creates it. This task will not pass `cargo check --features e2e` until the file exists, so create an empty stub now:

Create `crates/web/tests/e2e_support/harness.rs` with a single line:

```rust
// Harness implemented in Task 2.
```

- [ ] **Step 3: Verify the default suite ignores the e2e target**

Run: `cargo nextest run -p twitch-1337-web 2>&1 | tail -5`
Expected: existing web tests run and pass; NO test named `placeholder_compiles` appears (target not built without the feature).

- [ ] **Step 4: Verify it compiles under the feature**

Run: `cargo check -p twitch-1337-web --features e2e --tests 2>&1 | tail -5`
Expected: compiles clean (warnings about unused `harness`/`helpers` are fine for now).

- [ ] **Step 5: Verify Cargo.lock updated and commit**

Run: `cargo metadata --format-version 1 >/dev/null` (refreshes Cargo.lock with fantoccini), then:

```bash
git add crates/web/Cargo.toml crates/web/tests/e2e.rs crates/web/tests/e2e_support/harness.rs Cargo.lock
git commit -m "build(web): gated e2e test target + fantoccini dev-dep"
```

---

## Task 2: Harness — server spawn, browser, cookie auth, waits

**Files:**
- Modify: `crates/web/tests/e2e_support/harness.rs`
- Modify: `crates/web/tests/e2e.rs` (replace placeholder test with a harness smoke test)

- [ ] **Step 1: Write the failing smoke test in `crates/web/tests/e2e.rs`**

Replace the `placeholder_compiles` test with:

```rust
use harness::E2eSession;

#[tokio::test]
async fn harness_boots_and_authenticates() {
    let s = E2eSession::start().await;
    s.goto("/pings").await;
    // Authenticated mod session lands on the pings page with the sidebar.
    s.wait_for("aside.sidebar").await;
    let html = s.source().await;
    assert!(html.contains("Pings"), "sidebar/page did not render: {html:.0}");
    s.close().await;
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e harness_boots 2>&1 | tail -20`
Expected: FAIL to compile — `E2eSession` not defined in `harness`.

- [ ] **Step 3: Implement the harness in `crates/web/tests/e2e_support/harness.rs`**

```rust
//! E2E harness: in-process dashboard server + headless Chrome client with
//! an authenticated mod session. Reuses `tests/helpers` for state/auth.

#![allow(dead_code)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use fantoccini::error::CmdError;
use fantoccini::{Client, ClientBuilder, Locator};
use serde_json::{json, Map, Value};
use tokio::sync::Notify;
use twitch_1337_web::{bind, build_router, serve_app};

#[path = "../helpers/mod.rs"]
mod helpers;

/// Default WebDriver endpoint; override with `WEBDRIVER_URL`.
fn webdriver_url() -> String {
    std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:9515".to_owned())
}

/// Bounded wait defaults for element queries.
const WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const WAIT_POLL: Duration = Duration::from_millis(100);

/// A running dashboard + browser session, authenticated as a mod.
/// Holds tempdirs alive for the server's lifetime and tears everything
/// down on `close()`.
pub struct E2eSession {
    pub client: Client,
    pub base_url: String,
    shutdown: Arc<Notify>,
    // Kept alive so the server's data dirs are not removed mid-test.
    _dirs: (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
}

impl E2eSession {
    /// Build state + router, serve on an ephemeral port, connect a headless
    /// Chrome, and inject signed session cookies for a mod user.
    pub async fn start() -> Self {
        helpers::install_crypto();
        let user_id = "9001";
        let helix = helpers::admin_helix(user_id);
        let (state, d1, d2, d3) = helpers::build_state_with_all_dirs(helix).await;
        let (signed_sid, signed_csrf, _bare) = helpers::insert_session(&state, user_id, "admin");

        // Serve on an ephemeral loopback port.
        let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind ephemeral");
        let port = listener.local_addr().expect("local_addr").port();
        let base_url = format!("http://127.0.0.1:{port}");
        let app = build_router(state);
        let shutdown = Arc::new(Notify::new());
        let srv_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_app(listener, app, srv_shutdown).await;
        });

        // Headless Chrome capabilities; auto-accept confirm() dialogs so
        // htmx hx-confirm and the JS delete-confirm proceed.
        let mut chrome_opts = Map::new();
        chrome_opts.insert(
            "args".to_owned(),
            json!([
                "--headless=new",
                "--no-sandbox",
                "--disable-gpu",
                "--window-size=1400,900",
            ]),
        );
        let mut caps: Map<String, Value> = Map::new();
        caps.insert("goog:chromeOptions".to_owned(), Value::Object(chrome_opts));
        caps.insert("unhandledPromptBehavior".to_owned(), json!("accept"));

        let mut builder = ClientBuilder::rustls().expect("rustls builder");
        builder.capabilities(caps);
        let client = builder
            .connect(&webdriver_url())
            .await
            .unwrap_or_else(|e| panic!("connect chromedriver at {}: {e}", webdriver_url()));

        // Cookies require being on the origin first. Land on the page, set
        // both signed cookies, done.
        client.goto(&base_url).await.expect("goto origin");
        add_cookie(&client, &base_url, "tw1337_sid", &signed_sid).await;
        add_cookie(&client, &base_url, "tw1337_csrf", &signed_csrf).await;

        Self { client, base_url, shutdown, _dirs: (d1, d2, d3) }
    }

    /// Navigate to a same-origin path (leading slash).
    pub async fn goto(&self, path: &str) {
        let url = format!("{}{}", self.base_url, path);
        self.client.goto(&url).await.expect("goto");
    }

    /// Current browser URL as a string.
    pub async fn current_url(&self) -> String {
        self.client.current_url().await.expect("current_url").to_string()
    }

    /// Full page HTML source.
    pub async fn source(&self) -> String {
        self.client.source().await.expect("source")
    }

    /// Wait until a CSS selector matches, returning the element.
    pub async fn wait_for(&self, css: &str) -> fantoccini::elements::Element {
        self.client
            .wait()
            .at_most(WAIT_TIMEOUT)
            .every(WAIT_POLL)
            .for_element(Locator::Css(css))
            .await
            .unwrap_or_else(|e| panic!("wait_for {css}: {e}"))
    }

    /// Wait until a CSS selector no longer matches any element.
    pub async fn wait_gone(&self, css: &str) {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        loop {
            match self.client.find(Locator::Css(css)).await {
                Err(CmdError::NoSuchElement(_)) => return,
                Ok(_) => {}
                Err(e) => panic!("wait_gone {css}: {e}"),
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("wait_gone {css}: still present after {WAIT_TIMEOUT:?}");
            }
            tokio::time::sleep(WAIT_POLL).await;
        }
    }

    /// Find one element now (no wait).
    pub async fn find(&self, css: &str) -> fantoccini::elements::Element {
        self.client
            .find(Locator::Css(css))
            .await
            .unwrap_or_else(|e| panic!("find {css}: {e}"))
    }

    /// Find all elements matching a selector now (no wait).
    pub async fn find_all(&self, css: &str) -> Vec<fantoccini::elements::Element> {
        self.client.find_all(Locator::Css(css)).await.unwrap_or_default()
    }

    /// Type text into an input, clearing it first.
    pub async fn fill(&self, css: &str, text: &str) {
        let el = self.find(css).await;
        el.clear().await.expect("clear");
        el.send_keys(text).await.expect("send_keys");
    }

    /// Click the element matching a selector.
    pub async fn click(&self, css: &str) {
        self.find(css).await.click().await.expect("click");
    }

    /// Close the browser and stop the server.
    pub async fn close(self) {
        let _ = self.client.close().await;
        self.shutdown.notify_waiters();
    }
}

async fn add_cookie(client: &Client, base_url: &str, name: &str, value: &str) {
    use fantoccini::cookies::Cookie;
    let mut cookie = Cookie::new(name.to_owned(), value.to_owned());
    cookie.set_path("/");
    let url = base_url.parse().expect("parse base url");
    client.add_cookie(cookie).await.unwrap_or_else(|e| {
        panic!("add_cookie {name} (origin {url:?}): {e}")
    });
}
```

> If `helpers::install_crypto` / `helpers::admin_helix` are named differently, check `crates/web/tests/helpers/mod.rs` and adjust (the existing `pings_routes.rs` `authed_setup` shows the real names: `install_crypto()`, `admin_helix(user_id)`, `build_state_with_all_dirs`, `insert_session`). Use whatever those tests use.

- [ ] **Step 4: Remove the duplicate `mod helpers;` clash**

Because `harness.rs` declares `#[path = "../helpers/mod.rs"] mod helpers;` AND `e2e.rs` declares `mod helpers;`, they would be two separate module instances — fine, but to avoid confusion keep `mod helpers;` ONLY in `harness.rs` and delete the `mod helpers;` line from `e2e.rs`. Update `e2e.rs` top to:

```rust
#[path = "e2e_support/harness.rs"]
mod harness;
```

- [ ] **Step 5: Run the smoke test to verify it passes**

Run: `WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e harness_boots 2>&1 | tail -20`
Expected: PASS (1 test). Requires chromedriver running (see Prerequisites).

- [ ] **Step 6: Commit**

```bash
git add crates/web/tests/e2e.rs crates/web/tests/e2e_support/harness.rs
git commit -m "test(web): e2e harness — in-process server + headless chrome + cookie auth"
```

---

## Task 3: Flow 1 — auth + nav smoke

**Files:**
- Modify: `crates/web/tests/e2e.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/web/tests/e2e.rs`:

```rust
/// Every sidebar nav link loads its page without bouncing to login.
#[tokio::test]
async fn nav_links_all_load_authenticated() {
    let s = E2eSession::start().await;
    s.goto("/pings").await;
    s.wait_for("aside.sidebar").await;

    // Hrefs from templates/sidebar.html.
    let paths = [
        "/pings",
        "/leaderboard",
        "/flights",
        "/schedules",
        "/memory/soul",
        "/memory/lore",
        "/memory/users",
        "/memory/state",
        "/settings",
    ];
    for path in paths {
        s.goto(path).await;
        // Authenticated: stays on the path, not redirected to /auth/login.
        let url = s.current_url().await;
        assert!(
            url.ends_with(path),
            "expected to land on {path}, got {url}"
        );
        // Sidebar present => layout rendered, not an error page.
        s.wait_for("aside.sidebar").await;
    }
    s.close().await;
}
```

> `/logs` is a stub page; included pages above are the real ones. If `/logs` renders the sidebar too, you may add it — but keep the list to pages that reliably render the layout.

- [ ] **Step 2: Run to verify it passes (no new production code needed)**

Run: `WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e nav_links 2>&1 | tail -20`
Expected: PASS.

> This flow needs no implementation beyond the harness; if it fails, the failure is a real finding (a page that doesn't render under a mod session). Debug with `superpowers:systematic-debugging` before changing the test.

- [ ] **Step 3: Commit**

```bash
git add crates/web/tests/e2e.rs
git commit -m "test(web): e2e flow — auth + sidebar nav smoke"
```

---

## Task 4: Flow 2 — pings (create, add member, htmx delete)

**Files:**
- Modify: `crates/web/tests/e2e.rs`

Selectors (from `templates/pings/form.html`, `list.html`):
- New form: `GET /pings/new`; `input[name="name"]` (`#ping-name`), `textarea[name="template"]` (`#ping-template`), submit `button[type="submit"].btn.primary`. Submits `POST /pings` → redirect to `/pings`.
- Row identity: a `.tr` whose `.ping-name` text equals the name, or `data-filter` starts with the name.
- Edit page (`GET /pings/{name}`): add-member form `input[name="username"]` (`#ping-add-member`) + `button[type="submit"].btn.primary`. Submits `POST /pings/{name}/members` → redirect.
- Delete (on `/pings` list): row's `button.row-act.danger` with `hx-post="/pings/{name}/delete"`, `hx-swap="outerHTML"`, `hx-confirm` (auto-accepted by caps). Row disappears.

- [ ] **Step 1: Write the failing test**

Append to `crates/web/tests/e2e.rs`:

```rust
/// Create a ping (form POST), add a member (form POST), then delete it
/// (htmx outerHTML swap). Exercises both the form-nav and htmx paths.
#[tokio::test]
async fn pings_create_add_member_delete() {
    let s = E2eSession::start().await;

    // Create.
    s.goto("/pings/new").await;
    s.wait_for("#ping-name").await;
    s.fill("#ping-name", "squad").await;
    s.fill("#ping-template", "yo {mentions}").await;
    s.click("button[type=\"submit\"].btn.primary").await;

    // Back on the list, the row exists.
    s.wait_for("a.ping-name").await;
    let names: Vec<String> = {
        let mut out = Vec::new();
        for el in s.find_all("a.ping-name").await {
            out.push(el.text().await.unwrap_or_default());
        }
        out
    };
    assert!(names.iter().any(|n| n == "squad"), "ping row missing: {names:?}");

    // Add a member from the edit page.
    s.goto("/pings/squad").await;
    s.wait_for("#ping-add-member").await;
    s.fill("#ping-add-member", "alice").await;
    s.click("form[action=\"/pings/squad/members\"] button[type=\"submit\"]").await;
    s.wait_for(".ping-name").await;
    assert!(s.source().await.contains("alice"), "member not shown after add");

    // Delete from the list via htmx outerHTML swap (confirm auto-accepted).
    s.goto("/pings").await;
    s.wait_for("button.row-act.danger[hx-post=\"/pings/squad/delete\"]").await;
    s.click("button.row-act.danger[hx-post=\"/pings/squad/delete\"]").await;
    s.wait_gone("button.row-act.danger[hx-post=\"/pings/squad/delete\"]").await;

    s.close().await;
}
```

- [ ] **Step 2: Run to verify it passes**

Run: `WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e pings_create 2>&1 | tail -30`
Expected: PASS.

> If the htmx delete does not remove the row, confirm `htmx.min.js` loads on the list page and the confirm dialog is auto-accepted (caps `unhandledPromptBehavior: accept`). Debug before weakening the assertion.

- [ ] **Step 3: Commit**

```bash
git add crates/web/tests/e2e.rs
git commit -m "test(web): e2e flow — pings create/add-member/htmx-delete"
```

---

## Task 5: Flow 3 — schedules (create, toggle, delete)

**Files:**
- Modify: `crates/web/tests/e2e.rs`

Selectors (from `templates/schedules/_form.html`, `_card.html`):
- Add form: `GET /schedules?new=true`; `input[name="name"]`, `textarea[name="message"]`. Default trigger kind is `interval` (radio already checked) — fill `input[name="interval_every"]` with `01:00` (type=text, pattern `^\d{1,3}:[0-5]\d$`). Leave `enabled` checkbox UNCHECKED so the card is created paused. Submit `button[type="submit"].btn.primary` → `POST /schedules/add` → redirect.
- Card identity: `article.schedule-card` containing `.card-title` text == name. Disabled card has class `paused`; toggle button `.toggle-switch` text is `off` (disabled) / `on` (enabled).
- Toggle: `form.card-toggle button.toggle-switch` (form `POST /schedules/{name}/toggle`) → redirect/reload.
- Delete: `form.js-delete-schedule[data-name="{name}"] button.chip.danger` (`POST /schedules/{name}/delete`). The JS confirm is auto-accepted by caps.

- [ ] **Step 1: Write the failing test**

Append to `crates/web/tests/e2e.rs`:

```rust
/// Create a disabled interval schedule, toggle it on, then delete it.
#[tokio::test]
async fn schedules_create_toggle_delete() {
    let s = E2eSession::start().await;

    // Create (interval kind is the default checked radio).
    s.goto("/schedules?new=true").await;
    s.wait_for("input[name=\"name\"]").await;
    s.fill("input[name=\"name\"]", "standup").await;
    s.fill("textarea[name=\"message\"]", "daily standup time").await;
    s.fill("input[name=\"interval_every\"]", "01:00").await;
    // `enabled` left unchecked => created paused.
    s.click("button[type=\"submit\"].btn.primary").await;

    // Card appears, paused (button reads "off").
    let card = "article.schedule-card";
    s.wait_for(card).await;
    let toggle_btn = "article.schedule-card form.card-toggle button.toggle-switch";
    let before = s.find(toggle_btn).await.text().await.unwrap_or_default();
    assert_eq!(before.trim(), "off", "new schedule should start disabled");

    // Toggle on (full form POST + reload).
    s.click(toggle_btn).await;
    s.wait_for(toggle_btn).await;
    // Re-query after reload.
    let after = s.find(toggle_btn).await.text().await.unwrap_or_default();
    assert_eq!(after.trim(), "on", "toggle did not enable the schedule");

    // Delete (JS confirm auto-accepted).
    s.click("form.js-delete-schedule[data-name=\"standup\"] button.chip.danger").await;
    s.wait_gone("form.js-delete-schedule[data-name=\"standup\"]").await;

    s.close().await;
}
```

- [ ] **Step 2: Run to verify it passes**

Run: `WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e schedules_create 2>&1 | tail -30`
Expected: PASS.

> If `interval_every` validation rejects `01:00`, re-check the pattern in `_form.html` and the `ScheduleForm` parse in `schedules.rs`. If the toggle button text has surrounding whitespace/newlines, the `.trim()` handles it; if it renders differently, assert on the `article.schedule-card.paused` class presence/absence instead.

- [ ] **Step 3: Commit**

```bash
git add crates/web/tests/e2e.rs
git commit -m "test(web): e2e flow — schedules create/toggle/delete"
```

---

## Task 6: Flow 4 — settings save

**Files:**
- Modify: `crates/web/tests/e2e.rs`

Selectors (from `templates/settings/index.html`, `_macros.html`, `cards/cooldowns.html`):
- `GET /settings`; numeric field `input[name="cooldown_ai"]` (min 1, max 3600). Save button `button[type="submit"].btn.primary[form="settings-form"]`. Submits `POST /settings` → redirect/reload.
- After reload, the input's live value is the saved value; read via `prop("value")`.

- [ ] **Step 1: Write the failing test**

Append to `crates/web/tests/e2e.rs`:

```rust
/// Change a numeric setting, save (full form POST), reload, assert persisted.
#[tokio::test]
async fn settings_save_cooldown() {
    let s = E2eSession::start().await;
    s.goto("/settings").await;
    s.wait_for("input[name=\"cooldown_ai\"]").await;

    // Pick a value inside [1, 3600] unlikely to be the default.
    s.fill("input[name=\"cooldown_ai\"]", "47").await;
    s.click("button[type=\"submit\"].btn.primary[form=\"settings-form\"]").await;

    // Reload settings and confirm the saved value renders back.
    s.goto("/settings").await;
    let el = s.wait_for("input[name=\"cooldown_ai\"]").await;
    let val = el.prop("value").await.unwrap_or_default().unwrap_or_default();
    assert_eq!(val, "47", "cooldown_ai did not persist");

    s.close().await;
}
```

> `Element::prop` returns `Result<Option<String>>` in fantoccini; the double `unwrap_or_default()` flattens `Result` then `Option`. If the signature differs in 0.22, adjust to match (e.g. `.attr("value")`), keeping the assertion on the live value.

- [ ] **Step 2: Run to verify it passes**

Run: `WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e settings_save 2>&1 | tail -30`
Expected: PASS.

> If the save submits but the value does not persist, verify the settings tempdir is retained (harness keeps all three dirs via `_dirs`) — a dropped settings dir would lose the write.

- [ ] **Step 3: Run the whole e2e suite once**

Run: `WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e 2>&1 | tail -15`
Expected: all e2e tests pass (5 incl. the harness smoke).

- [ ] **Step 4: Commit**

```bash
git add crates/web/tests/e2e.rs
git commit -m "test(web): e2e flow — settings save persists"
```

---

## Task 7: CI job + Justfile recipe

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `Justfile`

- [ ] **Step 1: Add the non-blocking `e2e` job to `.github/workflows/ci.yml`**

Insert after the `test` job (mirror its checkout / toolchain / cache steps exactly). Use the real current values when implementing: pin `browser-actions/setup-chrome` to its **v2.1.2 commit SHA** (look it up at implementation time) with a `# v2.1.2` comment, and set `chrome-version` to a current stable build (e.g. a real `M.m.b.p`, or a major like `135`).

```yaml
  e2e:
    name: e2e
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6
        with:
          persist-credentials: false

      - name: Read Rust toolchain channel
        id: rust-toolchain
        run: |
          channel=$(grep '^channel' crates/twitch-1337/rust-toolchain.toml | sed -E 's/.*"([^"]+)".*/\1/')
          echo "channel=$channel" >> "$GITHUB_OUTPUT"

      - uses: dtolnay/rust-toolchain@master # zizmor: ignore[unpinned-uses]
        with:
          toolchain: ${{ steps.rust-toolchain.outputs.channel }}

      - uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32 # v2

      - name: Install nextest
        uses: taiki-e/install-action@nextest # zizmor: ignore[unpinned-uses]

      - id: chrome
        uses: browser-actions/setup-chrome@<PIN-v2.1.2-SHA> # v2.1.2
        with:
          chrome-version: '135'
          install-chromedriver: true
          install-dependencies: true

      - name: Start chromedriver
        run: |
          "${{ steps.chrome.outputs.chromedriver-path }}" --port=9515 &
          for i in $(seq 1 30); do
            curl -sf http://localhost:9515/status >/dev/null && break
            sleep 0.3
          done

      - name: cargo nextest (e2e)
        env:
          WEBDRIVER_URL: http://localhost:9515
        run: cargo nextest run -p twitch-1337-web --features e2e
```

> Do NOT add `e2e` to required status checks / branch protection — it stays non-blocking (per spec).

- [ ] **Step 2: Validate the workflow YAML**

Run: `which actionlint && actionlint .github/workflows/ci.yml || echo "actionlint not installed — re-read the edited block for indentation"`
Expected: no errors (or skipped if actionlint absent).

- [ ] **Step 3: Add the `just e2e` recipe to `Justfile`**

Add near the other test recipes:

```just
# Run browser e2e tests. Starts chromedriver on :9515 if not already up.
# Requires chromedriver + Chrome installed and version-matched.
e2e:
  @if ! curl -sf http://localhost:9515/status >/dev/null 2>&1; then \
      echo "starting chromedriver on :9515"; \
      chromedriver --port=9515 & \
      sleep 1; \
  fi
  WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e
```

- [ ] **Step 4: Verify the Justfile recipe parses**

Run: `just --list 2>&1 | grep e2e`
Expected: the `e2e` recipe appears.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml Justfile
git commit -m "build: non-blocking e2e CI job + just e2e recipe"
```

---

## Task 8: Full verification + push

**Files:** none (verification only)

- [ ] **Step 1: Confirm the required suite is untouched**

Run: `cargo nextest run --workspace 2>&1 | tail -5`
Expected: same pass count as before this branch (e2e target NOT built/run without the feature).

- [ ] **Step 2: fmt + clippy**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean. (Clippy does not build the e2e target without the feature; optionally also run `cargo clippy -p twitch-1337-web --features e2e --tests` and fix any lints in the harness/tests.)

- [ ] **Step 3: Full e2e run (chromedriver up)**

Run: `WEBDRIVER_URL=http://localhost:9515 cargo nextest run -p twitch-1337-web --features e2e 2>&1 | tail -10`
Expected: all e2e tests pass.

- [ ] **Step 4: Push + open PR**

```bash
git push -u origin feature/web-e2e-testing
gh pr create --fill --base main
```

Wait for the 9 required checks (the `e2e` job is extra and non-blocking). Rebase on main if `strict` blocks merge.

---

## Self-Review Notes (for the implementer)

- **Helper names:** Step 3 of Task 2 assumes `helpers::install_crypto`, `helpers::admin_helix`, `build_state_with_all_dirs`, `insert_session`. These are taken from the existing `pings_routes.rs`/`helpers/mod.rs`; confirm exact names and adjust if they differ.
- **fantoccini API drift:** `Element::prop`/`clear`/`send_keys`/`text` and `Wait::at_most/every/for_element` are from fantoccini 0.22. If a signature differs, fix the call, not the intent.
- **No restart-badge assertion:** the cooldowns card has no restart badge ("changes take effect right away"); the settings flow asserts persistence only. A restart-badge test would target a connection field (AI backend / channels) and is out of scope here.
- **Flakiness:** all waits are bounded polls (10s). If CI is slow, raise `WAIT_TIMEOUT`. Never replace a wait with a fixed `sleep`.
