//! Role-gate decision: hidden_admins → broadcaster → helix moderators.
//!
//! Hidden admins (configured in `[twitch].hidden_admins`) short-circuit the
//! helix lookup so a debugging account always retains access. The broadcaster
//! id is checked next as a fast path. Otherwise we follow the moderator list.

use secrecy::ExposeSecret as _;

use crate::helix::HelixClient;
use crate::state::WebState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    Allow,
    Deny,
}

/// Hidden_admins / broadcaster shortcuts shared by both check variants. Returns
/// `Some(Allow)` iff a shortcut applies; `None` means the helix lookup runs.
fn shortcut(user_id: &str, broadcaster_id: &str, hidden_admins: &[String]) -> Option<GateOutcome> {
    if hidden_admins.iter().any(|s| s == user_id) || user_id == broadcaster_id {
        Some(GateOutcome::Allow)
    } else {
        None
    }
}

pub async fn check_is_mod(
    helix: &dyn HelixClient,
    user_id: &str,
    broadcaster_id: &str,
    hidden_admins: &[String],
) -> eyre::Result<GateOutcome> {
    if let Some(o) = shortcut(user_id, broadcaster_id, hidden_admins) {
        return Ok(o);
    }
    if helix.is_moderator(broadcaster_id, user_id).await? {
        Ok(GateOutcome::Allow)
    } else {
        Ok(GateOutcome::Deny)
    }
}

/// Variant used during the OAuth callback. Asks Twitch which channels the
/// user moderates (scope `user:read:moderated_channels` on the user token)
/// and checks `broadcaster_id` against that list — the
/// `helix/moderation/moderators` endpoint can't be used here because it
/// requires the bearer to *be* the broadcaster.
pub async fn check_is_mod_with_token(
    state: &WebState,
    user_id: &str,
    user_access_token: &str,
    broadcaster_id: &str,
    hidden_admins: &[String],
) -> eyre::Result<GateOutcome> {
    if let Some(o) = shortcut(user_id, broadcaster_id, hidden_admins) {
        return Ok(o);
    }
    if is_moderator_with_user_token(user_id, user_access_token, broadcaster_id, state).await? {
        Ok(GateOutcome::Allow)
    } else {
        Ok(GateOutcome::Deny)
    }
}

async fn is_moderator_with_user_token(
    user_id: &str,
    access_token: &str,
    broadcaster_id: &str,
    state: &WebState,
) -> eyre::Result<bool> {
    crate::helix::user_moderates_channel(
        &state.oauth.http,
        &state.oauth.helix_api_base,
        state.client_id.expose_secret(),
        access_token,
        user_id,
        broadcaster_id,
        "helix moderated channels (user token)",
    )
    .await
}

/// Allow iff `user_id` appears in `allowlist`.
pub fn check_in_allowlist(user_id: &str, allowlist: &[String]) -> GateOutcome {
    if allowlist.iter().any(|id| id == user_id) {
        GateOutcome::Allow
    } else {
        GateOutcome::Deny
    }
}

#[cfg(test)]
mod tests {
    use eyre::eyre;

    use super::*;
    use crate::helix::{HelixClient, HelixUser};

    fn admins(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    // --- shortcut (pure) ---

    #[test]
    fn shortcut_allows_hidden_admin() {
        assert_eq!(
            shortcut("u1", "broadcaster", &admins(&["u1"])),
            Some(GateOutcome::Allow)
        );
    }

    #[test]
    fn shortcut_allows_broadcaster() {
        assert_eq!(
            shortcut("broadcaster", "broadcaster", &admins(&[])),
            Some(GateOutcome::Allow)
        );
    }

    #[test]
    fn shortcut_returns_none_for_ordinary_user() {
        assert_eq!(shortcut("u2", "broadcaster", &admins(&["u1"])), None);
    }

    // --- check_in_allowlist (pure) ---

    #[test]
    fn allowlist_allows_listed_user() {
        assert_eq!(
            check_in_allowlist("u1", &admins(&["u0", "u1"])),
            GateOutcome::Allow
        );
    }

    #[test]
    fn allowlist_denies_unlisted_and_empty() {
        assert_eq!(
            check_in_allowlist("u9", &admins(&["u1"])),
            GateOutcome::Deny
        );
        assert_eq!(check_in_allowlist("u1", &admins(&[])), GateOutcome::Deny);
    }

    // --- check_is_mod (with a fake helix) ---

    /// Records whether `is_moderator` was called and returns a canned result.
    struct FakeHelix {
        mod_result: std::sync::Mutex<eyre::Result<bool>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeHelix {
        fn ok(value: bool) -> Self {
            Self {
                mod_result: std::sync::Mutex::new(Ok(value)),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn erroring() -> Self {
            Self {
                mod_result: std::sync::Mutex::new(Err(eyre!("helix down"))),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl HelixClient for FakeHelix {
        async fn fetch_user_by_id(&self, _user_id: &str) -> eyre::Result<Option<HelixUser>> {
            unimplemented!("not exercised by role-gate tests")
        }
        async fn fetch_user_by_login(&self, _login: &str) -> eyre::Result<Option<HelixUser>> {
            unimplemented!("not exercised by role-gate tests")
        }
        async fn is_moderator(&self, _broadcaster: &str, _user: &str) -> eyre::Result<bool> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Replace with a fresh Err so we don't need Clone on eyre::Report.
            std::mem::replace(&mut *self.mod_result.lock().unwrap(), Ok(false))
        }
    }

    #[tokio::test]
    async fn check_is_mod_short_circuits_without_helix_call() {
        let helix = FakeHelix::ok(false); // would Deny if consulted
        let outcome = check_is_mod(&helix, "u1", "broadcaster", &admins(&["u1"]))
            .await
            .unwrap();
        assert_eq!(outcome, GateOutcome::Allow);
        assert_eq!(helix.calls(), 0, "shortcut must not hit helix");
    }

    #[tokio::test]
    async fn check_is_mod_allows_when_helix_says_moderator() {
        let helix = FakeHelix::ok(true);
        let outcome = check_is_mod(&helix, "u2", "broadcaster", &admins(&["u1"]))
            .await
            .unwrap();
        assert_eq!(outcome, GateOutcome::Allow);
        assert_eq!(helix.calls(), 1);
    }

    #[tokio::test]
    async fn check_is_mod_denies_when_helix_says_not_moderator() {
        let helix = FakeHelix::ok(false);
        let outcome = check_is_mod(&helix, "u2", "broadcaster", &admins(&["u1"]))
            .await
            .unwrap();
        assert_eq!(outcome, GateOutcome::Deny);
    }

    #[tokio::test]
    async fn check_is_mod_propagates_helix_error() {
        let helix = FakeHelix::erroring();
        let result = check_is_mod(&helix, "u2", "broadcaster", &admins(&["u1"])).await;
        assert!(result.is_err(), "helix failure must not silently allow");
    }
}
