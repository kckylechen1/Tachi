use std::path::{Path, PathBuf};
use std::process::Command;

use crate::holder::{self, HolderEvidence, HolderProbeFn};
use crate::registry;
use crate::scrap_ledger;

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
    warnings: Vec<String>,
    errors: Vec<String>,
}

pub fn run_wt_remove(options: WtRemoveOptions) -> Result<(), String> {
    let report = plan_wt_remove(&options.path, !options.force, &holder::probe_holders);
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
fn plan_wt_remove(path: &Path, dry_run: bool, probe: &HolderProbeFn) -> WtRemoveReport {
    let mut report = WtRemoveReport {
        action: "wt-remove",
        path: path.display().to_string(),
        canonical_path: None,
        repo_root: None,
        branch: None,
        dry_run,
        removed: false,
        allowed: false,
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let canonical = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(err) => {
            report
                .errors
                .push(format!("path does not exist or cannot be resolved: {err}"));
            return report;
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
    let Some(repo_root) = report.repo_root.clone() else {
        report.errors.push("missing repo root".to_string());
        return report;
    };

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
            // Scrap ledger (tachi#1118 freeze boundary 3): record path+branch
            // so `wt-open` can refuse a future same-path re-entry. Best-effort
            // — a ledger write failure never undoes a removal that already
            // happened, it only weakens the re-entry gate's memory.
            if let Some(branch) = report.branch.clone() {
                if let Err(err) = scrap_ledger::record_scrap(Path::new(&path), &branch) {
                    report
                        .warnings
                        .push(format!("scrap ledger write failed: {err}"));
                }
            } else {
                report.warnings.push(
                    "scrap ledger not recorded: could not resolve the worktree's branch before removal"
                        .to_string(),
                );
            }
            if let Err(err) = append_log(&report) {
                report
                    .warnings
                    .push(format!("cleanup log write failed: {err}"));
            }
        }
        Ok(out) => report.errors.push(format!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(err) => report
            .errors
            .push(format!("failed to run git worktree remove: {err}")),
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
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

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

        let report = plan_wt_remove(&worktree, false, &fake_held);
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

        let report = plan_wt_remove(&worktree, false, &fake_unknown);
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

        let report = plan_wt_remove(&worktree, false, &|_path: &Path| HolderEvidence::Clear);
        assert!(
            report.allowed,
            "a clean, unheld, registered worktree must be allowed: {:?}",
            report.errors
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
