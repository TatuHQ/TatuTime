//! Compile-time build identity: what `/healthz` and the GUI footer report.
//!
//! [`VERSION`] is the crate version — the release version, bumped in the PR that
//! lands (`AGENTS.md`). [`BUILD`] is the commit the binary was compiled from,
//! recorded by `build.rs` from git on a source build or from `TUCANO_BUILD` when
//! the build context has no `.git` (the Docker image). Neither is read from
//! storage or taken from a client: a running server can only ever describe
//! itself, so the footer cannot be made to claim a version it is not.

use serde_json::{Value, json};

/// The crate version from `Cargo.toml` — also what `--version` prints.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit this binary was built from; `unknown` when the build could not
/// name one (no git checkout and no `TUCANO_BUILD`).
pub const BUILD: &str = env!("TUCANO_BUILD");

/// The liveness body: status plus the build identity, so an operator — or the
/// GUI footer — can tell *which* server answered without shelling into it.
pub fn health() -> Value {
    json!({ "status": "ok", "version": VERSION, "build": BUILD })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_names_the_version_and_a_non_empty_build() {
        let body = health();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], VERSION);
        // The build id is whatever the build script recorded; it must never be
        // blank, because a blank footer suffix would read as "no build".
        assert!(
            body["build"]
                .as_str()
                .is_some_and(|build| !build.trim().is_empty()),
            "the build id is always a non-empty label"
        );
    }
}
