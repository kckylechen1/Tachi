//! Compile-time build provenance shared core.
//!
//! PARITY CONTRACT: this file must stay byte-identical to its twin —
//! `crates/tachi-server/src/build_info_core.rs` in tachi-server,
//! `crates/tachi-bootstrap/src/build_info_core.rs` in tachi-bootstrap.
//! tachi-server's `build_info_core_stays_in_parity_with_bootstrap` test
//! (`crates/tachi-server/src/build_info.rs`) enforces this by
//! `include_str!`-comparing the two files byte-for-byte at test time — edit
//! one, edit its twin too, or the test fails loudly.
//!
//! Values are injected by each crate's own `build.rs` via
//! `cargo:rustc-env=...` at compile time:
//!
//! * [`GIT_SHA`] — full 40-char git HEAD SHA, or `"unknown"` for a no-git
//!   (tarball/sandbox) build.
//! * [`BUILD_TIME`] — UTC build timestamp, or `"unknown"`.
//!
//! Both crates embed this same source, each compiled against its own
//! `env!`-injected values, so `tachi-server`'s `/health` and
//! `tachi-bootstrap`'s `--version` can report the same provenance without a
//! shared dependency edge between them.

/// Full git HEAD SHA captured at build time, or `"unknown"` if git was not
/// available (e.g. a tarball build). This is the SAME value reported by
/// `/health`'s `git_sha` field.
pub const GIT_SHA: &str = env!("GIT_SHA");

/// UTC build timestamp captured at build time, or `"unknown"`.
pub const BUILD_TIME: &str = env!("BUILD_TIME");

/// The package version (`CARGO_PKG_VERSION`), kept here so callers have a
/// single source for "what version am I".
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
