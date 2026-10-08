use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::holder::{self, HolderEvidence};
use crate::wt_clean::OutputFormat;

const DEFAULT_MAX_AGE_DAYS: u64 = 7;

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
    /// Lossless filesystem path; `home` is only its display form, which can
    /// differ for non-UTF-8 components.
    #[serde(skip)]
    home_path: PathBuf,
    #[serde(skip)]
    home_identity: Option<PathSnapshot>,
    #[serde(skip)]
    logs_identity: Option<PathSnapshot>,
    #[serde(skip)]
    max_age: Duration,
}

#[derive(Debug, serde::Serialize)]
struct TachiCleanCandidate {
    path: String,
    /// Lossless filesystem path; `path` is only its display form.
    #[serde(skip)]
    fs_path: PathBuf,
    kind: &'static str,
    reason: String,
    bytes: u64,
    #[serde(skip)]
    snapshot: PathSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathSnapshot {
    device: u64,
    inode: u64,
    directory: bool,
    modified: SystemTime,
    bytes: u64,
}

fn snapshot(path: &Path) -> std::io::Result<PathSnapshot> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(std::io::Error::other(
            "symlink or special entry is retained",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(PathSnapshot {
            device: metadata.dev(),
            inode: metadata.ino(),
            directory: metadata.is_dir(),
            modified: metadata.modified()?,
            bytes: metadata.len(),
        })
    }
    #[cfg(not(unix))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "stable filesystem identity is unavailable; cleanup is refused",
        ))
    }
}

fn same_directory(path: &Path, expected: &PathSnapshot) -> bool {
    snapshot(path).is_ok_and(|actual| {
        actual.directory && actual.device == expected.device && actual.inode == expected.inode
    })
}

fn holder_refusal(evidence: HolderEvidence) -> Option<String> {
    match evidence {
        HolderEvidence::Clear => None,
        HolderEvidence::Held(processes) => Some(format!(
            "logs have open files held by PID(s) {:?}",
            processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>()
        )),
        HolderEvidence::Unknown(reason) => {
            Some(format!("log holder evidence is inconclusive: {reason}"))
        }
    }
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
    plan_tachi_clean_with_probe(home_override, dry_run, now, max_age, &holder::probe_holders)
}

fn plan_tachi_clean_with_probe(
    home_override: Option<&Path>,
    dry_run: bool,
    now: SystemTime,
    max_age: Duration,
    probe: &dyn Fn(&Path) -> HolderEvidence,
) -> TachiCleanReport {
    let home = resolve_tachi_home(home_override);
    let mut report = TachiCleanReport {
        action: "tachi-clean",
        home: home.display().to_string(),
        home_path: home.clone(),
        dry_run,
        removed: Vec::new(),
        candidates: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
        home_identity: None,
        logs_identity: None,
        max_age,
    };

    if !home.exists() {
        report
            .warnings
            .push("Tachi home does not exist; nothing to clean".to_string());
        return report;
    }

    // These paths contain durable identity receipts, evaluation evidence and
    // database rollback images. Neither age nor terminal status proves that
    // their consumers no longer need them.
    report.warnings.push(
        "retained runs, .agent/claude-code-runs and cleanup-backups; age alone is not deletion authority".to_string(),
    );
    if let Err(error) = prepare_log_root(&home, &mut report) {
        report
            .warnings
            .push(format!("retained Tachi artifacts: {error}"));
        return report;
    }
    let logs = report.home_path.join("logs");
    if let Some(reason) = holder_refusal(probe(&logs)) {
        report.warnings.push(reason);
        return report;
    }
    collect_age_candidates(&logs, "log", now, max_age, &mut report);

    if report.candidates.is_empty() {
        report
            .warnings
            .push("no cleanable Tachi artifacts found".to_string());
    }

    report
}

fn prepare_log_root(home: &Path, report: &mut TachiCleanReport) -> std::io::Result<()> {
    let home_identity = snapshot(home)?;
    if !home_identity.directory {
        return Err(std::io::Error::other("Tachi home is not a real directory"));
    }
    let canonical = home.canonicalize()?;
    if !same_directory(&canonical, &home_identity) {
        return Err(std::io::Error::other("Tachi home identity changed"));
    }
    let logs_identity = snapshot(&canonical.join("logs"))?;
    if !logs_identity.directory {
        return Err(std::io::Error::other("logs is not a real directory"));
    }
    report.home = canonical.display().to_string();
    report.home_path = canonical;
    report.home_identity = Some(home_identity);
    report.logs_identity = Some(logs_identity);
    Ok(())
}

fn collect_age_candidates(
    path: &Path,
    kind: &'static str,
    now: SystemTime,
    max_age: Duration,
    report: &mut TachiCleanReport,
) {
    let entries = match list_child_paths(path) {
        Ok(entries) => entries,
        Err(error) => {
            report.warnings.push(error);
            return;
        }
    };
    for entry in entries {
        let captured = match snapshot(&entry) {
            Ok(captured) if !captured.directory => captured,
            Ok(_) => {
                report
                    .warnings
                    .push(format!("retained log directory {}", entry.display()));
                continue;
            }
            Err(error) => {
                report
                    .warnings
                    .push(format!("retained {}: {error}", entry.display()));
                continue;
            }
        };
        let Ok(age) = now.duration_since(captured.modified) else {
            report
                .warnings
                .push(format!("retained future-dated log {}", entry.display()));
            continue;
        };
        if age >= max_age {
            report.candidates.push(TachiCleanCandidate {
                bytes: captured.bytes,
                path: entry.display().to_string(),
                fs_path: entry,
                kind,
                reason: format!("older than {} day(s)", DEFAULT_MAX_AGE_DAYS),
                snapshot: captured,
            });
        }
    }
}

fn execute_tachi_clean(report: &mut TachiCleanReport) {
    execute_tachi_clean_with_probe(report, &holder::probe_holders);
}

fn execute_tachi_clean_with_probe(
    report: &mut TachiCleanReport,
    probe: &dyn Fn(&Path) -> HolderEvidence,
) {
    for candidate in &report.candidates {
        let path = candidate.fs_path.as_path();
        let home = report.home_path.as_path();
        let logs = home.join("logs");
        let roots_match = || {
            report
                .home_identity
                .as_ref()
                .is_some_and(|expected| same_directory(home, expected))
                && report
                    .logs_identity
                    .as_ref()
                    .is_some_and(|expected| same_directory(&logs, expected))
        };
        if !roots_match() || path.parent() != Some(logs.as_path()) {
            report.warnings.push(format!(
                "retained {}: cleanup root identity changed",
                path.display()
            ));
            continue;
        }
        if let Some(reason) = holder_refusal(probe(&logs)) {
            report
                .warnings
                .push(format!("retained {}: {reason}", path.display()));
            continue;
        }
        let still_old = SystemTime::now()
            .duration_since(candidate.snapshot.modified)
            .is_ok_and(|age| age >= report.max_age);
        if !roots_match() || !still_old || snapshot(path).ok().as_ref() != Some(&candidate.snapshot)
        {
            report.warnings.push(format!(
                "retained {}: file or root changed since planning",
                path.display()
            ));
            continue;
        }
        // Like sweep's existing guard, this narrows the same-user check/unlink
        // window; it is not atomic exclusion of a writer starting afterward.
        // No runtime directory, backup, symlink or log directory reaches unlink.
        match std::fs::remove_file(path) {
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
    fn tachi_clean_retains_durable_runtime_and_all_backups() {
        let home = unique_temp_dir("tachi-clean-durable-evidence");
        let retained = [
            "runs/active/status.json",
            "runs/completed/status.json",
            ".agent/claude-code-runs/run-1/backup.db",
            "cleanup-backups/2026-01-01/backup.db",
            "cleanup-backups/2026-01-02/backup.db",
            "cleanup-backups/2026-01-03/backup.db",
        ];
        for relative in retained {
            let path = home.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"durable evidence").unwrap();
        }
        let mut report = plan_tachi_clean(Some(&home), false, SystemTime::now(), Duration::ZERO);
        execute_tachi_clean(&mut report);
        for relative in retained {
            assert_eq!(
                std::fs::read(home.join(relative)).unwrap(),
                b"durable evidence"
            );
        }
        assert!(report.removed.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn tachi_clean_retains_nested_log_directory() {
        let home = unique_temp_dir("tachi-clean-nested-log");
        let log = home.join("logs/old-run/still-written.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, b"active writer evidence").unwrap();
        let mut report = plan_tachi_clean(Some(&home), false, SystemTime::now(), Duration::ZERO);
        execute_tachi_clean(&mut report);
        assert_eq!(std::fs::read(log).unwrap(), b"active writer evidence");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn tachi_clean_keeps_all_backups_without_lifecycle_authority() {
        let home = unique_temp_dir("tachi-clean-home-backups");
        let backups = home.join("cleanup-backups");
        for name in ["2026-01-01", "2026-01-02", "2026-01-03"] {
            std::fs::create_dir_all(backups.join(name)).unwrap();
        }

        let report = plan_tachi_clean(Some(&home), true, SystemTime::now(), Duration::ZERO);

        assert!(report.candidates.is_empty());
        for name in ["2026-01-01", "2026-01-02", "2026-01-03"] {
            assert!(backups.join(name).exists());
        }

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn tachi_clean_never_uses_age_to_remove_runs_or_backups() {
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
        assert!(backups.join("2026-01-01").exists());
        assert!(backups.join("2026-01-02").exists());
        assert!(backups.join("2026-01-03").exists());
        assert!(runs.join("run-1").exists());
        assert!(report.removed.is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    fn log_fixture(prefix: &str) -> (PathBuf, PathBuf) {
        let home = unique_temp_dir(prefix);
        std::fs::create_dir(home.join("logs")).unwrap();
        let log = home.join("logs/old.log");
        std::fs::write(&log, b"old log").unwrap();
        (home, log)
    }

    #[cfg(unix)]
    fn log_plan(home: &Path) -> TachiCleanReport {
        plan_tachi_clean_with_probe(
            Some(home),
            false,
            SystemTime::now(),
            Duration::ZERO,
            &|path| {
                assert_eq!(path, home.join("logs"));
                HolderEvidence::Clear
            },
        )
    }

    /// #2037 review: a non-UTF-8 home component must reach the filesystem
    /// losslessly. The display string is only the report's rendering.
    /// APFS rejects such names, so this runs on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_home_component_is_planned_and_cleaned_losslessly() {
        use std::os::unix::ffi::OsStrExt;
        let parent = unique_temp_dir("tachi-clean-non-utf8-home");
        let home = parent.join(std::ffi::OsStr::from_bytes(b"home-\xff"));
        std::fs::create_dir_all(home.join("logs")).unwrap();
        let log = home.join("logs/old.log");
        std::fs::write(&log, b"old log").unwrap();
        let logs = home.canonicalize().unwrap().join("logs");
        let mut report = plan_tachi_clean_with_probe(
            Some(&home),
            false,
            SystemTime::now(),
            Duration::ZERO,
            &|path| {
                assert_eq!(path, logs, "the probe must target the real directory");
                HolderEvidence::Clear
            },
        );
        assert_eq!(report.candidates.len(), 1, "{:?}", report.warnings);
        assert!(report.home.contains('\u{fffd}'), "display form stays lossy");
        execute_tachi_clean_with_probe(&mut report, &|_| HolderEvidence::Clear);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!log.exists(), "{:?}", report.warnings);
        std::fs::remove_dir_all(parent).unwrap();
    }

    /// Platform-independent half of the guard above: execution must act on
    /// the lossless paths even when the rendered strings differ from them,
    /// as they do for non-UTF-8 components.
    #[cfg(unix)]
    #[test]
    fn execution_uses_lossless_paths_not_their_display_strings() {
        let (home, log) = log_fixture("tachi-clean-lossless-paths");
        let mut report = log_plan(&home);
        assert_eq!(report.candidates.len(), 1);
        report.home = home.join("display-only").display().to_string();
        report.candidates[0].path = home.join("display-only/old.log").display().to_string();
        execute_tachi_clean_with_probe(&mut report, &|_| HolderEvidence::Clear);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!log.exists(), "{:?}", report.warnings);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_regular_log_is_removed_only_after_a_second_clear_probe() {
        let (home, log) = log_fixture("tachi-clean-stale-log");
        let mut report = log_plan(&home);
        assert_eq!(report.candidates.len(), 1);
        assert!(log.exists());
        let probes = std::cell::Cell::new(0);
        execute_tachi_clean_with_probe(&mut report, &|path| {
            assert_eq!(path, home.join("logs"));
            probes.set(probes.get() + 1);
            HolderEvidence::Clear
        });
        assert_eq!(probes.get(), 1);
        assert_eq!(report.removed, vec![log.display().to_string()]);
        assert!(!log.exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn public_dry_run_retains_an_eligible_stale_log() {
        let (home, log) = log_fixture("tachi-clean-public-dry-run");
        let old = SystemTime::now() - Duration::from_secs(10 * 86400);
        std::fs::File::options()
            .write(true)
            .open(&log)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        let plan = plan_tachi_clean(
            Some(&home),
            true,
            SystemTime::now(),
            Duration::from_secs(7 * 86400),
        );
        assert_eq!(
            plan.candidates.len(),
            1,
            "real OS holder probe must prove fixture eligible"
        );
        run_tachi_clean(TachiCleanOptions {
            home: Some(home.clone()),
            force: false,
            output: OutputFormat::Json,
        })
        .unwrap();
        assert_eq!(std::fs::read(log).unwrap(), b"old log");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn held_or_unknown_logs_are_retained_at_plan_and_execute() {
        let (home, log) = log_fixture("tachi-clean-held-log");
        for evidence in [
            HolderEvidence::Held(vec![]),
            HolderEvidence::Unknown("permission denied".into()),
        ] {
            let plan = plan_tachi_clean_with_probe(
                Some(&home),
                true,
                SystemTime::now(),
                Duration::ZERO,
                &|_| evidence.clone(),
            );
            assert!(plan.candidates.is_empty());
            let mut report = log_plan(&home);
            execute_tachi_clean_with_probe(&mut report, &|_| evidence.clone());
            assert!(report.removed.is_empty());
            assert_eq!(std::fs::read(&log).unwrap(), b"old log");
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn logs_written_during_the_execute_probe_are_retained() {
        let (home, log) = log_fixture("tachi-clean-new-write");
        let mut report = log_plan(&home);
        execute_tachi_clean_with_probe(&mut report, &|_| {
            std::fs::write(&log, b"new writer evidence").unwrap();
            HolderEvidence::Clear
        });
        assert!(report.removed.is_empty());
        assert_eq!(std::fs::read(log).unwrap(), b"new writer evidence");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replaced_log_inode_and_symlink_are_retained() {
        let (home, log) = log_fixture("tachi-clean-replaced-log");
        let mut report = log_plan(&home);
        let old = home.join("old-log-evidence");
        std::fs::rename(&log, &old).unwrap();
        std::fs::write(&log, b"old log").unwrap();
        execute_tachi_clean_with_probe(&mut report, &|_| HolderEvidence::Clear);
        assert!(report.removed.is_empty());
        assert_eq!(std::fs::read(&log).unwrap(), b"old log");

        let mut report = log_plan(&home);
        std::fs::remove_file(&log).unwrap();
        std::os::unix::fs::symlink(&old, &log).unwrap();
        execute_tachi_clean_with_probe(&mut report, &|_| HolderEvidence::Clear);
        assert!(report.removed.is_empty());
        assert!(std::fs::symlink_metadata(&log)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(old).unwrap(), b"old log");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replaced_logs_root_is_retained() {
        let (home, log) = log_fixture("tachi-clean-replaced-root");
        let mut report = log_plan(&home);
        std::fs::rename(home.join("logs"), home.join("original-logs")).unwrap();
        std::fs::create_dir(home.join("logs")).unwrap();
        std::fs::write(&log, b"foreign replacement").unwrap();
        execute_tachi_clean_with_probe(&mut report, &|_| {
            panic!("changed root must be refused before probing")
        });
        assert!(report.removed.is_empty());
        assert_eq!(std::fs::read(log).unwrap(), b"foreign replacement");
        assert_eq!(
            std::fs::read(home.join("original-logs/old.log")).unwrap(),
            b"old log"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn fresh_future_and_symlink_logs_are_never_candidates() {
        let (home, log) = log_fixture("tachi-clean-fresh-log");
        let now = SystemTime::now();
        let week = Duration::from_secs(DEFAULT_MAX_AGE_DAYS * 86400);
        let fresh =
            plan_tachi_clean_with_probe(Some(&home), true, now, week, &|_| HolderEvidence::Clear);
        assert!(fresh.candidates.is_empty());
        std::fs::File::options()
            .write(true)
            .open(&log)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(now + week))
            .unwrap();
        assert!(log_plan(&home).candidates.is_empty());
        std::fs::remove_file(&log).unwrap();
        std::os::unix::fs::symlink(home.join("missing-target"), &log).unwrap();
        assert!(log_plan(&home).candidates.is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(not(unix))]
    #[test]
    fn tachi_clean_refuses_without_stable_identity() {
        let home = unique_temp_dir("tachi-clean-identity-refusal");
        std::fs::create_dir(home.join("logs")).unwrap();
        std::fs::write(home.join("logs/old.log"), b"evidence").unwrap();
        let report = plan_tachi_clean_with_probe(
            Some(&home),
            true,
            SystemTime::now(),
            Duration::ZERO,
            &|_| panic!("identity must be checked first"),
        );
        assert!(report.candidates.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("stable filesystem identity")));
        assert_eq!(
            std::fs::read(home.join("logs/old.log")).unwrap(),
            b"evidence"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }
}
