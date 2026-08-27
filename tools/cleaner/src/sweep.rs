use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::holder::{self, HolderEvidence};
use crate::scrap_ledger;
use crate::work_claim;
use crate::wt_clean::OutputFormat;

pub const DEFAULT_SWEEP_MAX_AGE_DAYS: u64 = 7;
const DEFAULT_MAX_DEPTH: usize = 4;

#[derive(Debug)]
pub struct SweepOptions {
    pub roots: Vec<PathBuf>,
    pub max_age_days: u64,
    pub force: bool,
    pub output: OutputFormat,
}

#[derive(Debug, serde::Serialize)]
struct SweepReport {
    action: &'static str,
    roots: Vec<String>,
    max_age_days: u64,
    dry_run: bool,
    removed: Vec<String>,
    candidates: Vec<SweepCandidate>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct SweepCandidate {
    path: String,
    repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    marker_path: String,
    age_days: u64,
    /// True whenever the OS-view holder probe is NOT `Clear` — i.e. a live
    /// attributed process holder OR inconclusive evidence. Fail-closed
    /// (tachi#1118 freeze boundary 1): a probe that could not run/parse
    /// (missing `lsof`, permission denial) must never be read as "not
    /// active" the way the prior bare-bool check silently did.
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    holder_evidence: Option<String>,
    /// Fail-closed: true unless `git status --porcelain` positively proves
    /// the worktree has no outstanding changes (besides the marker file).
    /// A candidate we cannot verify as clean is treated as dirty.
    dirty: bool,
}

pub fn run_sweep(options: SweepOptions) -> Result<(), String> {
    let roots = if options.roots.is_empty() {
        default_roots()
    } else {
        options.roots
    };
    let mut report = plan_sweep(
        &roots,
        Duration::from_secs(options.max_age_days * 24 * 60 * 60),
        options.max_age_days,
        !options.force,
        SystemTime::now(),
    );
    if options.force && report.errors.is_empty() {
        execute_sweep(&mut report);
    }

    emit_report(&report, options.output)?;
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("; "))
    }
}

fn plan_sweep(
    roots: &[PathBuf],
    max_age: Duration,
    max_age_days: u64,
    dry_run: bool,
    now: SystemTime,
) -> SweepReport {
    let mut report = SweepReport {
        action: "sweep",
        roots: roots
            .iter()
            .map(|root| root.display().to_string())
            .collect(),
        max_age_days,
        dry_run,
        removed: Vec::new(),
        candidates: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    for root in roots {
        collect_marker_candidates(root, max_age, now, &mut report);
    }

    if report.candidates.is_empty() {
        report
            .warnings
            .push("no stale Tachi-managed worktrees found".to_string());
    }

    report
}

fn collect_marker_candidates(
    root: &Path,
    max_age: Duration,
    now: SystemTime,
    report: &mut SweepReport,
) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let marker = dir.join(".tachi-worktree.json");
        if marker.is_file() {
            if let Some(candidate) = candidate_from_marker(&dir, &marker, max_age, now) {
                report.candidates.push(candidate);
            }
            continue;
        }
        if depth >= DEFAULT_MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() && !is_skip_dir(&path) {
                stack.push((path, depth + 1));
            }
        }
    }
}

fn candidate_from_marker(
    worktree_root: &Path,
    marker_path: &Path,
    max_age: Duration,
    now: SystemTime,
) -> Option<SweepCandidate> {
    let metadata = std::fs::symlink_metadata(worktree_root).ok()?;
    let modified = metadata.modified().ok()?;
    let age = now.duration_since(modified).ok()?;
    if age < max_age {
        return None;
    }

    let marker = std::fs::read_to_string(marker_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let repo_root = marker
        .as_ref()
        .and_then(|value| value.get("repo_root"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let branch = marker
        .as_ref()
        .and_then(|value| value.get("branch"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    let evidence = holder::probe_holders(worktree_root);
    let active = !matches!(evidence, HolderEvidence::Clear);
    let holder_evidence = active.then(|| evidence.describe_family());

    Some(SweepCandidate {
        path: worktree_root.display().to_string(),
        repo_root,
        branch,
        marker_path: marker_path.display().to_string(),
        age_days: age.as_secs() / (24 * 60 * 60),
        active,
        holder_evidence,
        dirty: worktree_is_dirty(worktree_root),
    })
}

/// Same dirty guard as the direct `wt-remove` close path
/// (`wt_clean::dirty_entries_excluding_marker`) so sweep/reclaim can never
/// clobber uncommitted work just because a marker aged out. Anything we
/// cannot positively verify as clean (git status fails, not a worktree,
/// etc.) is treated as dirty — fail closed, never fail open on a delete path.
fn worktree_is_dirty(path: &Path) -> bool {
    match crate::wt_clean::dirty_entries_excluding_marker(path) {
        Ok(entries) => !entries.is_empty(),
        Err(_) => true,
    }
}

fn execute_sweep(report: &mut SweepReport) {
    for candidate in &report.candidates {
        if candidate.active {
            report.warnings.push(format!(
                "skipped worktree with a live/inconclusive OS-view process holder {}: {}",
                candidate.path,
                candidate
                    .holder_evidence
                    .as_deref()
                    .unwrap_or("(no attribution captured)")
            ));
            continue;
        }
        // Ownership to reclaim = marker AND clean AND not-active. `--force`
        // only waives the age/dry-run gate above it, never this one: a
        // dirty worktree is NEVER removed by sweep, no override.
        if candidate.dirty {
            report.warnings.push(format!(
                "skipped dirty worktree {} (uncommitted changes; commit, stash, or discard before reclaim)",
                candidate.path
            ));
            continue;
        }
        let Some(repo_root) = &candidate.repo_root else {
            report.errors.push(format!(
                "missing repo_root in marker for {}",
                candidate.path
            ));
            continue;
        };
        // CP1/CP2 defense-in-depth: re-check dirtiness AND holder evidence
        // immediately before removal rather than trusting only the
        // snapshot taken during candidate collection, shrinking (not
        // closing) the window between "we decided this is clean/unheld"
        // and "we ran `git worktree remove`". A same-user check-then-act
        // race in that shrunk window is accepted residual risk for this
        // single-user local tool; full TOCTOU-safety (locking / openat) is
        // deliberately out of scope.
        if worktree_is_dirty(Path::new(&candidate.path)) {
            report.warnings.push(format!(
                "skipped dirty worktree {} (became dirty since snapshot; commit, stash, or discard before reclaim)",
                candidate.path
            ));
            continue;
        }
        let recheck = holder::probe_holders(Path::new(&candidate.path));
        if !matches!(recheck, HolderEvidence::Clear) {
            report.warnings.push(format!(
                "skipped worktree {} (a live/inconclusive OS-view process holder appeared since snapshot): {}",
                candidate.path,
                recheck.describe_family()
            ));
            continue;
        }
        // The persisted holder ledger is independent from the OS probe above.
        // Do not infer a clear answer when this CLI cannot resolve its runtime
        // DB/home: every non-clear/non-legacy result is a loud skip.
        let db_evidence = work_claim::probe_worktree_holder(Path::new(&candidate.path));
        if let Some(reason) = db_evidence.refusal_reason() {
            report
                .warnings
                .push(format!("skipped worktree {} ({reason})", candidate.path));
            continue;
        }

        // Fail-closed integrity boundary (tachi#1212 fix-round, codex
        // checkpoint 4/5), same discipline as the direct wt-remove path:
        // record the scrap BEFORE the destructive removal and abort this
        // candidate (never remove) if we can't. Also canonicalize the path
        // before recording it — sweep's candidate.path is the raw spelling
        // collected while walking the scan roots (e.g. `/tmp/tachi-123`),
        // while wt-open's re-entry lookup canonicalizes before consulting
        // the ledger (`wt_open::canonicalize_prefix`; on macOS `/tmp` ->
        // `/private/tmp`). Recording the raw spelling here would silently
        // defeat the re-entry gate for anything reached through a
        // symlinked root.
        let Some(branch) = &candidate.branch else {
            report.warnings.push(format!(
                "skipped {} (marker had no branch field; the scrap ledger could not be written \
                 ahead of a destructive removal, tachi#1118 fail-closed integrity boundary)",
                candidate.path
            ));
            continue;
        };
        let canonical_path = match std::fs::canonicalize(&candidate.path) {
            Ok(p) => p,
            Err(err) => {
                report.warnings.push(format!(
                    "skipped {} (could not canonicalize path before scrap-ledger recording: \
                     {err}; fail-closed rather than remove a tree the re-entry gate cannot \
                     remember, tachi#1118)",
                    candidate.path
                ));
                continue;
            }
        };
        let removal_claim = match work_claim::claim_worktree_removal(&canonical_path) {
            Ok(claim) => claim,
            Err(err) => {
                report.warnings.push(format!(
                    "skipped {} (could not atomically claim ExecEnv removal: {err})",
                    candidate.path
                ));
                continue;
            }
        };
        let reclaimed_bytes = match work_claim::measure_worktree_bytes(&canonical_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                report.warnings.push(format!(
                    "skipped {} (could not measure worktree before deletion: {err})",
                    candidate.path
                ));
                if let Err(abort_error) = removal_claim.abort() {
                    report.errors.push(format!(
                        "ExecEnv removal claim abort failed for {}; lease remains fail-closed: {abort_error}",
                        candidate.path
                    ));
                }
                continue;
            }
        };
        if let Err(err) = scrap_ledger::record_scrap(&canonical_path, branch) {
            report.warnings.push(format!(
                "skipped {} (scrap ledger write failed: {err}; fail-closed rather than remove a \
                 tree the re-entry gate cannot remember, tachi#1118)",
                candidate.path
            ));
            if let Err(abort_error) = removal_claim.abort() {
                report.errors.push(format!(
                    "ExecEnv removal claim abort failed for {}; lease remains fail-closed: {abort_error}",
                    candidate.path
                ));
            }
            continue;
        }

        match Command::new("git")
            .args([
                "-C",
                repo_root,
                "worktree",
                "remove",
                "--force",
                &candidate.path,
            ])
            .output()
        {
            Ok(out) if out.status.success() => {
                report.removed.push(candidate.path.clone());
                if let Err(err) = removal_claim.complete(reclaimed_bytes) {
                    report.errors.push(format!(
                        "worktree {} was removed but ExecEnv removal completion failed; lease remains fail-closed: {err}",
                        candidate.path
                    ));
                }
                let _ = Command::new("git")
                    .args(["-C", repo_root, "worktree", "prune"])
                    .output();
                if !branch.is_empty()
                    && branch != "HEAD"
                    && branch != "main"
                    && branch != "master"
                    && branch != "trunk"
                {
                    let _ = Command::new("git")
                        .args(["-C", repo_root, "branch", "-D", branch])
                        .output();
                    let _ = Command::new("git")
                        .args(["-C", repo_root, "push", "origin", "--delete", branch])
                        .output();
                }
            }
            Ok(out) => {
                if let Err(abort_error) = removal_claim.abort() {
                    report.errors.push(format!(
                        "ExecEnv removal claim abort failed for {}; lease remains fail-closed: {abort_error}",
                        candidate.path
                    ));
                }
                report.errors.push(format!(
                    "git worktree remove failed for {}: {}",
                    candidate.path,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
                report.warnings.push(format!(
                    "the scrap ledger was already recorded for {} before this failed removal; \
                     it is now flagged as scrapped even though the tree is still present and \
                     untouched",
                    candidate.path
                ));
            }
            Err(err) => {
                if let Err(abort_error) = removal_claim.abort() {
                    report.errors.push(format!(
                        "ExecEnv removal claim abort failed for {}; lease remains fail-closed: {abort_error}",
                        candidate.path
                    ));
                }
                report.errors.push(format!(
                    "failed to run git worktree remove for {}: {err}",
                    candidate.path
                ));
                report.warnings.push(format!(
                    "the scrap ledger was already recorded for {} before this failed removal; \
                     it is now flagged as scrapped even though the tree is still present and \
                     untouched",
                    candidate.path
                ));
            }
        }
    }
    if report.errors.is_empty() {
        report.dry_run = false;
    }
}

fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        roots.push(PathBuf::from(tmpdir));
    }
    roots.push(PathBuf::from("/private/tmp"));
    roots.push(std::env::temp_dir());
    // Managed worktree root (#484): default open placement + GC scan target.
    // If HOME/USERPROFILE and TACHI_WORKTREES_ROOT are both unset, there is
    // no safe managed root to scan (see wt_open::default_worktrees_root's
    // doc comment on why it refuses to fall back to cwd); just omit that
    // candidate root rather than propagating the error into every sweep.
    if let Ok(managed_root) = crate::wt_open::default_worktrees_root() {
        roots.push(managed_root);
    }
    roots.sort();
    roots.dedup();
    roots
}

fn is_skip_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "target" | "node_modules" | ".venv")
    )
}

fn emit_report(report: &SweepReport, output: OutputFormat) -> Result<(), String> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|err| format!("serialize report: {err}"))?
        ),
        OutputFormat::Text => {
            let mode = if report.dry_run { "dry-run" } else { "force" };
            println!("tachi-clean sweep ({mode})");
            println!("  max_age_days: {}", report.max_age_days);
            for root in &report.roots {
                println!("  root: {root}");
            }
            for candidate in &report.candidates {
                println!(
                    "  candidate: {} age_days={} active={} repo_root={}",
                    candidate.path,
                    candidate.age_days,
                    candidate.active,
                    candidate.repo_root.as_deref().unwrap_or("")
                );
            }
            for removed in &report.removed {
                println!("  removed: {removed}");
            }
            for warning in &report.warnings {
                println!("  warning: {warning}");
            }
            for error in &report.errors {
                println!("  error: {error}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_finds_only_old_marked_worktrees() {
        let root = unique_temp_dir("tachi-clean-sweep-test");
        let old = root.join("old/wt");
        let fresh = root.join("fresh/wt");
        let unmarked = root.join("old/unmarked");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::create_dir_all(&unmarked).unwrap();
        write_marker(&old, "/repo");
        write_marker(&fresh, "/repo");

        let now = SystemTime::now() + Duration::from_secs(10 * 24 * 60 * 60);
        let report = plan_sweep(
            std::slice::from_ref(&root),
            Duration::from_secs(7 * 24 * 60 * 60),
            7,
            true,
            now,
        );

        assert_eq!(report.candidates.len(), 2);
        assert!(report
            .candidates
            .iter()
            .any(|candidate| candidate.path.ends_with("old/wt")));
        assert!(report
            .candidates
            .iter()
            .any(|candidate| candidate.path.ends_with("fresh/wt")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_skips_target_and_node_modules_dirs() {
        assert!(is_skip_dir(Path::new("target")));
        assert!(is_skip_dir(Path::new("node_modules")));
        assert!(!is_skip_dir(Path::new("worktree")));
    }

    fn write_marker(path: &Path, repo_root: &str) {
        std::fs::write(
            path.join(".tachi-worktree.json"),
            serde_json::json!({
                "path": path.display().to_string(),
                "repo_root": repo_root,
                "branch": "feature/test",
            })
            .to_string(),
        )
        .unwrap();
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
