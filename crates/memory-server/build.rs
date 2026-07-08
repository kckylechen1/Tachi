//! Build script — compile-time build provenance stamping.
//!
//! Captures the git HEAD SHA and a UTC build timestamp so the running daemon
//! can self-report exactly which commit it was built from. This is the
//! root-cause fix for the 2026-07-06 deploy autopsy: `CARGO_PKG_VERSION` alone
//! could not distinguish two builds sharing a semver, so a stale daemon could
//! masquerade as "current". With `GIT_SHA` baked in, `build source == run
//! source` is auditable via `/health` and the pid file.
//!
//! Intentionally hand-rolled (`std::process::Command`), NOT `vergen`, to avoid
//! a new dependency. A missing VCS (tarball build) MUST NOT fail the build —
//! every value falls back to the literal `"unknown"`.
//!
//! A twin of this script lives at `crates/tachi-bootstrap/build.rs` so the
//! `--version` CLI output (rendered by `tachi-bootstrap`) can show the same
//! sha; keep the two in sync.

fn main() {
    // ── GIT_SHA ──────────────────────────────────────────────────────────
    // `git rev-parse HEAD` → full 40-char SHA. Fallback "unknown" covers:
    //   * builds outside any git checkout (tarballs, some sandboxes)
    //   * git not on PATH
    //   * non-utf8 / empty output
    let git_sha = capture("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_SHA={git_sha}");

    // ── BUILD_TIME ───────────────────────────────────────────────────────
    // Coarse "when was this compiled" stamp. `date -u` is universally present
    // on the Unix deploy targets (macOS/Linux bottles); fallback "unknown".
    let build_time =
        capture("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BUILD_TIME={build_time}");

    // Re-stamp when the checked-out commit changes. Paths are relative to this
    // crate's manifest dir (`crates/memory-server`), so the repo `.git` is two
    // levels up. In a worktree `.git` is a file and these may be no-ops, which
    // is fine: CI/deploy is a fresh clone (real `.git/HEAD`) where this fires
    // correctly, and a stale local stamp still falls through to the real SHA
    // on the next clean build.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}

/// Run `cmd` with `args`, return the trimmed stdout on success (exit 0 +
/// non-empty utf8), else `None`. Never panics — a build-script failure would
/// break no-git builds.
fn capture(cmd: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(cmd).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
