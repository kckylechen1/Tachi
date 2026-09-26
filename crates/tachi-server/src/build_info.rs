//! Compile-time build provenance — the daemon's answer to "which commit am I?".
//!
//! Re-exports [`tachi_bootstrap::build_info`], the single stamped source
//! (its `build.rs` injects `GIT_SHA` / `BUILD_TIME`), so the daemon's
//! `/health`, the pid file and the `--version` CLI output all report the same
//! provenance. See that module for the rationale (deploy autopsy 2026-07-06).

pub use tachi_bootstrap::build_info::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_version_matches_tachi_server_version() {
        // `/health` and the pid file report `PKG_VERSION` as tachi-server's
        // version, but the value is now tachi-bootstrap's `CARGO_PKG_VERSION`.
        // The two crates release in lockstep (scripts/check_release_versions.py);
        // this pins that the re-export never reports a different semver.
        assert_eq!(PKG_VERSION, env!("CARGO_PKG_VERSION"));
    }
}
