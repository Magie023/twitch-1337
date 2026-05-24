//! Web-crate-local config view.
//!
//! Mirrors the relevant fields of `core::config::WebConfig` so the web crate
//! stays decoupled from core. The bin populates this from the parsed
//! `Configuration` when wiring up the dashboard.
//!
//! `session_ttl` and `mod_check_refresh` are intentionally absent: they are
//! read live from the `SettingsHandle` on every request so dashboard changes
//! take effect without a restart.

use secrecy::SecretString;

#[derive(Clone)]
pub struct WebConfig {
    pub bind_addr: String,
    pub public_url: String,
    pub session_secret: SecretString,
}
