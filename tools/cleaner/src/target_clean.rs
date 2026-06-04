use std::path::{Path, PathBuf};

use crate::wt_clean::OutputFormat;

#[derive(Debug)]
pub struct TargetCleanOptions {
    pub path: PathBuf,
    pub force: bool,
    pub output: OutputFormat,
}

#[derive(Debug, serde::Serialize)]
struct TargetCleanReport {
    action: &'static str,
    path: String,
    target_dir: Option<String>,
    dry_run: bool,
    removed: Vec<String>,
    candidates: Vec<TargetCleanCandidate>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct TargetCleanCandidate {
    path: String,
    kind: &'static str,
    exists: bool,
    bytes: u64,
}

pub fn run_target_clean(options: TargetCleanOptions) -> Result<(), String> {
    let mut report = plan_target_clean(&options.path, !options.force);
    if options.force && report.errors.is_empty() {
        execute_target_clean(&mut report);
    }

    emit_report(&report, options.output)?;
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("; "))
    }
}

fn plan_target_clean(path: &Path, dry_run: bool) -> TargetCleanReport {
    let mut report = TargetCleanReport {
        action: "target-clean",
        path: path.display().to_string(),
        target_dir: None,
        dry_run,
        removed: Vec::new(),
        candidates: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let target_dir = resolve_target_dir(path);
    let Ok(target_dir) = target_dir else {
        report.errors.push(target_dir.unwrap_err());
        return report;
    };
    report.target_dir = Some(target_dir.display().to_string());

    let release_dir = target_dir.join("release");
    let candidates = [
        (target_dir.join("debug"), "debug"),
        (release_dir.join("deps"), "release-deps"),
        (release_dir.join("build"), "release-build"),
        (release_dir.join("incremental"), "release-incremental"),
        (release_dir.join("examples"), "release-examples"),
        (release_dir.join(".fingerprint"), "release-fingerprint"),
    ];

    for (path, kind) in candidates {
        let exists = path.exists();
        report.candidates.push(TargetCleanCandidate {
            bytes: if exists { dir_size(&path) } else { 0 },
            path: path.display().to_string(),
            kind,
            exists,
        });
    }

    if report.candidates.iter().all(|candidate| !candidate.exists) {
        report
            .warnings
            .push("no cleanable target artifacts found".to_string());
    }

    report
}

fn execute_target_clean(report: &mut TargetCleanReport) {
    for candidate in &report.candidates {
        if !candidate.exists {
            continue;
        }
        let path = Path::new(&candidate.path);
        match std::fs::remove_dir_all(path) {
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

fn resolve_target_dir(path: &Path) -> Result<PathBuf, String> {
    let canonical =
        std::fs::canonicalize(path).map_err(|err| format!("cannot canonicalize path: {err}"))?;
    if canonical.file_name().is_some_and(|name| name == "target") {
        return Ok(canonical);
    }
    let target = canonical.join("target");
    if target.is_dir() {
        Ok(target)
    } else {
        Err(format!("target directory not found under {}", canonical.display()))
    }
}

fn dir_size(path: &Path) -> u64 {
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
        .map(|entry| dir_size(&entry.path()))
        .sum()
}

fn emit_report(report: &TargetCleanReport, output: OutputFormat) -> Result<(), String> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|err| format!("serialize report: {err}"))?
        ),
        OutputFormat::Text => {
            let mode = if report.dry_run { "dry-run" } else { "force" };
            println!("tachi-clean target ({mode})");
            println!("  path: {}", report.path);
            if let Some(target_dir) = &report.target_dir {
                println!("  target: {target_dir}");
            }
            for candidate in &report.candidates {
                println!(
                    "  candidate: {} exists={} bytes={}",
                    candidate.path, candidate.exists, candidate.bytes
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
    fn target_clean_keeps_release_outputs_and_removes_only_build_artifacts() {
        let root = unique_temp_dir("tachi-clean-target-test");
        let release = root.join("target/release");
        std::fs::create_dir_all(release.join("deps")).unwrap();
        std::fs::create_dir_all(release.join("build")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(release.join("tachi"), "release-binary").unwrap();
        std::fs::write(release.join("deps/libtachi.rlib"), "dep").unwrap();
        std::fs::write(root.join("target/debug/tachi"), "debug-binary").unwrap();

        let mut report = plan_target_clean(&root, true);
        assert!(report.errors.is_empty());
        assert_eq!(
            report
                .candidates
                .iter()
                .filter(|candidate| candidate.exists)
                .count(),
            3
        );

        execute_target_clean(&mut report);
        assert!(report.errors.is_empty());
        assert!(release.join("tachi").exists());
        assert!(!release.join("deps").exists());
        assert!(!release.join("build").exists());
        assert!(!root.join("target/debug").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn target_clean_accepts_target_dir_directly() {
        let root = unique_temp_dir("tachi-clean-target-direct-test");
        let target = root.join("target");
        std::fs::create_dir_all(target.join("debug")).unwrap();

        let report = plan_target_clean(&target, true);

        assert!(report.errors.is_empty());
        assert_eq!(
            report.target_dir.as_deref(),
            Some(
                std::fs::canonicalize(&target)
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
