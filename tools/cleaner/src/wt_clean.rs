use std::path::{Path, PathBuf};
use std::process::Command;

use crate::holder::{self, HolderEvidence, HolderProbeFn};
use crate::registry;
use crate::scrap_ledger;
use crate::work_claim::{self, DbHolderProbeFn};

#[derive(Debug, Clone, Copy)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug)]
pub struct WtRemoveOptions {
    pub path: PathBuf,
    pub force: bool,
    pub output: OutputFormat,
}

#[derive(Debug, serde::Serialize)]
struct WtRemoveReport {
    action: &'static str,
    path: String,
    canonical_path: Option<String>,
    repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    dry_run: bool,
    removed: bool,
    allowed: bool,
    /// #1605: this close has no directory left to remove and only reconciles a
    /// stale registry row. Nothing destructive runs — no `git worktree
    /// remove`, no scrap-ledger record — so the field is load-bearing for the
    /// execute step, not just for the report reader.
    registry_only: bool,
    warnings: Vec<String>,
    errors: Vec<String>,
}

pub fn run_wt_remove(options: WtRemoveOptions) -> Result<(), String> {
    let report = plan_wt_remove(
        &options.path,
        !options.force,
        &work_claim::probe_worktree_holder,
        &holder::probe_holders,
    );
    let report = if options.force && report.allowed {
        execute_wt_remove(report)
    } else {
        report
    };

    emit_report(&report, options.output)?;
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("; "))
    }
}

/// Core close-predicate planning logic, parameterized over the holder
/// probe so the fail-closed-on-inconclusive-evidence branch (tachi#1118
/// freeze boundary 1) is unit-testable without shelling out to a real
/// (possibly PATH-shimmed) `lsof`. Production callers always pass
/// `&holder::probe_holders`; tests inject a fake to exercise `Held` /
/// `Unknown` deterministically.
fn plan_wt_remove(
    path: &Path,
    dry_run: bool,
    db_probe: &DbHolderProbeFn,
    probe: &HolderProbeFn,
) -> WtRemoveReport {
    let mut report = WtRemoveReport {
        action: "wt-remove",
        path: path.display().to_string(),
        canonical_path: None,
        repo_root: None,
        branch: None,
        dry_run,
        removed: false,
        allowed: false,
        registry_only: false,
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let canonical = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(err) => {
            // #1605: a registration whose directory is already gone was
            // unclosable — canonicalization ran before any registry lookup, so
            // the only documented remediation for the doctor's
            // `registered_worktree_missing` warning was to hand-edit
            // ~/.tachi/worktrees.json. A stale row is precisely the case where
            // the path cannot canonicalize, so the row is matched on its
            // stored string instead.
            return plan_stale_registry_row_close(report, path, &err.to_string());
        }
    };
    report.canonical_path = Some(canonical.display().to_string());

    let canonical_str = canonical_string(&canonical);
    let worktree_root =
        match git_output(&["-C", canonical_str.as_str(), "rev-parse", "--show-toplevel"]) {
            Ok(root) => PathBuf::from(root),
            Err(err) => {
                report.errors.push(format!("not a git worktree: {err}"));
                return report;
            }
        };
    let worktree_root = match std::fs::canonicalize(&worktree_root) {
        Ok(path) => path,
        Err(err) => {
            report
                .errors
                .push(format!("cannot canonicalize worktree root: {err}"));
            return report;
        }
    };
    report.canonical_path = Some(worktree_root.display().to_string());

    let repo_root = match repo_root_from_worktree(&worktree_root) {
        Ok(path) => path,
        Err(err) => {
            report.errors.push(err);
            return report;
        }
    };
    report.repo_root = Some(repo_root.display().to_string());

    if worktree_root == repo_root {
        report
            .errors
            .push("refusing to remove repository root/main worktree".to_string());
        return report;
    }

    if !is_registered_or_marked(&worktree_root) {
        report.warnings.push(
            "worktree is not registered in ~/.tachi/worktrees.json and has no .tachi-worktree.json marker; allowing dry-run only until dispatch registry integration lands"
                .to_string(),
        );
        if !dry_run {
            report
                .errors
                .push("refusing to remove unregistered worktree with --force".to_string());
            return report;
        }
    }

    match dirty_entries_excluding_marker(&worktree_root) {
        Ok(entries) if entries.is_empty() => {}
        Ok(entries) => {
            report.errors.push(format!(
                "refusing to remove dirty worktree; commit, stash, or discard changes first: {}",
                entries.join(" | ")
            ));
            return report;
        }
        Err(err) => {
            report
                .errors
                .push(format!("could not inspect worktree cleanliness: {err}"));
            return report;
        }
    }

    // Capture the branch now, while the worktree still exists, so a
    // successful removal can record it in the scrap ledger (tachi#1118
    // freeze boundary 3: a scrapped tree may only reopen under a NEW
    // branch + NEW path).
    report.branch = current_branch(&worktree_root).ok();

    // Durable WorkClaim evidence is a separate precondition from OS process
    // evidence.  Clear and legacy NotApplicable may proceed; every other
    // answer is a loud refusal before any destructive action is considered.
    let db_evidence = db_probe(&worktree_root);
    if let Some(reason) = db_evidence.refusal_reason() {
        report
            .errors
            .push(format!("refusing to remove worktree: {reason}"));
        return report;
    }

    // OS-view attributed process-holder evidence (tachi#1118 freeze
    // boundary 1/2). Fail-closed: `Unknown` (probe unavailable/parse
    // failure) refuses exactly like `Held` — an inconclusive OS-view
    // check must never be treated as "not held". `Clear` is the only
    // outcome that proceeds.
    let evidence = probe(&worktree_root);
    match &evidence {
        HolderEvidence::Clear => {}
        HolderEvidence::Held(_) | HolderEvidence::Unknown(_) => {
            report.errors.push(format!(
                "refusing to remove worktree with a live OS-view process holder ({}): {}",
                match &evidence {
                    HolderEvidence::Held(_) => "attributed pid family",
                    _ => "inconclusive evidence",
                },
                evidence.describe_family()
            ));
            return report;
        }
    }

    report.allowed = true;
    report
}

/// Close plan for a path that cannot be canonicalized (kckylechen1/tachi#1605).
///
/// If the registry has no row for it, the pre-existing "path does not exist"
/// error stands: there is nothing to remove and nothing to reconcile. If a row
/// IS present, the tree was removed outside this tool and only bookkeeping is
/// left, so the close is allowed and marked `registry_only` — the execute step
/// then drops the row and runs nothing destructive.
///
/// Deliberately does NOT write the scrap ledger: that record exists to gate
/// re-entry to a tree *this tool* scrapped (tachi#1118 freeze boundary 3).
/// Nothing was removed here, so inventing a scrap would newly refuse an
/// operator's reopen of a path/branch on the strength of an event this command
/// never performed.
fn plan_stale_registry_row_close(
    mut report: WtRemoveReport,
    path: &Path,
    canonicalize_error: &str,
) -> WtRemoveReport {
    let listed = match registry::find_registry_entry(path) {
        Ok(listed) => listed,
        Err(err) => {
            report.errors.push(format!(
                "path does not exist or cannot be resolved: {canonicalize_error}; \
                 the worktree registry could not be read either: {err}"
            ));
            return report;
        }
    };
    let Some(listed) = listed else {
        report.errors.push(format!(
            "path does not exist or cannot be resolved: {canonicalize_error}"
        ));
        return report;
    };

    report.canonical_path = Some(listed.path.clone());
    report.repo_root = Some(listed.repo_root.clone());
    report.branch = Some(listed.branch.clone());
    report.registry_only = true;
    report.allowed = true;
    report.warnings.push(format!(
        "directory is already gone ({canonicalize_error}); this close only drops the stale \
         registry row for branch '{}' — no git worktree remove, no scrap ledger record",
        listed.branch
    ));
    report
}

/// Current branch of a worktree (`git rev-parse --abbrev-ref HEAD`), used
/// only to populate the scrap-ledger record on a successful removal — a
/// failure to resolve it never blocks the removal itself.
fn current_branch(worktree_root: &Path) -> Result<String, String> {
    let worktree_str = canonical_string(worktree_root);
    git_output(&[
        "-C",
        worktree_str.as_str(),
        "rev-parse",
        "--abbrev-ref",
        "HEAD",
    ])
}

fn execute_wt_remove(mut report: WtRemoveReport) -> WtRemoveReport {
    let Some(path) = report.canonical_path.clone() else {
        report.errors.push("missing canonical path".to_string());
        return report;
    };
    if report.registry_only {
        // #1605 fix round (codex review): re-verify staleness at execute time
        // before touching the registry. `plan_stale_registry_row_close` ran
        // at plan time; between plan and execute something may have been
        // created at this exact path (e.g. a symlink pointed at a
        // DIFFERENT, LIVE registered worktree). If the path now resolves,
        // this row is no longer verifiably stale — refuse rather than let a
        // canonicalizing matcher merge this row into (and delete) a live
        // one. The operator re-runs planning to see current reality.
        if std::fs::canonicalize(&path).is_ok() {
            report.errors.push(format!(
                "refusing registry-only close: {path} now resolves to something on disk \
                 (it did not at plan time) — this row may no longer be stale; re-run \
                 `wt-remove` to re-plan against current state before dropping it"
            ));
            return report;
        }
        // Exact-row deletion: match the literal stored path string of the
        // planned row only, no canonicalization, no `paths_equal`. This is
        // the other half of the TOCTOU fix — even if the path above still
        // fails to canonicalize, byte-matching only the planned row's own
        // stored string guarantees at most one row is ever removed, and
        // never one that merely canonicalizes equal to it.
        match registry::remove_registry_entry_exact(&path) {
            Ok(true) => {
                report.removed = true;
                report.dry_run = false;
                if let Err(err) = append_log(&report) {
                    report
                        .warnings
                        .push(format!("cleanup log write failed: {err}"));
                }
            }
            Ok(false) => report.errors.push(format!(
                "stale registry row for {path} disappeared before it could be dropped"
            )),
            Err(err) => report
                .errors
                .push(format!("registry cleanup failed: {err}")),
        }
        return report;
    }
    let Some(repo_root) = report.repo_root.clone() else {
        report.errors.push("missing repo root".to_string());
        return report;
    };

    // Fail-closed integrity boundary (tachi#1212 fix-round, codex checkpoint
    // 4): record the scrap BEFORE the destructive `git worktree remove`,
    // and abort the removal entirely if we can't. A removal that "succeeds"
    // but leaves the re-entry gate with no memory of it is exactly the
    // fail-open the review flagged — recording first means a write failure
    // here has nothing to undo, because the removal has not happened yet.
    let Some(branch) = report.branch.clone() else {
        report.errors.push(
            "refusing to remove: could not resolve the worktree's branch before removal, so \
             the scrap ledger could not be written ahead of a destructive removal (tachi#1118 \
             fail-closed integrity boundary)"
                .to_string(),
        );
        return report;
    };
    if let Err(err) = scrap_ledger::record_scrap(Path::new(&path), &branch) {
        report.errors.push(format!(
            "refusing to remove: scrap ledger write failed ({err}); fail-closed rather than \
             remove a tree the re-entry gate cannot remember (tachi#1118)"
        ));
        return report;
    }

    match Command::new("git")
        .args(["-C", &repo_root, "worktree", "remove", "--force", &path])
        .output()
    {
        Ok(out) if out.status.success() => {
            report.removed = true;
            report.dry_run = false;
            match registry::remove_registry_entry(Path::new(&path)) {
                Ok(true) => {}
                Ok(false) => report
                    .warnings
                    .push("worktree was not present in registry".to_string()),
                Err(err) => report
                    .warnings
                    .push(format!("registry cleanup failed: {err}")),
            }
            if let Err(err) = append_log(&report) {
                report
                    .warnings
                    .push(format!("cleanup log write failed: {err}"));
            }
        }
        Ok(out) => {
            report.errors.push(format!(
                "git worktree remove failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            report.warnings.push(
                "the scrap ledger was already recorded before this failed removal; the path \
                 and branch are now flagged as scrapped even though the tree is still present \
                 and untouched — reopening either exact one will be (over-cautiously) refused \
                 until a new branch/path is used"
                    .to_string(),
            );
        }
        Err(err) => {
            report
                .errors
                .push(format!("failed to run git worktree remove: {err}"));
            report.warnings.push(
                "the scrap ledger was already recorded before this failed removal; the path \
                 and branch are now flagged as scrapped even though the tree is still present \
                 and untouched — reopening either exact one will be (over-cautiously) refused \
                 until a new branch/path is used"
                    .to_string(),
            );
        }
    }
    report
}

fn emit_report(report: &WtRemoveReport, output: OutputFormat) -> Result<(), String> {
    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(report)
                    .map_err(|err| format!("serialize report: {err}"))?
            );
        }
        OutputFormat::Text => {
            let mode = if report.dry_run { "dry-run" } else { "force" };
            println!("tachi-clean wt-remove ({mode})");
            println!(
                "  path: {}",
                report
                    .canonical_path
                    .as_deref()
                    .unwrap_or(report.path.as_str())
            );
            if let Some(repo_root) = &report.repo_root {
                println!("  repo_root: {repo_root}");
            }
            println!("  allowed: {}", report.allowed);
            println!("  removed: {}", report.removed);
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

fn repo_root_from_worktree(worktree_root: &Path) -> Result<PathBuf, String> {
    let worktree_str = canonical_string(worktree_root);
    let common_dir = git_output(&[
        "-C",
        worktree_str.as_str(),
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ])?;
    let common_dir = PathBuf::from(common_dir);
    let repo_root = common_dir
        .parent()
        .ok_or_else(|| "git common dir has no parent".to_string())?;
    std::fs::canonicalize(repo_root).map_err(|err| format!("cannot canonicalize repo root: {err}"))
}

fn is_registered_or_marked(worktree_root: &Path) -> bool {
    worktree_root.join(".tachi-worktree.json").exists()
        || registry::registry_contains(worktree_root)
}

/// `git status --porcelain` entries for `worktree_root`, excluding the
/// Tachi marker file itself. `pub(crate)` so the sweep/reclaim path
/// (`sweep.rs`) can apply the SAME dirty guard as this direct-close path
/// instead of re-deriving its own (weaker) notion of "clean".
pub(crate) fn dirty_entries_excluding_marker(worktree_root: &Path) -> Result<Vec<String>, String> {
    let worktree_str = canonical_string(worktree_root);
    let out = Command::new("git")
        .args(["-C", worktree_str.as_str(), "status", "--porcelain"])
        .output()
        .map_err(|err| format!("failed to run git status: {err}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }

    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let path = line.get(3..).unwrap_or(line).trim();
            if path == ".tachi-worktree.json" {
                None
            } else {
                Some(line.to_string())
            }
        })
        .collect())
}

fn git_output(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|err| format!("failed to run git: {err}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn append_log(report: &WtRemoveReport) -> Result<(), String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(());
    };
    let log_path = PathBuf::from(home).join(".tachi").join("cleaner.log");
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("create log dir: {err}"))?;
    }
    let mut line = serde_json::to_value(report).map_err(|err| format!("serialize log: {err}"))?;
    if let Some(obj) = line.as_object_mut() {
        obj.insert(
            "timestamp".to_string(),
            serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
        );
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|err| format!("open log: {err}"))?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&line).map_err(|err| format!("serialize log line: {err}"))?
    )
    .map_err(|err| format!("write log: {err}"))
}

fn canonical_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::holder::HolderProcess;
    use crate::registry::{self, RegisterOptions, RegisterOutputFormat};
    use crate::test_support::home_env_lock as env_lock;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct HomeGuard(Option<std::ffi::OsString>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), nanos));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// Sets up a real git repo + a real registered linked worktree so
    /// `plan_wt_remove` can be exercised end-to-end (dirty check, registry
    /// check) while only the holder probe is faked.
    fn setup_registered_worktree(root: &Path) -> (HomeGuard, PathBuf) {
        let home = root.join("home");
        let repo = root.join("repo");
        let worktree = root.join("wt");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.email", "tachi-test@example.com"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.name", "tachi-test"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        std::fs::write(repo.join("README"), "hello").unwrap();
        assert!(Command::new("git")
            .args(["add", "README"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "feature/holder-test",
                worktree.to_str().unwrap(),
            ])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());

        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        registry::run_wt_register(RegisterOptions {
            path: worktree.clone(),
            repo_root: repo.clone(),
            branch: "feature/holder-test".to_string(),
            dispatch_id: None,
            pr: None,
            output: RegisterOutputFormat::Json,
        })
        .unwrap();

        (HomeGuard(old_home), worktree)
    }

    #[test]
    fn plan_wt_remove_refuses_a_live_holder_and_names_the_pid_family() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp_dir("wt-clean-live-holder");
        let (_home_guard, worktree) = setup_registered_worktree(&root);

        let fake_held = |_path: &Path| {
            HolderEvidence::Held(vec![HolderProcess {
                pid: 987_654,
                ppid: Some(1),
                tty: Some("ttys009".to_string()),
                command: "sleep 999".to_string(),
                cwd: None,
            }])
        };

        let report = plan_wt_remove(
            &worktree,
            false,
            &|_| crate::work_claim::DbHolderEvidence::Clear,
            &fake_held,
        );
        assert!(
            !report.allowed,
            "a live attributed holder must refuse the close predicate"
        );
        let joined = report.errors.join(" | ");
        assert!(
            joined.contains("987654") || joined.contains("987_654"),
            "refusal must name the held pid: {joined}"
        );
        assert!(
            joined.contains("ppid="),
            "refusal must carry OS-view attribution (ppid), not just a raw pid: {joined}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_wt_remove_fails_closed_on_inconclusive_holder_evidence() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp_dir("wt-clean-unknown-holder");
        let (_home_guard, worktree) = setup_registered_worktree(&root);

        let fake_unknown =
            |_path: &Path| HolderEvidence::Unknown("lsof unavailable: No such file".to_string());

        let report = plan_wt_remove(
            &worktree,
            false,
            &|_| crate::work_claim::DbHolderEvidence::Clear,
            &fake_unknown,
        );
        assert!(
            !report.allowed,
            "inconclusive holder evidence must never be treated as safe to reclaim (fail-closed, tachi#1118)"
        );
        assert!(
            report.errors.join(" | ").contains("inconclusive"),
            "refusal must explain the evidence is inconclusive, not silently pass: {:?}",
            report.errors
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_wt_remove_allows_a_clean_unheld_worktree() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp_dir("wt-clean-clear-holder");
        let (_home_guard, worktree) = setup_registered_worktree(&root);

        let report = plan_wt_remove(
            &worktree,
            false,
            &|_| crate::work_claim::DbHolderEvidence::Clear,
            &|_path: &Path| HolderEvidence::Clear,
        );
        assert!(
            report.allowed,
            "a clean, unheld, registered worktree must be allowed: {:?}",
            report.errors
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn persisted_holder_refusals_are_loud_while_clear_and_legacy_proceed() {
        // RED/GREEN discrimination: without the DB gate every case below
        // reaches the Clear OS probe and is allowed. The fixed predicate only
        // permits explicit Clear and legacy NotApplicable evidence.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp_dir("wt-clean-db-holder");
        let (_home_guard, worktree) = setup_registered_worktree(&root);
        let clear_os = |_path: &Path| HolderEvidence::Clear;

        for evidence in [
            crate::work_claim::DbHolderEvidence::Held,
            crate::work_claim::DbHolderEvidence::Contradictory,
            crate::work_claim::DbHolderEvidence::Unavailable("DB locked".to_string()),
            crate::work_claim::DbHolderEvidence::Unverifiable("missing claim row".to_string()),
        ] {
            let probe_evidence = evidence.clone();
            let report = plan_wt_remove(
                &worktree,
                false,
                &move |_| probe_evidence.clone(),
                &clear_os,
            );
            assert!(
                !report.allowed,
                "{evidence:?} must refuse before OS-clear deletion"
            );
            assert!(report
                .errors
                .join(" | ")
                .contains("persisted WorkClaim holder evidence"));
        }

        for evidence in [
            crate::work_claim::DbHolderEvidence::Clear,
            crate::work_claim::DbHolderEvidence::NotApplicable,
        ] {
            let probe_evidence = evidence.clone();
            let report = plan_wt_remove(
                &worktree,
                false,
                &move |_| probe_evidence.clone(),
                &clear_os,
            );
            assert!(
                report.allowed,
                "{evidence:?} may proceed to the OS/dirty guards"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The registered path as the registry stores it — the form the doctor's
    /// `registered_worktree_missing` remediation prints, and the only form that
    /// can still be matched once the directory is gone (`canonicalize` fails,
    /// so a differently-spelled but equivalent path can no longer be resolved
    /// to it).
    fn registered_path_string(worktree: &Path) -> String {
        registry::find_registry_entry(worktree)
            .expect("read registry")
            .expect("worktree is registered")
            .path
    }

    /// #1605: a registration whose directory was removed by hand could not be
    /// closed at all — `plan_wt_remove` canonicalized before it ever looked at
    /// the registry, so the stale row survived every documented remediation.
    #[test]
    fn close_drops_a_stale_registry_row_when_the_directory_is_gone() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp_dir("wt-clean-stale-row");
        let (_home_guard, worktree) = setup_registered_worktree(&root);
        let stored_path = registered_path_string(&worktree);

        // Hand-removal: the directory disappears, the registry row does not.
        std::fs::remove_dir_all(&worktree).unwrap();

        let report = plan_wt_remove(
            Path::new(&stored_path),
            false,
            &|_| crate::work_claim::DbHolderEvidence::Clear,
            &|_| HolderEvidence::Clear,
        );
        assert!(
            report.allowed,
            "a stale row must be closable: {:?}",
            report.errors
        );
        assert!(
            report.registry_only,
            "nothing destructive is left to do; only the row"
        );
        assert_eq!(report.branch.as_deref(), Some("feature/holder-test"));
        assert!(
            report.warnings.join(" | ").contains("already gone"),
            "the close must say why it is registry-only: {:?}",
            report.warnings
        );

        let report = execute_wt_remove(report);
        assert!(
            report.errors.is_empty(),
            "registry-only close must not error: {:?}",
            report.errors
        );
        assert!(report.removed, "the stale row must actually be dropped");
        assert!(
            registry::find_registry_entry(Path::new(&stored_path))
                .expect("read registry")
                .is_none(),
            "the registry row must be gone after the close"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// kckylechen1/tachi#1605 fix round (codex review): TOCTOU pin. A path
    /// planned as a stale registry-only close must be RE-VERIFIED stale at
    /// execute time. If something gets created at that exact path between
    /// plan and execute — here, a symlink pointing at a DIFFERENT, LIVE
    /// registered worktree — execute must refuse loudly rather than let the
    /// old canonicalizing matcher resolve the stale spelling onto the live
    /// row and drop it (or drop both).
    #[test]
    fn execute_refuses_registry_only_close_when_path_resolves_again_before_execute() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp_dir("wt-clean-toctou");
        let home = root.join("home");
        let repo = root.join("repo");
        let live_wt = root.join("wt-live");
        let stale_wt = root.join("wt-stale");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.email", "tachi-test@example.com"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.name", "tachi-test"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        std::fs::write(repo.join("README"), "hello").unwrap();
        assert!(Command::new("git")
            .args(["add", "README"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "feature/toctou-live",
                live_wt.to_str().unwrap(),
            ])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "feature/toctou-stale",
                stale_wt.to_str().unwrap(),
            ])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());

        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        let _home_guard = HomeGuard(old_home);

        registry::run_wt_register(RegisterOptions {
            path: live_wt.clone(),
            repo_root: repo.clone(),
            branch: "feature/toctou-live".to_string(),
            dispatch_id: None,
            pr: None,
            output: RegisterOutputFormat::Json,
        })
        .unwrap();
        registry::run_wt_register(RegisterOptions {
            path: stale_wt.clone(),
            repo_root: repo.clone(),
            branch: "feature/toctou-stale".to_string(),
            dispatch_id: None,
            pr: None,
            output: RegisterOutputFormat::Json,
        })
        .unwrap();

        let stale_stored_path = registered_path_string(&stale_wt);
        let live_stored_path = registered_path_string(&live_wt);

        // Remove the stale worktree for real (git-managed + on disk) so
        // planning against its stored path takes the stale-registry-row
        // branch (canonicalize fails).
        assert!(Command::new("git")
            .args(["worktree", "remove", "--force", stale_wt.to_str().unwrap()])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(!stale_wt.exists());

        let report = plan_wt_remove(
            Path::new(&stale_stored_path),
            false,
            &|_| crate::work_claim::DbHolderEvidence::Clear,
            &|_| HolderEvidence::Clear,
        );
        assert!(
            report.registry_only,
            "must plan as a stale registry-only row: {:?}",
            report.errors
        );

        // TOCTOU: between plan and execute, something is created at the
        // exact planned path — a symlink pointing at the LIVE registered
        // worktree, not a re-creation of the stale one.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&live_wt, &stale_wt).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&live_wt, &stale_wt).unwrap();

        let report = execute_wt_remove(report);
        assert!(
            !report.removed,
            "must not remove anything once the planned path resolves again: {report:?}"
        );
        assert!(
            !report.errors.is_empty(),
            "must refuse loudly, not silently no-op: {report:?}"
        );
        let joined = report.errors.join(" | ");
        assert!(
            joined.contains("re-run") && joined.contains("resolves"),
            "refusal must be typed and point the operator at re-planning: {joined}"
        );

        let listed = registry::list_registered_worktrees().expect("read registry");
        assert_eq!(
            listed.len(),
            2,
            "BOTH rows must survive the refused close — the stale row wasn't dropped and the \
             live row wasn't collaterally erased: {listed:?}"
        );
        assert!(
            listed.iter().any(|w| w.path == stale_stored_path),
            "the stale row must still be present: {listed:?}"
        );
        assert!(
            listed.iter().any(|w| w.path == live_stored_path),
            "the live row must not have been dropped: {listed:?}"
        );

        // Symlink first: recursive removal of `root` must not follow it
        // into `live_wt` and delete the still-registered live worktree.
        let _ = std::fs::remove_file(&stale_wt);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The fallback is a registry reconciliation, not a blanket relaxation: a
    /// path that neither exists nor is registered still refuses.
    #[test]
    fn close_still_refuses_a_missing_path_with_no_registry_row() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp_dir("wt-clean-unknown-missing");
        let (_home_guard, _worktree) = setup_registered_worktree(&root);

        let report = plan_wt_remove(
            &root.join("never-registered"),
            false,
            &|_| crate::work_claim::DbHolderEvidence::Clear,
            &|_| HolderEvidence::Clear,
        );
        assert!(!report.allowed);
        assert!(!report.registry_only);
        assert!(
            report
                .errors
                .join(" | ")
                .contains("path does not exist or cannot be resolved"),
            "unchanged refusal expected, got: {:?}",
            report.errors
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
