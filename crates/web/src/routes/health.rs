use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

pub const GIT_SHA: &str = env!("GIT_SHA_SHORT");
pub const BUILD_NUM: &str = env!("BUILD_NUM");

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
