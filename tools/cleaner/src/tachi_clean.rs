use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::wt_clean::OutputFormat;

const DEFAULT_MAX_AGE_DAYS: u64 = 7;
const BACKUPS_TO_KEEP: usize = 2;

#[derive(Debug)]
pub struct TachiCleanOptions {
    pub home: Option<PathBuf>,
    pub force: bool,
    pub output: OutputFormat,
}

#[derive(Debug, serde::Serialize)]
struct TachiCleanReport {
    action: &'static str,
    home: String,
    dry_run: bool,
    removed: Vec<String>,
    candidates: Vec<TachiCleanCandidate>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct TachiCleanCandidate {
    path: String,
    kind: &'static str,
    reason: String,
    bytes: u64,
}

pub fn run_tachi_clean(options: TachiCleanOptions) -> Result<(), String> {
    let mut report = plan_tachi_clean(
        options.home.as_deref(),
        !options.force,
        SystemTime::now(),
        Duration::from_secs(DEFAULT_MAX_AGE_DAYS * 24 * 60 * 60),
    );
    if options.force && report.errors.is_empty() {
        execute_tachi_clean(&mut report);
    }

    emit_report(&report, options.output)?;
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("; "))
    }
}

fn plan_tachi_clean(
    home_override: Option<&Path>,
    dry_run: bool,
    now: SystemTime,
    max_age: Duration,
) -> TachiCleanReport {
    let home = resolve_tachi_home(home_override);
    let mut report = TachiCleanReport {
        action: "tachi-clean",
        home: home.display().to_string(),
        dry_run,
        removed: Vec::new(),
        candidates: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    if !home.exists() {
        report
            .warnings
            .push("Tachi home does not exist; nothing to clean".to_string());
        return report;
    }

    collect_backup_candidates(&home.join("cleanup-backups"), &mut report);
    for (rel_path, kind) in [
        ("logs", "log"),
        ("runs", "run"),
        (".agent/claude-code-runs", "claude-code-run"),
    ] {
        collect_age_candidates(&home.join(rel_path), kind, now, max_age, &mut report);
    }

    if report.candidates.is_empty() {
        report
            .warnings
            .push("no cleanable Tachi artifacts found".to_string());
    }

    report
}

fn collect_backup_candidates(path: &Path, report: &mut TachiCleanReport) {
    let Ok(entries) = list_child_paths(path) else {
        return;
    };
    let mut entries = entries;
    entries.sort_by_key(|path| Reverse(file_name_string(path)));
    for entry in entries.into_iter().skip(BACKUPS_TO_KEEP) {
        report.candidates.push(TachiCleanCandidate {
            bytes: path_size(&entry),
            path: entry.display().to_string(),
            kind: "cleanup-backup",
            reason: format!("older than latest {BACKUPS_TO_KEEP} backup(s)"),
        });
    }
}

fn collect_age_candidates(
    path: &Path,
    kind: &'static str,
    now: SystemTime,
    max_age: Duration,
    report: &mut TachiCleanReport,
) {
    let Ok(entries) = list_child_paths(path) else {
        return;
    };
    for entry in entries {
        let Ok(metadata) = std::fs::symlink_metadata(&entry) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age >= max_age {
            report.candidates.push(TachiCleanCandidate {
                bytes: path_size(&entry),
                path: entry.display().to_string(),
                kind,
                reason: format!("older than {} day(s)", DEFAULT_MAX_AGE_DAYS),
            });
        }
    }
}

fn execute_tachi_clean(report: &mut TachiCleanReport) {
    for candidate in &report.candidates {
        let path = Path::new(&candidate.path);
        match remove_path(path) {
            Ok(()) => report.removed.push(candidate.path.clone()),
            Err(err) => report
                .errors
                .push(format!("failed to remove {}: {err}", candidate.path)),
        }
    }
    if report.errors.is_empty() {
        report.dry_run = false;
    }
}

fn resolve_tachi_home(home_override: Option<&Path>) -> PathBuf {
    if let Some(home) = home_override {
        return home.to_path_buf();
    }
    if let Some(home) = std::env::var_os("TACHI_HOME") {
        return PathBuf::from(home);
    }
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".tachi")
}

fn list_child_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let entries =
        std::fs::read_dir(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    Ok(entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect())
}

fn file_name_string(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn path_size(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }

    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| path_size(&entry.path()))
        .sum()
}

fn emit_report(report: &TachiCleanReport, output: OutputFormat) -> Result<(), String> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|err| format!("serialize report: {err}"))?
        ),
        OutputFormat::Text => {
            let mode = if report.dry_run { "dry-run" } else { "force" };
            println!("tachi-clean tachi ({mode})");
            println!("  home: {}", report.home);
            for candidate in &report.candidates {
                println!(
                    "  candidate: {} kind={} bytes={} reason={}",
                    candidate.path, candidate.kind, candidate.bytes, candidate.reason
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
    fn tachi_clean_keeps_two_latest_backups_by_name() {
        let home = unique_temp_dir("tachi-clean-home-backups");
        let backups = home.join("cleanup-backups");
        for name in ["2026-01-01", "2026-01-02", "2026-01-03"] {
            std::fs::create_dir_all(backups.join(name)).unwrap();
        }

        let report = plan_tachi_clean(Some(&home), true, SystemTime::now(), Duration::ZERO);

        assert_eq!(report.candidates.len(), 1);
        assert!(report.candidates[0].path.ends_with("2026-01-01"));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn tachi_clean_removes_planned_candidates_but_keeps_recent_runs() {
        let home = unique_temp_dir("tachi-clean-home-execute");
        let backups = home.join("cleanup-backups");
        for name in ["2026-01-01", "2026-01-02", "2026-01-03"] {
            std::fs::create_dir_all(backups.join(name)).unwrap();
        }
        let runs = home.join("runs");
        std::fs::create_dir_all(runs.join("run-1")).unwrap();

        let mut report = plan_tachi_clean(Some(&home), true, SystemTime::now(), Duration::ZERO);
        execute_tachi_clean(&mut report);

        assert!(report.errors.is_empty());
        assert!(!backups.join("2026-01-01").exists());
        assert!(backups.join("2026-01-02").exists());
        assert!(backups.join("2026-01-03").exists());
        assert!(!runs.join("run-1").exists());

        let _ = std::fs::remove_dir_all(&home);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
