//! E2E harness: in-process dashboard server + headless Chrome client with
//! an authenticated mod session. Reuses `tests/helpers` for state/auth.

#![allow(dead_code)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use fantoccini::{Client, ClientBuilder, Locator};
use serde_json::json;
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

/// Selects which session role the harness injects.
enum SessionKind {
    /// Authenticated mod (no owner configured). Default for `start()`.
    Mod,
    /// Authenticated owner (user marked as the configured owner).
    /// Required for owner-gated pages like /settings.
    Owner,
}

/// A running dashboard + browser session, authenticated as a mod.
/// Holds tempdirs alive for the server's lifetime and tears everything
/// down on `close()`. On a panic before `close()`, the browser session
/// leaks until chromedriver exits — acceptable here since the CI/local
/// driver is ephemeral (fantoccini's async `close` consumes `self`, so it
/// can't run from `Drop`).
pub struct E2eSession {
    client: Client,
    base_url: String,
    shutdown: Arc<Notify>,
    // Kept alive so the server's data dirs are not removed mid-test.
    _dirs: (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
}

impl E2eSession {
    /// Build state + router, serve on an ephemeral port, connect a headless
    /// Chrome, and inject signed session cookies for a mod user.
    pub async fn start() -> Self {
        Self::start_inner(SessionKind::Mod).await
    }

    /// Like [`start`] but configures an owner session.
    /// Required for owner-gated pages like `/settings`.
    pub async fn start_as_owner() -> Self {
        Self::start_inner(SessionKind::Owner).await
    }

    async fn start_inner(kind: SessionKind) -> Self {
        helpers::install_crypto();
        let user_id = "9001";
        let helix = helpers::admin_helix(user_id);
        let (state, d1, d2, d3) = helpers::build_state_with_all_dirs(helix).await;
        let (signed_sid, signed_csrf, _bare) = match kind {
            SessionKind::Mod => helpers::insert_session(&state, user_id, "admin"),
            SessionKind::Owner => {
                helpers::set_owner(&state, Some(user_id));
                helpers::insert_session_as(
                    &state,
                    user_id,
                    "owner",
                    twitch_1337_web::auth::Role::Owner,
                )
            }
        };

        // Serve on an ephemeral loopback port.
        let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind ephemeral");
        let port = listener.local_addr().expect("local_addr").port();
        // Use "localhost" (not "127.0.0.1") so WebDriver's AddCookie command
        // accepts the domain: Chrome rejects IP-address domains with
        // "invalid argument: invalid 'domain'".
        let base_url = format!("http://localhost:{port}");
        let app = build_router(state);
        let shutdown = Arc::new(Notify::new());
        let srv_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = serve_app(listener, app, srv_shutdown).await;
        });

        // Headless Chrome capabilities; auto-accept confirm() dialogs so
        // htmx hx-confirm and the JS delete-confirm proceed.
        let mut caps = json!({
            "goog:chromeOptions": {
                "args": ["--headless=new", "--no-sandbox", "--disable-gpu", "--window-size=1400,900"],
            },
            "unhandledPromptBehavior": "accept",
        });
        // Pin the Chrome binary when given (CI sets this to the
        // setup-chrome-installed browser). Without it, chromedriver launches
        // whatever chrome it finds by default — on GitHub runners that's a
        // preinstalled Chrome whose version may not match the installed
        // chromedriver, yielding "session not created: only supports Chrome N".
        if let Ok(bin) = std::env::var("E2E_CHROME_BINARY")
            && !bin.is_empty()
        {
            caps["goog:chromeOptions"]["binary"] = json!(bin);
        }
        let caps = caps.as_object().expect("caps is a json object").clone();

        let mut builder = ClientBuilder::rustls().expect("rustls builder");
        builder.capabilities(caps);
        let client = builder
            .connect(&webdriver_url())
            .await
            .unwrap_or_else(|e| panic!("connect chromedriver at {}: {e}", webdriver_url()));

        // Cookies require being on the origin first. Navigate to the public
        // health endpoint so the browser is on the right origin before we
        // inject cookies. Using /healthz avoids auth redirects.
        client
            .goto(&format!("{base_url}/healthz"))
            .await
            .expect("goto origin");
        // Use JavaScript to set the cookies — WebDriver's AddCookie command
        // rejects loopback origins with "invalid 'domain'" on Chrome 148.
        set_cookie_js(&client, "tw1337_sid", &signed_sid).await;
        set_cookie_js(&client, "tw1337_csrf", &signed_csrf).await;

        Self {
            client,
            base_url,
            shutdown,
            _dirs: (d1, d2, d3),
        }
    }

    /// Navigate to a same-origin path (leading slash).
    pub async fn goto(&self, path: &str) {
        let url = format!("{}{}", self.base_url, path);
        self.client.goto(&url).await.expect("goto");
    }

    /// Current browser URL as a string.
    pub async fn current_url(&self) -> String {
        self.client
            .current_url()
            .await
            .expect("current_url")
            .to_string()
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
                Err(e) if e.is_no_such_element() => return,
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
        self.client
            .find_all(Locator::Css(css))
            .await
            .unwrap_or_default()
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

    /// Wait for an input to appear, then fill it (clearing first). Reuses the
    /// waited element — no second `find` round-trip.
    pub async fn wait_and_fill(&self, css: &str, text: &str) {
        let el = self.wait_for(css).await;
        el.clear().await.expect("clear");
        el.send_keys(text).await.expect("send_keys");
    }

    /// Wait for an element to appear, then click it (reusing the waited element).
    pub async fn wait_and_click(&self, css: &str) {
        self.wait_for(css).await.click().await.expect("click");
    }

    /// Wait for an element, then return its trimmed visible text.
    pub async fn wait_text(&self, css: &str) -> String {
        self.wait_for(css)
            .await
            .text()
            .await
            .unwrap_or_default()
            .trim()
            .to_owned()
    }

    /// Close the browser and stop the server.
    pub async fn close(self) {
        let _ = self.client.close().await;
        self.shutdown.notify_waiters();
    }
}

/// Set a session cookie via `document.cookie`. Bypasses WebDriver's
/// `AddCookie` command which Chrome rejects for loopback origins.
async fn set_cookie_js(client: &Client, name: &str, value: &str) {
    // Percent-encode so `=`/`;`/space in the signed cookie string don't corrupt
    // the cookie; the server's `parse_encoded` percent-decodes it back.
    let encoded_value = urlencoding::encode(value);
    let script = format!("document.cookie = \"{name}={encoded_value}; path=/\";");
    client
        .execute(&script, vec![])
        .await
        .unwrap_or_else(|e| panic!("set_cookie_js {name}: {e}"));
}
