//! Compile-time build provenance — the binary's answer to "which commit am I?".
//!
//! Rationale (deploy autopsy 2026-07-06): `CARGO_PKG_VERSION` alone cannot
//! distinguish two builds sharing a semver, so a stale daemon could masquerade
//! as "current" while running source that diverged from the build source.
//! Surfacing `GIT_SHA` on `--version`, `/health` and the pid file makes
//! "build source == run source" auditable at runtime.
//!
//! This module is the single source of these values: `tachi_server::build_info`
//! re-exports it, so `--version` (rendered here) and the daemon's `/health`
//! report the same provenance from one build-script run.
//!
//! Values are injected by this crate's `build.rs` via `cargo:rustc-env=...`:
//!
//! * [`GIT_SHA`] — full 40-char git HEAD SHA, or `"unknown"` for a no-git
//!   (tarball/sandbox) build.
//! * [`BUILD_TIME`] — UTC timestamp, or `"unknown"`: wall-clock build time
//!   for release builds, `SOURCE_DATE_EPOCH` when set, and the HEAD commit
//!   time for non-release builds (see `build.rs`).

/// Full git HEAD SHA captured at build time, or `"unknown"` if git was not
/// available (e.g. a tarball build). This is the SAME value reported by
/// `/health`'s `git_sha` field.
pub const GIT_SHA: &str = env!("GIT_SHA");

/// UTC build timestamp captured at build time, or `"unknown"`.
pub const BUILD_TIME: &str = env!("BUILD_TIME");

/// The package version (`CARGO_PKG_VERSION`), kept here so callers have a
/// single source for "what version am I". `tachi-server` re-exports this
/// value; the two crates' versions are kept in lockstep by
/// `scripts/check_release_versions.py`.
pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A short (first 12 chars) git SHA, or the full value if shorter than 12
/// (covers the `"unknown"` fallback). 12 chars matches GitHub's abbreviated
/// SHAs and is collision-resistant enough for human-readable surfaces.
pub fn git_sha_short() -> &'static str {
    if GIT_SHA.len() >= 12 {
        &GIT_SHA[..12]
    } else {
        GIT_SHA
    }
}

/// Human-readable build identifier: `"{version}+{git_sha_short}"`.
///
/// Used by `--version` and startup logging where a single line is preferred
/// over separate fields. Example: `"1.6.4+52fdd5306a1a"`.
pub fn build_version_string() -> String {
    format!("{PKG_VERSION}+{}", git_sha_short())
}

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
}
