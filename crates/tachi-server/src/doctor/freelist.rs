//! Read-only SQLite freelist bloat tripwire for `tachi doctor`.
//!
//! Surfaces healthy databases whose `PRAGMA freelist_count` dead pages represent
//! substantial reclaimable disk space (`>= 10 MiB` and `>= 25%` of total file
//! pages). Informational only: never acquires write locks or mutates files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::path_utils::make_immutable_uri;

use super::schema_skew::is_probeable_tachi_db;
use super::{DoctorFinding, DoctorWarning};

pub const FREELIST_WARN_MIN_BYTES: u64 = 10 * 1024 * 1024;
pub const FREELIST_WARN_MIN_PERCENT: u64 = 25;

pub fn freelist_bloat_warnings(findings: &[DoctorFinding]) -> Vec<DoctorWarning> {
    freelist_bloat_warnings_with_thresholds(
        findings,
        FREELIST_WARN_MIN_BYTES,
        FREELIST_WARN_MIN_PERCENT,
    )
}

pub(crate) fn freelist_bloat_warnings_with_thresholds(
    findings: &[DoctorFinding],
    min_bytes: u64,
    min_percent: u64,
) -> Vec<DoctorWarning> {
    let mut seen_physical = BTreeSet::<PathBuf>::new();
    let mut warnings = Vec::new();

    for finding in findings {
        if !is_probeable_tachi_db(finding) {
            continue;
        }
        let path = Path::new(&finding.path);
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !seen_physical.insert(canonical) {
            continue;
        }
        let Ok(conn) = memcore::db::open_immutable_readonly(&make_immutable_uri(path)) else {
            continue;
        };
        let Ok(stats) = memcore::db::freelist_stats(&conn) else {
            continue;
        };
        if stats.freelist_bytes() >= min_bytes && stats.freelist_percent() >= min_percent {
            let dead_mib = stats.freelist_bytes() as f64 / (1024.0 * 1024.0);
            let total_mib = stats.total_bytes() as f64 / (1024.0 * 1024.0);
            warnings.push(DoctorWarning {
                code: "high_freelist_bloat".to_string(),
                path: finding.path.clone(),
                message: format!(
                    "{}: {} dead freelist pages ({dead_mib:.1} MiB, {}% of {total_mib:.1} MiB file)",
                    finding.path,
                    stats.freelist_count,
                    stats.freelist_percent(),
                ),
                remediation: format!(
                    "stop the tachi daemon (`tachi daemon kill`) and run `tachi repair vacuum --db {} --apply` to reclaim dead pages",
                    finding.path
                ),
            });
        }
    }

    warnings
}
