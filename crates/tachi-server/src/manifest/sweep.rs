use std::path::Path;

use crate::doctor::{DbClassification, DoctorReport};

use super::Manifest;

/// Result of a manifest-guided sweep operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepReport {
    pub planned: Vec<SweepAction>,
    pub applied: Vec<SweepAction>,
    pub skipped: Vec<SweepAction>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepAction {
    pub path: String,
    pub reason: String,
    pub quarantine_to: Option<String>,
    pub note: String,
}

/// Plan a sweep: identify Placeholder/Backup files from a fresh doctor scan
/// that are NOT in the manifest, and propose moving them to quarantine.
/// Manifest-recorded entries are NEVER swept (they are owned).
///
/// Safety rules (paranoid by default):
///   1. The file must be classified Placeholder or Backup.
///   2. The file must NOT be in the manifest (owned files are sacred).
///   3. The file must live under a "Tachi-owned root" — either:
///        a. its parent directory contains a manifest-recorded Tachi DB, OR
///        b. its path matches one of the well-known Tachi roots
///           (`~/.tachi`, `~/.openclaw/extensions/tachi`, `~/.gemini/antigravity`).
///      This prevents sweeping unrelated sqlite files like `5min.db`,
///      `rust_gateway.db`, `cursor_mcp.db` which belong to other tools.
///   4. Quarantine target names are made unique with a numeric suffix to avoid
///      collisions when multiple swept files share the same basename.
pub fn plan_sweep(
    report: &DoctorReport,
    manifest: &Manifest,
    quarantine_dir: &Path,
) -> SweepReport {
    let mut planned = Vec::new();
    let mut skipped = Vec::new();
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Build the set of parent directories that already host an owned Tachi DB.
    let mut owned_parents: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &manifest.dbs {
        if let Some(p) = std::path::Path::new(&e.path).parent() {
            owned_parents.insert(p.to_string_lossy().to_string());
        }
    }

    for f in &report.findings {
        if manifest.lookup(&f.path).is_some() {
            skipped.push(SweepAction {
                path: f.path.clone(),
                reason: format!("{:?}", f.classification),
                quarantine_to: None,
                note: "in manifest — owned, never swept".to_string(),
            });
            continue;
        }
        let should_sweep = matches!(
            f.classification,
            DbClassification::Placeholder | DbClassification::Backup
        );
        if !should_sweep {
            continue;
        }

        // Tachi-owned-root gate.
        let parent = std::path::Path::new(&f.path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let in_tachi_root = parent.contains("/.tachi/")
            || parent.ends_with("/.tachi")
            || parent.contains("/.openclaw/extensions/tachi")
            || parent.contains("/.gemini/antigravity");
        let neighbor_owned = owned_parents.contains(&parent);

        if !in_tachi_root && !neighbor_owned {
            skipped.push(SweepAction {
                path: f.path.clone(),
                reason: format!("{:?}", f.classification),
                quarantine_to: None,
                note: "outside Tachi-owned roots — refusing to sweep".to_string(),
            });
            continue;
        }

        // Build a collision-free quarantine name.
        let base = std::path::Path::new(&f.path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "db".into());
        let mut candidate = format!("swept-{}", base);
        let mut n = 1;
        while used_names.contains(&candidate) || quarantine_dir.join(&candidate).exists() {
            candidate = format!("swept-{}.{}", base, n);
            n += 1;
        }
        used_names.insert(candidate.clone());
        let qpath = quarantine_dir
            .join(&candidate)
            .to_string_lossy()
            .to_string();

        planned.push(SweepAction {
            path: f.path.clone(),
            reason: format!("{:?}", f.classification),
            quarantine_to: Some(qpath),
            note: f.error.clone().unwrap_or_default(),
        });
    }

    SweepReport {
        planned,
        applied: Vec::new(),
        skipped,
    }
}

/// Execute a sweep plan: move each planned file to its quarantine target.
/// Returns the report with `applied` populated. Entries that fail to move
/// are recorded in `skipped` with the error reason.
pub fn apply_sweep(mut report: SweepReport, quarantine_dir: &Path) -> SweepReport {
    if let Err(e) = std::fs::create_dir_all(quarantine_dir) {
        for p in report.planned.drain(..) {
            report.skipped.push(SweepAction {
                note: format!("quarantine_dir create failed: {e}"),
                ..p
            });
        }
        return report;
    }
    let planned = std::mem::take(&mut report.planned);
    for action in planned {
        let Some(target) = action.quarantine_to.clone() else {
            report.skipped.push(action);
            continue;
        };
        match std::fs::rename(&action.path, &target) {
            Ok(_) => report.applied.push(action),
            Err(e) => {
                let note = format!("rename failed: {e}");
                report.skipped.push(SweepAction { note, ..action });
            }
        }
    }
    report
}
