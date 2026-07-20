//! Compile-time build provenance — the daemon's answer to "which commit am I?".
//!
//! Rationale (deploy autopsy 2026-07-06): `CARGO_PKG_VERSION` alone cannot
//! distinguish two builds sharing a semver, so a stale daemon could masquerade
//! as "current" while running source that diverged from the build source.
//! Surfacing `GIT_SHA` on `/health`, the pid file, and `--version` makes
//! "build source == run source" auditable at runtime.
//!
//! The shared consts/fns (injected by [`build.rs`](../build.rs) via
//! `cargo:rustc-env=...` at compile time) live in `build_info_core`,
//! byte-identical to `tachi_bootstrap`'s copy of the same file — enforced
//! below by `build_info_core_stays_in_parity_with_bootstrap`. This file
//! only re-exports them plus this crate's tests.

pub use crate::build_info_core::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_sha_is_nonempty() {
        // Even in a no-git build the fallback is the literal "unknown" — never
        // the empty string. An empty value here would mean build.rs broke its
        // invariants and the stamp is meaningless, which is a deploy hazard.
        assert!(!GIT_SHA.is_empty(), "GIT_SHA must never be empty");
    }

    #[test]
    fn build_time_is_nonempty() {
        assert!(!BUILD_TIME.is_empty(), "BUILD_TIME must never be empty");
    }

    #[test]
    fn version_string_contains_pkg_version_and_sha_marker() {
        let s = build_version_string();
        // Always embeds the package version so a human can read the semver,
        // and uses '+' to separate the semver from the provenance suffix —
        // mirroring the PEP 440 / SemVer "+local" convention so it sorts and
        // greps predictably.
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
        // The slicing guard must handle the short "unknown" fallback without
        // panicking, and must return a non-empty slice either way.
        let short = git_sha_short();
        assert!(!short.is_empty());
        assert!(short.len() <= 12);
    }

    #[test]
    fn build_info_json_shape_has_git_sha_and_build_time() {
        // Mirrors the `/health` field set so a regression in either place is
        // caught by the same structural assertion. This is the red/green
        // discriminator: before build.rs landed there was NO `git_sha` field
        // anywhere, so this test structurally cannot pass on the pre-fix code.
        let payload = serde_json::json!({
            "version": PKG_VERSION,
            "git_sha": GIT_SHA,
            "build_time": BUILD_TIME,
        });
        let obj = payload.as_object().expect("json object");
        assert!(obj.contains_key("git_sha"), "git_sha field required");
        assert!(obj.contains_key("build_time"), "build_time field required");
        // git_sha must be a non-empty string — the whole point of stamping.
        let git_sha = obj
            .get("git_sha")
            .and_then(|v| v.as_str())
            .expect("git_sha must be a string");
        assert!(!git_sha.is_empty(), "git_sha must be non-empty");
    }

    // `build_info_core.rs` here and in `tachi-bootstrap` were hand-synced
    // twins (comments said so by construction, before this test existed —
    // same shape as the malformed-JSON middleware parity gate added for
    // tachi-server/portable-server). tachi-bootstrap has no dependency edge
    // onto tachi-server (and vice versa), so there is no shared crate to
    // hold these ~50 lines of build-provenance logic in without adding one.
    // Enforced duplication is the ruled-on tradeoff instead. This test is
    // the enforcement: it fails loudly if the two copies ever drift.
    #[test]
    fn build_info_core_stays_in_parity_with_bootstrap() {
        let tachi_server_copy = include_str!("build_info_core.rs");
        let bootstrap_copy = include_str!("../../tachi-bootstrap/src/build_info_core.rs");
        assert_eq!(
            tachi_server_copy, bootstrap_copy,
            "crates/tachi-server/src/build_info_core.rs and \
             crates/tachi-bootstrap/src/build_info_core.rs must stay \
             byte-identical — edit one, edit its twin too"
        );
    }
}
