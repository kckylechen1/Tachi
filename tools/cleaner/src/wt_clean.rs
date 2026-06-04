use std::path::{Path, PathBuf};
use std::process::Command;

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
    dry_run: bool,
    removed: bool,
    allowed: bool,
    warnings: Vec<String>,
    errors: Vec<String>,
}

pub fn run_wt_remove(options: WtRemoveOptions) -> Result<(), String> {
    let report = plan_wt_remove(&options.path, !options.force);
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

fn plan_wt_remove(path: &Path, dry_run: bool) -> WtRemoveReport {
    let mut report = WtRemoveReport {
        action: "wt-remove",
        path: path.display().to_string(),
        canonical_path: None,
        repo_root: None,
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

    let worktree_root = match git_output(&[
        "-C",
        &canonical_string(&canonical),
        "rev-parse",
        "--show-toplevel",
    ]) {
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

    if paths_equal(&worktree_root, &repo_root) {
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

    match active_processes(&worktree_root) {
        ActiveProcessCheck::Clear => {}
        ActiveProcessCheck::Unavailable(reason) => {
            report
                .warnings
                .push(format!("active process check unavailable: {reason}"));
        }
        ActiveProcessCheck::Active(lines) => {
            report.errors.push(format!(
                "refusing to remove worktree with active processes: {}",
                lines.join(" | ")
            ));
            return report;
        }
    }

    report.allowed = true;
    report
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
    let common_dir = git_output(&[
        "-C",
        &canonical_string(worktree_root),
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
    worktree_root.join(".tachi-worktree.json").exists() || registry_contains(worktree_root)
}

fn dirty_entries_excluding_marker(worktree_root: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args([
            "-C",
            &canonical_string(worktree_root),
            "status",
            "--porcelain",
        ])
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

fn registry_contains(worktree_root: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let registry = PathBuf::from(home).join(".tachi").join("worktrees.json");
    let Ok(raw) = std::fs::read_to_string(registry) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let needle = canonical_string(worktree_root);
    value_contains_path(&value, &needle)
}

fn value_contains_path(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(s) => paths_equal(Path::new(s), Path::new(needle)),
        serde_json::Value::Array(items) => {
            items.iter().any(|item| value_contains_path(item, needle))
        }
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            (key == "path"
                && value
                    .as_str()
                    .is_some_and(|s| paths_equal(Path::new(s), Path::new(needle))))
                || value_contains_path(value, needle)
        }),
        _ => false,
    }
}

enum ActiveProcessCheck {
    Clear,
    Active(Vec<String>),
    Unavailable(String),
}

fn active_processes(path: &Path) -> ActiveProcessCheck {
    let output = Command::new("lsof").arg("+D").arg(path).output();
    match output {
        Ok(out) if out.status.success() => {
            let lines = String::from_utf8_lossy(&out.stdout)
                .lines()
                .skip(1)
                .take(5)
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            if lines.is_empty() {
                ActiveProcessCheck::Clear
            } else {
                ActiveProcessCheck::Active(lines)
            }
        }
        Ok(out) if out.status.code() == Some(1) => ActiveProcessCheck::Clear,
        Ok(out) => {
            ActiveProcessCheck::Unavailable(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
        Err(err) => ActiveProcessCheck::Unavailable(err.to_string()),
    }
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

fn paths_equal(a: &Path, b: &Path) -> bool {
    let a = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let b = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a == b
}

fn canonical_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_registry_path_is_detected() {
        let value = serde_json::json!({
            "worktrees": [
                {"path": "/tmp/a"},
                {"metadata": {"path": "/tmp/b"}}
            ]
        });

        assert!(value_contains_path(&value, "/tmp/a"));
        assert!(value_contains_path(&value, "/tmp/b"));
        assert!(!value_contains_path(&value, "/tmp/c"));
    }
}
