//! Compile-time build provenance for the `--version` CLI surface.
//!
//! Values are injected by [`build.rs`](../build.rs) at compile time. This is
//! the twin of `tachi_server::build_info`; the two crates share the same
//! `GIT_SHA` so the binary's `--version` and the daemon's `/health` report
//! the same provenance.

/// Full git HEAD SHA captured at build time, or `"unknown"` for a no-git
/// (tarball/sandbox) build.
pub const GIT_SHA: &str = env!("GIT_SHA");

/// UTC build timestamp captured at build time, or `"unknown"`.
pub const BUILD_TIME: &str = env!("BUILD_TIME");

/// The package version.
pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// First 12 chars of the git SHA (or the full value if shorter).
pub fn git_sha_short() -> &'static str {
    if GIT_SHA.len() >= 12 {
        &GIT_SHA[..12]
    } else {
        GIT_SHA
    }
}

/// Human-readable build identifier used by `--version`: the package version
/// followed by `+` and the short sha, e.g. `"1.6.4+52fdd5306a1a"`.
pub fn build_version_string() -> String {
    format!("{PKG_VERSION}+{}", git_sha_short())
}
