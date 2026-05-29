//! Bake build-identity env vars into the `core` crate so chat-facing surfaces
//! (startup/shutdown announce, `!v`) report the same BUILD_NUM + GIT_SHA the
//! web dashboard shows. Mirrors `crates/web/build.rs`.

use std::env;
use std::process::Command;

/// Commit SHA: Docker supplies `GIT_SHA` (the `.dockerignore` strips `.git/`);
/// local cargo runs fall back to `git rev-parse`, then `"unknown"`.
fn git_sha() -> String {
    if let Ok(sha) = env::var("GIT_SHA")
        && !sha.is_empty()
    {
        return sha.chars().take(7).collect();
    }
    Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// CI run number: Docker supplies `BUILD_NUM`; local cargo runs fall back to "dev".
fn build_num() -> String {
    env::var("BUILD_NUM")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_owned())
}

fn main() {
    println!("cargo:rustc-env=GIT_SHA_SHORT={}", git_sha());
    println!("cargo:rerun-if-env-changed=GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    println!("cargo:rustc-env=BUILD_NUM={}", build_num());
    println!("cargo:rerun-if-env-changed=BUILD_NUM");
}
