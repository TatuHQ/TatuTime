//! Build identity for `/healthz` and the GUI footer.
//!
//! The version is [`Cargo.toml`](Cargo.toml)'s (`CARGO_PKG_VERSION`, the release
//! version per `AGENTS.md`). The build id is the commit the binary was compiled
//! from: `git rev-parse` answers that for a source build, but the Docker builder
//! never copies `.git`, so a `TATU_BUILD` environment variable wins when it is
//! set (CI's image job passes the commit as a build arg). A build that can say
//! neither is labelled `unknown` rather than inventing an identifier.
//!
//! The id is normalised to the seven characters the release tags use
//! (`v<version>+<short-sha>`), so the footer, `/healthz` and the GitHub Release
//! name the same build the same way.

use std::process::Command;

fn main() {
    // Every way the checked-out commit can move: a checkout rewrites
    // `.git/HEAD`, while a commit or merge leaves `HEAD` alone and updates
    // `.git/refs/heads/<branch>` and `.git/logs/HEAD` (a gc may pack the refs).
    // Watching only `.git/HEAD` let a binary keep stamping the *previous* commit
    // — exactly the stale identity this file exists to prevent.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-changed=.git/logs/HEAD");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    println!("cargo:rerun-if-env-changed=TATU_BUILD");
    println!("cargo:rustc-env=TATU_BUILD={}", build_id());
}

/// `TATU_BUILD` when set, else the short HEAD sha, else `unknown`.
fn build_id() -> String {
    let from_env = std::env::var("TATU_BUILD")
        .ok()
        .map(|value| normalise(value.trim()))
        .filter(|value| !value.is_empty());
    from_env
        .or_else(git_short_sha)
        .unwrap_or_else(|| "unknown".to_string())
}

/// The short commit sha of the checkout the build script runs in.
fn git_short_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?;
    let sha = normalise(sha.trim());
    (!sha.is_empty()).then_some(sha)
}

/// Shorten a full commit sha to the tagged form's seven characters. Only
/// hex-only values are touched, so a human or CI label (`ci-1234`) survives
/// verbatim.
fn normalise(value: &str) -> String {
    if value.len() > 7 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        value[..7].to_string()
    } else {
        value.to_string()
    }
}
