use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::{
    canonicalize_db_path, classify_db_schema, should_skip_path, DbEntry, Manifest, SchemaKind,
};

/// Report from `gc_manifest`. Counts mutations applied during the GC pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcReport {
    pub canonicalized: usize,
    pub removed_missing: usize,
    pub removed_fixture: usize,
    pub schema_kind_fixed: usize,
    pub dedup_collapsed: usize,
    /// Total entries before GC.
    pub entries_before: usize,
    /// Total entries after GC.
    pub entries_after: usize,
    /// True if GC was aborted (e.g. >50% removal sanity guard tripped).
    pub aborted: bool,
    /// Optional human-readable abort/skip reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_reason: Option<String>,
}

/// Run a GC / hygiene pass over the manifest file at `manifest_path`:
///   1. Load the manifest (returns Ok with empty report if file missing).
///   2. Write a one-shot `{manifest_path}.bak` backup before mutation.
///   3. For each entry: canonicalize path, drop if file is missing, drop if
///      `should_skip_path` matches, re-classify `schema_kind`.
///   4. Dedup by canonical path (prefer entry with most-recent `last_doctor_at`).
///   5. Sanity guard: if would remove >50% of entries, ABORT, return a report
///      flagged `aborted: true` and DO NOT mutate the on-disk manifest.
///   6. Atomic save (write `.tmp`, rename).
///
/// Idempotent: a second invocation reports all-zero counters.
pub fn gc_manifest(manifest_path: &Path) -> std::io::Result<GcReport> {
    if !manifest_path.exists() {
        return Ok(GcReport::default());
    }
    let mut manifest = Manifest::load(manifest_path)?;
    let mut report = GcReport {
        entries_before: manifest.dbs.len(),
        ..GcReport::default()
    };

    // 2. Refuse to mutate unless a backup was durably written first.
    let backup_path: PathBuf = {
        let mut s = manifest_path.as_os_str().to_os_string();
        s.push(".bak");
        PathBuf::from(s)
    };
    let orig_bytes = std::fs::read(manifest_path)?;
    crate::utils::write_owner_only_file_atomic(&backup_path, &orig_bytes).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("manifest backup write {}: {e}", backup_path.display()),
        )
    })?;

    // 3. Per-entry pass.
    let original = std::mem::take(&mut manifest.dbs);
    // Map canonical path -> chosen entry. Last-write-wins preferring
    // most-recent `last_doctor_at`.
    let mut by_canon: std::collections::HashMap<PathBuf, DbEntry> =
        std::collections::HashMap::new();
    let mut order: Vec<PathBuf> = Vec::new();
    let mut removed_total: usize = 0;

    for mut entry in original {
        let raw_path = PathBuf::from(&entry.path);

        // (a) fixture skip-list — applies before any I/O.
        if let Some(reason) = should_skip_path(&raw_path) {
            tracing::debug!(target: "tachi::manifest::gc", path=%entry.path, reason, "skip fixture");
            report.removed_fixture += 1;
            removed_total += 1;
            continue;
        }
        // (b) missing on disk → drop.
        if !raw_path.exists() {
            tracing::debug!(target: "tachi::manifest::gc", path=%entry.path, "drop missing");
            report.removed_missing += 1;
            removed_total += 1;
            continue;
        }
        // (c) canonicalize.
        let canon = canonicalize_db_path(&raw_path);
        let canon_str = canon.to_string_lossy().to_string();
        if canon_str != entry.path {
            tracing::debug!(target: "tachi::manifest::gc", from=%entry.path, to=%canon_str, "canonicalize");
            entry.path = canon_str.clone();
            report.canonicalized += 1;
        }
        // (d) re-classify schema. Only correct mismatches; don't downgrade
        //     on a transient open failure (Unknown).
        let detected = classify_db_schema(&canon);
        if detected != SchemaKind::Unknown && detected.as_str() != entry.schema_kind {
            tracing::debug!(
                target: "tachi::manifest::gc",
                path=%entry.path,
                from=%entry.schema_kind,
                to=%detected.as_str(),
                "schema_kind fix"
            );
            entry.schema_kind = detected.as_str().to_string();
            report.schema_kind_fixed += 1;
        }
        // (e) dedup by canonical path.
        match by_canon.get(&canon) {
            Some(existing) => {
                // Prefer the entry with the most recent last_doctor_at.
                let keep_new = entry.last_doctor_at > existing.last_doctor_at;
                if keep_new {
                    by_canon.insert(canon.clone(), entry);
                } // else discard the new one
                report.dedup_collapsed += 1;
                removed_total += 1;
                tracing::debug!(target: "tachi::manifest::gc", path=%canon.display(), "dedup collapse");
            }
            None => {
                by_canon.insert(canon.clone(), entry);
                order.push(canon);
            }
        }
    }

    // 5. Sanity guard.
    if report.entries_before > 0 && removed_total * 2 > report.entries_before {
        report.aborted = true;
        report.abort_reason = Some(format!(
            "would remove {removed_total}/{} entries (>50%); aborting GC, on-disk manifest unchanged",
            report.entries_before
        ));
        report.entries_after = report.entries_before;
        tracing::error!(
            target: "tachi::manifest::gc",
            removed = removed_total,
            before = report.entries_before,
            "aborting GC: sanity guard tripped"
        );
        return Ok(report);
    }

    // Re-assemble in original-encounter order, then sort for stable output
    // (matches populate_from_doctor's behaviour).
    let mut new_dbs: Vec<DbEntry> = order
        .into_iter()
        .filter_map(|k| by_canon.remove(&k))
        .collect();
    new_dbs.sort_by(|a, b| a.path.cmp(&b.path));
    manifest.dbs = new_dbs;
    manifest.generated_at = Utc::now().to_rfc3339();
    report.entries_after = manifest.dbs.len();
    // 6. Atomic save.
    manifest.save(manifest_path)?;
    Ok(report)
}
