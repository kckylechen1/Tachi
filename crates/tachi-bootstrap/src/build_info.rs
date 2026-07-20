//! Compile-time build provenance for the `--version` CLI surface.
//!
//! This is the twin of `tachi_server::build_info`; the two crates share the
//! same `GIT_SHA` so the binary's `--version` and the daemon's `/health`
//! report the same provenance.
//!
//! The shared consts/fns live in `build_info_core`, byte-identical between
//! the two crates (enforced by
//! `tachi_server::build_info::build_info_core_stays_in_parity_with_bootstrap`,
//! which `include_str!`-compares
//! `crates/tachi-server/src/build_info_core.rs` against
//! `crates/tachi-bootstrap/src/build_info_core.rs`) — this file only
//! re-exports them plus this crate's own coverage of that surface.

pub use crate::build_info_core::*;

#[cfg(test)]
mod tests {
    use super::*;

    // No prior test coverage existed for tachi-bootstrap's own build_info
    // (tachi-server has the equivalent assertions for its copy, in
    // `crates/tachi-server/src/build_info.rs`). Same shape checks here.

    #[test]
    fn version_string_contains_pkg_version_and_sha_marker() {
        let s = build_version_string();
        assert!(
            s.contains(PKG_VERSION),
            "build_version_string must contain the package version: {s}"
        );
        assert!(
            s.contains('+'),
            "build_version_string must separate version and sha with '+': {s}"
        );
    }

    #[test]
    fn git_sha_short_never_panics_on_unknown() {
        let short = git_sha_short();
        assert!(!short.is_empty());
        assert!(short.len() <= 12);
    }
}
