//! User-facing error messages for form re-renders.
//!
//! Handlers that preserve the user's draft on validation failure thread
//! these strings into the template `error` field instead of duplicating
//! per-variant match arms at every call site.

use twitch_1337_core::ai::memory::store::WriteError;

use crate::error::WebError;

/// Maps a domain error to the short message shown in form error banners.
pub trait UserFacing {
    fn user_message(&self) -> String;
}

impl UserFacing for WriteError {
    fn user_message(&self) -> String {
        match self {
            WriteError::Full => "exceeds byte cap".to_owned(),
            WriteError::StateFull => "state collection full".to_owned(),
            WriteError::InvalidSlug => "reserved or invalid slug".to_owned(),
            // `Io` should bubble via [`write_error_for_form`], not display.
            WriteError::Io(e) => format!("{e:#}"),
        }
    }
}

/// Ping create/update/member validation errors from `PingHandle`.
impl UserFacing for eyre::Report {
    fn user_message(&self) -> String {
        self.to_string()
    }
}

/// `WriteError::Io` bubbles to `WebError::Internal`; other variants become
/// an inline form message.
pub fn write_error_for_form(err: WriteError) -> Result<String, WebError> {
    match err {
        WriteError::Io(e) => Err(WebError::Internal(e)),
        e => Ok(e.user_message()),
    }
}
