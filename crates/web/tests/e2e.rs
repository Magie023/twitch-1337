//! Browser-level e2e tests for the dashboard. Compiled only under
//! `--features e2e` (see Cargo.toml `[[test]]`). Needs a running
//! chromedriver at `$WEBDRIVER_URL` (default http://localhost:9515).

#[path = "e2e_support/harness.rs"]
mod harness;

use harness::E2eSession;

#[tokio::test]
async fn harness_boots_and_authenticates() {
    let s = E2eSession::start().await;
    s.goto("/pings").await;
    // Authenticated mod session lands on the pings page with the sidebar.
    s.wait_for("aside.sidebar").await;
    let html = s.source().await;
    assert!(
        html.contains("Pings"),
        "sidebar/page did not render; first 200 chars: {html:.200}"
    );
    s.close().await;
}

/// Every sidebar nav link loads its page without bouncing to login.
#[tokio::test]
async fn nav_links_all_load_authenticated() {
    let s = E2eSession::start().await;
    s.goto("/pings").await;
    s.wait_for("aside.sidebar").await;

    // Hrefs from templates/sidebar.html.
    // NOTE: /settings is owner-gated (require_owner middleware) and the
    // harness session is Role::Mod with no owner configured in the stub state
    // (state.owner = None). A Mod session returns 403 on GET /settings —
    // confirmed by settings_route.rs::non_owner_get_settings_returns_403.
    // Excluded here; it is exercised in settings_route.rs integration tests.
    let paths = [
        "/pings",
        "/leaderboard",
        "/flights",
        "/schedules",
        "/memory/soul",
        "/memory/lore",
        "/memory/users",
        "/memory/state",
    ];
    for path in paths {
        s.goto(path).await;
        // Authenticated: stays on the path, not redirected to /auth/login.
        let url = s.current_url().await;
        assert!(url.ends_with(path), "expected to land on {path}, got {url}");
        // Sidebar present => layout rendered, not an error page.
        s.wait_for("aside.sidebar").await;
    }
    s.close().await;
}

/// Create a ping (form POST), add a member (form POST), then delete it
/// (htmx outerHTML swap). Exercises both the form-nav and htmx paths.
#[tokio::test]
async fn pings_create_add_member_delete() {
    let s = E2eSession::start().await;

    // Create.
    s.goto("/pings/new").await;
    s.wait_and_fill("#ping-name", "squad").await;
    s.fill("#ping-template", "yo {mentions}").await;
    s.click("button[type=\"submit\"].btn.primary").await;

    // Back on the list, the row exists.
    s.wait_for("a.ping-name").await;
    let mut names = Vec::new();
    for el in s.find_all("a.ping-name").await {
        names.push(el.text().await.unwrap_or_default());
    }
    assert!(
        names.iter().any(|n| n == "squad"),
        "ping row missing: {names:?}"
    );

    // Add a member from the edit page.
    s.goto("/pings/squad").await;
    s.wait_and_fill("#ping-add-member", "alice").await;
    s.click("form[action=\"/pings/squad/members\"] button[type=\"submit\"]")
        .await;
    s.wait_for(".ping-name").await;
    assert!(
        s.source().await.contains("alice"),
        "member not shown after add"
    );

    // Delete from the list via htmx outerHTML swap (confirm auto-accepted).
    s.goto("/pings").await;
    let del_btn = "button.row-act.danger[hx-post=\"/pings/squad/delete\"]";
    s.wait_and_click(del_btn).await;
    s.wait_gone(del_btn).await;

    s.close().await;
}

/// Create a disabled interval schedule, toggle it on, then delete it.
#[tokio::test]
async fn schedules_create_toggle_delete() {
    let s = E2eSession::start().await;

    // Create (interval kind is the default checked radio).
    s.goto("/schedules?new=true").await;
    s.wait_and_fill("input[name=\"name\"]", "standup").await;
    s.fill("textarea[name=\"message\"]", "daily standup time")
        .await;
    s.fill("input[name=\"interval_every\"]", "01:00").await;
    // `enabled` left unchecked => created paused.
    s.click("button[type=\"submit\"].btn.primary").await;

    // Card appears, paused. CSS `text-transform` may uppercase the rendered
    // "off"/"on", so normalise to lowercase before comparing.
    let toggle_btn = "article.schedule-card form.card-toggle button.toggle-switch";
    let before = s.wait_text(toggle_btn).await.to_lowercase();
    assert_eq!(before, "off", "new schedule should start disabled");

    // Toggle on (full form POST + redirect to /schedules).
    s.click(toggle_btn).await;
    // Wait for the paused card to disappear — confirms the page has reloaded
    // after the POST and the schedule is now enabled.
    s.wait_gone("article.schedule-card.paused").await;
    let after = s.wait_text(toggle_btn).await.to_lowercase();
    assert_eq!(after, "on", "toggle did not enable the schedule");

    // Delete (JS confirm auto-accepted).
    s.click("form.js-delete-schedule[data-name=\"standup\"] button.chip.danger")
        .await;
    s.wait_gone("form.js-delete-schedule[data-name=\"standup\"]")
        .await;

    s.close().await;
}

/// Change a numeric setting, save (full form POST), reload, assert persisted.
#[tokio::test]
async fn settings_save_cooldown() {
    let s = E2eSession::start_as_owner().await;
    s.goto("/settings").await;

    // Pick a value inside [1, 3600] unlikely to be the default.
    s.wait_and_fill("input[name=\"cooldown_ai\"]", "47").await;
    s.click("button[type=\"submit\"].btn.primary[form=\"settings-form\"]")
        .await;

    // Reload settings and confirm the saved value renders back.
    s.goto("/settings").await;
    let el = s.wait_for("input[name=\"cooldown_ai\"]").await;
    let val = el
        .prop("value")
        .await
        .unwrap_or_default()
        .unwrap_or_default();
    assert_eq!(val, "47", "cooldown_ai did not persist");

    s.close().await;
}
