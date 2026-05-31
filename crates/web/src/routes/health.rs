use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

pub const GIT_SHA: &str = match option_env!("GIT_SHA_SHORT") {
    Some(v) => v,
    None => "unknown",
};
pub const BUILD_NUM: &str = match option_env!("BUILD_NUM") {
    Some(v) => v,
    None => "dev",
};

pub fn router(irc_connected: Arc<AtomicBool>) -> Router {
    Router::new().route(
        "/healthz",
        get(move || {
            let flag = irc_connected.clone();
            async move {
                if flag.load(Ordering::Relaxed) {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }
        }),
    )
}
