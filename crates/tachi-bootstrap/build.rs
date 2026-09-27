//! Build script — compile-time build provenance stamping.
//!
//! Captures the git HEAD SHA and a UTC build timestamp so the binary can
//! self-report exactly which commit it was built from. This is the root-cause
//! fix for the 2026-07-06 deploy autopsy: `CARGO_PKG_VERSION` alone could not
//! distinguish two builds sharing a semver, so a stale daemon could masquerade
//! as "current". With `GIT_SHA` baked in, `build source == run source` is
//! auditable via `--version`, `/health` and the pid file.
//!
//! This is the ONLY provenance build script: `tachi-server` re-exports
//! `tachi_bootstrap::build_info` instead of stamping its own copy, so the
//! `--version` output and the daemon's `/health` report the same values.
//!
//! Intentionally hand-rolled (`std::process::Command`), NOT `vergen`, to avoid
//! a new dependency. A missing VCS (tarball build) MUST NOT fail the build —
//! every value falls back to the literal `"unknown"`.
//!
//! Rebuild hygiene: the rerun triggers point at the REAL git files (resolved
//! through `git rev-parse --git-dir` / `--git-common-dir`, so a linked
//! worktree whose `.git` is a file works), never at a path that may not exist
//! — Cargo treats a missing `rerun-if-changed` path as permanently stale and
//! would re-run this script, and recompile every dependent, on every cargo
//! invocation. `BUILD_TIME` is only a wall-clock value for release builds;
//! see [`build_time`].

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    // ── GIT_SHA ──────────────────────────────────────────────────────────
    // `git rev-parse HEAD` → full 40-char SHA. Fallback "unknown" covers:
    //   * builds outside any git checkout (tarballs, some sandboxes)
    //   * git not on PATH
    //   * non-utf8 / empty output
    let git_sha = capture("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_SHA={git_sha}");

    // ── BUILD_TIME ───────────────────────────────────────────────────────
    println!("cargo:rustc-env=BUILD_TIME={}", build_time());

    // ── Rerun triggers ───────────────────────────────────────────────────
    for path in git_rerun_paths() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// The `BUILD_TIME` stamp, in `YYYY-MM-DDTHH:MM:SSZ` (UTC) or `"unknown"`.
///
/// * `SOURCE_DATE_EPOCH` set (reproducible-build convention): that instant.
/// * Release profile (`PROFILE=release`, which also covers custom profiles
///   inheriting from `release`): the wall-clock build time — the value the
///   shipped `--version` / `/health` surfaces have always reported.
/// * Any other profile: the HEAD commit time. It is stable for a given
///   `GIT_SHA`, so a re-run of this script without a commit change (a ref
///   pack, a branch switch to the same commit) emits byte-identical output
///   instead of a new per-second value that forces dependents to recompile.
fn build_time() -> String {
    if let Some(epoch) = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
    {
        return format_utc(epoch);
    }
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        return capture("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])
            .unwrap_or_else(|| "unknown".to_string());
    }
    capture("git", &["log", "-1", "--format=%ct", "HEAD"])
        .and_then(|v| v.parse::<i64>().ok())
        .map(format_utc)
        .unwrap_or_else(|| "unknown".to_string())
}

/// Format a Unix timestamp as `YYYY-MM-DDTHH:MM:SSZ` (UTC), matching
/// `date -u +%Y-%m-%dT%H:%M:%SZ`. Uses Howard Hinnant's `civil_from_days`,
/// so no date crate and no GNU/BSD `date -d`/`-r` divergence.
fn format_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// The existing files/directories whose change means HEAD may now resolve to
/// a different commit. Empty outside a git checkout (then only `build.rs`
/// itself triggers a re-run, which is correct: the stamp is `"unknown"`).
///
/// * `<git-dir>/HEAD` — per-worktree; changes on checkout / detached commit.
/// * The loose ref HEAD points at (`<git-common-dir>/refs/heads/<branch>`),
///   which changes on every commit to that branch. When the ref is only
///   packed (no loose file yet), watch its nearest existing parent directory
///   under `refs/` instead, so the first loose write is still seen.
/// * `<git-common-dir>/packed-refs`, when present.
///
/// Every emitted path exists at emit time; the whole `refs/` tree is never
/// watched, so a `git fetch` touching `refs/remotes` does not trigger a
/// rebuild.
fn git_rerun_paths() -> Vec<PathBuf> {
    let Some(git_dir) = capture("git", &["rev-parse", "--git-dir"]) else {
        return Vec::new();
    };
    let common_dir =
        capture("git", &["rev-parse", "--git-common-dir"]).unwrap_or_else(|| git_dir.clone());
    // Relative results are relative to the build script's cwd (the manifest
    // dir); `join` keeps an absolute result as-is.
    let cwd = std::env::current_dir().unwrap_or_default();
    let git_dir = cwd.join(git_dir);
    let common_dir = cwd.join(common_dir);

    let mut paths = Vec::new();
    push_if_exists(&mut paths, git_dir.join("HEAD"));

    if let Some(symref) = capture("git", &["symbolic-ref", "-q", "HEAD"]) {
        let refs_root = common_dir.join("refs");
        let mut candidate = common_dir.join(&symref);
        while !candidate.exists() && candidate.starts_with(&refs_root) && candidate != refs_root {
            match candidate.parent() {
                Some(parent) => candidate = parent.to_path_buf(),
                None => break,
            }
        }
        if candidate.starts_with(&refs_root) {
            push_if_exists(&mut paths, candidate);
        }
    }

    push_if_exists(&mut paths, common_dir.join("packed-refs"));
    paths
}

fn push_if_exists(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() {
        paths.push(path);
    }
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
