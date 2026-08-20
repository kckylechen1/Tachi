use std::fs;
use std::path::{Path, PathBuf};

use super::scan::is_backup_filename;
use super::{DbClassification, DoctorFinding, JobBreakdown};
use crate::memory_search_ops::routing_config::RoutingConfigProvider;

// ─── Classification ──────────────────────────────────────────────────────────

#[cfg(test)]
pub fn classify_one(path: &Path) -> DoctorFinding {
    let provider = RoutingConfigProvider::new(crate::path_utils::tachi_home());
    classify_one_with_provider(path, &provider)
}

pub(super) fn classify_one_with_provider(
    path: &Path,
    routing_config: &RoutingConfigProvider,
) -> DoctorFinding {
    classify_one_with_provider_mode(path, routing_config, false)
}

pub(super) fn classify_one_immutable_with_provider(
    path: &Path,
    routing_config: &RoutingConfigProvider,
) -> DoctorFinding {
    classify_one_with_provider_mode(path, routing_config, true)
}

fn classify_one_with_provider_mode(
    path: &Path,
    routing_config: &RoutingConfigProvider,
    immutable: bool,
) -> DoctorFinding {
    let path_str = path.display().to_string();
    let scope_hint = scope_hint_for(path);
    let file_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let wal_path = sidecar(path, "-wal");
    let has_wal = wal_path.exists()
        && fs::metadata(&wal_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false);

    let basename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    // 1. Backup/archival path or filename → short-circuit (don't try to open).
    if crate::manifest::is_archival_db_path(path) || is_backup_filename(&basename) {
        return DoctorFinding {
            path: path_str,
            classification: DbClassification::Backup,
            file_size,
            has_wal,
            mem_count: None,
            vec_rowid_count: None,
            none_domain_count: None,
            cross_domain_suspect_count: None,
            cross_domain_suspect_sample: Vec::new(),
            jobs: JobBreakdown::default(),
            schema_kind: "unknown".to_string(),
            error: None,
            scope_hint,
        };
    }

    // 2. 0-byte file → placeholder.
    if file_size == 0 {
        return DoctorFinding {
            path: path_str,
            classification: DbClassification::Placeholder,
            file_size,
            has_wal,
            mem_count: None,
            vec_rowid_count: None,
            none_domain_count: None,
            cross_domain_suspect_count: None,
            cross_domain_suspect_sample: Vec::new(),
            jobs: JobBreakdown::default(),
            schema_kind: "empty".to_string(),
            error: None,
            scope_hint,
        };
    }

    // 3. Open read-only without `immutable=1`. Immutable SQLite handles ignore
    // committed WAL frames, so they undercount a healthy live store. The
    // shared inventory opener cannot create/migrate a DB or mutate authority.
    let conn = match if immutable {
        memcore::db::open_immutable_readonly(&make_immutable_uri(path))
    } else {
        crate::physical_db_identity::open_read_only_connection(path)
    } {
        Ok(c) => c,
        Err(e) => {
            return DoctorFinding {
                path: path_str,
                classification: DbClassification::Corrupt,
                file_size,
                has_wal,
                mem_count: None,
                vec_rowid_count: None,
                none_domain_count: None,
                cross_domain_suspect_count: None,
                cross_domain_suspect_sample: Vec::new(),
                jobs: JobBreakdown::default(),
                schema_kind: "unknown".to_string(),
                error: Some(format!("open failed: {e}")),
                scope_hint,
            };
        }
    };
    let _ = memcore::db::try_load_sqlite_vec(&conn);

    // Validate this is actually a SQLite file before any further probing.
    // pragma schema_version is cheap and fails immediately on garbage bytes
    // ("file is not a database" / "not a database").
    if let Err(e) = memcore::db::schema_version(&conn) {
        return DoctorFinding {
            path: path_str,
            classification: DbClassification::Corrupt,
            file_size,
            has_wal,
            mem_count: None,
            vec_rowid_count: None,
            none_domain_count: None,
            cross_domain_suspect_count: None,
            cross_domain_suspect_sample: Vec::new(),
            jobs: JobBreakdown::default(),
            schema_kind: "unknown".to_string(),
            error: Some(format!("schema_version probe failed: {e}")),
            scope_hint,
        };
    }

    // Detect schema kind. #1041 B6: `table_exists` now surfaces a real
    // `sqlite_master` query error instead of folding it into `false` — treat
    // it exactly like the `schema_version`/`count_result` probe failures
    // just above: a real error here means this DB couldn't even be
    // classified, not "schema kind unknown, otherwise fine".
    let has_memories = match memcore::db::table_exists(&conn, "memories") {
        Ok(v) => v,
        Err(e) => {
            return DoctorFinding {
                path: path_str,
                classification: DbClassification::Corrupt,
                file_size,
                has_wal,
                mem_count: None,
                vec_rowid_count: None,
                none_domain_count: None,
                cross_domain_suspect_count: None,
                cross_domain_suspect_sample: Vec::new(),
                jobs: JobBreakdown::default(),
                schema_kind: "unknown".to_string(),
                error: Some(format!("table_exists('memories') probe failed: {e}")),
                scope_hint,
            };
        }
    };
    let has_chunks = match memcore::db::table_exists(&conn, "chunks") {
        Ok(v) => v,
        Err(e) => {
            return DoctorFinding {
                path: path_str,
                classification: DbClassification::Corrupt,
                file_size,
                has_wal,
                mem_count: None,
                vec_rowid_count: None,
                none_domain_count: None,
                cross_domain_suspect_count: None,
                cross_domain_suspect_sample: Vec::new(),
                jobs: JobBreakdown::default(),
                schema_kind: "unknown".to_string(),
                error: Some(format!("table_exists('chunks') probe failed: {e}")),
                scope_hint,
            };
        }
    };
    let has_memories_vec = match memcore::db::table_exists(&conn, "memories_vec") {
        Ok(v) => v,
        Err(e) => {
            return DoctorFinding {
                path: path_str,
                classification: DbClassification::Corrupt,
                file_size,
                has_wal,
                mem_count: None,
                vec_rowid_count: None,
                none_domain_count: None,
                cross_domain_suspect_count: None,
                cross_domain_suspect_sample: Vec::new(),
                jobs: JobBreakdown::default(),
                schema_kind: "unknown".to_string(),
                error: Some(format!("table_exists('memories_vec') probe failed: {e}")),
                scope_hint,
            };
        }
    };

    let schema_kind = if has_memories {
        "tachi"
    } else if has_chunks {
        "openclaw_legacy"
    } else {
        "unknown"
    }
    .to_string();

    // Quick integrity check. We INTENTIONALLY skip pragma quick_check on DBs that
    // contain sqlite-vec virtual tables when the extension isn't loaded — that path
    // produces "stepping, SQL logic error" which is a FALSE positive for corruption.
    // We fall back to a simple SELECT count(*) on the canonical table.
    let count_result = if has_memories {
        memcore::db::count_memories_rows(&conn)
    } else if has_chunks {
        memcore::db::count_chunks_rows(&conn)
    } else {
        Ok(0)
    };

    if let Err(e) = &count_result {
        // Could not even read the canonical table → corrupt or schema mismatch.
        let msg = format!("{e}");
        // Distinguish "vec extension issue" from real corruption.
        let class = if has_memories_vec && msg.to_ascii_lowercase().contains("no such module") {
            DbClassification::VecExtensionMissing
        } else if msg.contains("malformed") || msg.contains("not a database") {
            DbClassification::Corrupt
        } else {
            DbClassification::Corrupt
        };
        return DoctorFinding {
            path: path_str,
            classification: class,
            file_size,
            has_wal,
            mem_count: None,
            vec_rowid_count: None,
            none_domain_count: None,
            cross_domain_suspect_count: None,
            cross_domain_suspect_sample: Vec::new(),
            jobs: JobBreakdown::default(),
            schema_kind,
            error: Some(msg),
            scope_hint,
        };
    }

    let mem_count = count_result.ok();

    // Legacy schema short-circuit: openclaw `chunks` only.
    if !has_memories && has_chunks {
        return DoctorFinding {
            path: path_str,
            classification: DbClassification::LegacySchema,
            file_size,
            has_wal,
            mem_count,
            vec_rowid_count: None,
            none_domain_count: None,
            cross_domain_suspect_count: None,
            cross_domain_suspect_sample: Vec::new(),
            jobs: JobBreakdown::default(),
            schema_kind,
            error: None,
            scope_hint,
        };
    }

    // Healthy / vec-missing detail probe: try reading vec rowid count.
    let mut vec_rowid_count: Option<usize> = None;
    let mut classification = DbClassification::Healthy;
    if has_memories_vec {
        match memcore::db::count_memories_vec_rows(&conn) {
            Ok(n) => vec_rowid_count = Some(n),
            Err(e) => {
                let msg = format!("{e}").to_ascii_lowercase();
                if msg.contains("no such module") || msg.contains("vec0") {
                    classification = DbClassification::VecExtensionMissing;
                } else {
                    // Real read error against vec table → mark vec-extension-missing
                    // rather than corrupting the whole DB.
                    classification = DbClassification::VecExtensionMissing;
                }
            }
        }
    }

    // WAL-orphan check is overlaid LAST so a healthy DB with a stale WAL
    // gets flagged for a copy-aside checkpoint.
    if has_wal && classification == DbClassification::Healthy {
        // Heuristic: if the wal sidecar exists and is non-trivial (>4KB) and the DB
        // file modification time is older than the WAL, the WAL likely has unflushed
        // pages. We surface as WalOrphan even for healthy-looking DBs so auto-fix
        // can produce a clean copy.
        let wal_size = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
        if wal_size > 4096 {
            classification = DbClassification::WalOrphan;
        }
    }

    // Detail probes (best-effort, all errors swallowed).
    let none_domain_count = memcore::db::count_memories_missing_domain(&conn).ok();
    let config = routing_config.get();
    let cross_domain_probe = super::cross_domain::probe(&conn, &scope_hint, &config);
    let cross_domain_suspect_count = cross_domain_probe.as_ref().map(|probe| probe.count);
    let cross_domain_suspect_sample = cross_domain_probe
        .map(|probe| probe.sample_ids)
        .unwrap_or_default();
    let jobs = job_breakdown_from_counts(memcore::db::foundry_job_status_counts(&conn));

    DoctorFinding {
        path: path_str,
        classification,
        file_size,
        has_wal,
        mem_count,
        vec_rowid_count,
        none_domain_count,
        cross_domain_suspect_count,
        cross_domain_suspect_sample,
        jobs,
        schema_kind,
        error: None,
        scope_hint,
    }
}

// pub(crate): reused by `schema_skew` (#1119) to reopen an already-classified
// finding's DB read-only/immutable for a best-effort `PRAGMA user_version`
// probe, exactly as `classify_one` opens it.
pub(crate) fn make_immutable_uri(path: &Path) -> String {
    // sqlite URI percent-encoding: only ?, #, and space need handling for typical paths.
    let s = path.to_string_lossy().to_string();
    let encoded = s
        .replace('?', "%3f")
        .replace('#', "%23")
        .replace(' ', "%20");
    format!("file:{encoded}?mode=ro&immutable=1")
}

pub(crate) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Convert memcore's raw status tally into the report-shaping
/// `JobBreakdown` (which additionally derives the `other` bucket).
fn job_breakdown_from_counts(counts: memcore::db::FoundryJobStatusCounts) -> JobBreakdown {
    let known = counts.completed + counts.skipped + counts.failed + counts.pending;
    JobBreakdown {
        total: counts.total,
        completed: counts.completed,
        skipped: counts.skipped,
        failed: counts.failed,
        pending: counts.pending,
        other: counts.total.saturating_sub(known),
    }
}

pub fn scope_hint_for(path: &Path) -> String {
    scope_hint_for_in_home(path, &crate::path_utils::tachi_home())
}

fn is_global_identity(path: &Path, tachi_home: &Path) -> bool {
    for filename in [
        memcore::MEMORY_DB_FILENAME,
        memcore::LEGACY_MEMORY_DB_FILENAME,
    ] {
        for candidate in [
            tachi_home.join(filename),
            tachi_home.join("global").join(filename),
        ] {
            if path == candidate {
                return true;
            }
            if let (Ok(canon_path), Ok(canon_candidate)) = (
                std::fs::canonicalize(path),
                std::fs::canonicalize(&candidate),
            ) {
                if canon_path == canon_candidate {
                    return true;
                }
            }
        }
    }
    false
}

pub fn scope_hint_for_in_home(path: &Path, tachi_home: &Path) -> String {
    if is_global_identity(path, tachi_home) {
        return "global".to_string();
    }
    let n = path.to_string_lossy().replace('\\', "/");
    if let Some((_, rest)) = n.split_once("/.tachi/projects/") {
        let proj = rest.split('/').next().unwrap_or("unknown");
        return format!("project:{proj}");
    }
    if let Some((_, rest)) = n.split_once("/.sigil/projects/") {
        let proj = rest.split('/').next().unwrap_or("unknown");
        return format!("project:{proj}");
    }
    let projects_dir = tachi_home.join("projects");
    if let Ok(rel) = path.strip_prefix(&projects_dir) {
        if let Some(first) = rel.components().next() {
            let proj = first.as_os_str().to_string_lossy();
            return format!("project:{proj}");
        }
    }
    if let Some((_, rest)) = n.split_once("/.openclaw/extensions/tachi/data/agents/") {
        let agent = rest.split('/').next().unwrap_or("unknown");
        return format!("openclaw-agent:{agent}");
    }
    if let Some((_, rest)) = n.split_once("/.openclaw/agents/") {
        let agent = rest.split('/').next().unwrap_or("unknown");
        return format!("openclaw-agent-local:{agent}");
    }
    if n.contains("/.openclaw/backups/") {
        return "openclaw-backup".to_string();
    }
    if n.contains("/.openclaw/memory/") {
        return "openclaw-legacy".to_string();
    }
    if n.contains("/.gemini/antigravity/") {
        return "antigravity".to_string();
    }
    if n.contains("/.gemini/") {
        return "gemini-global".to_string();
    }
    if n.contains("/.tachi/") {
        return "tachi-other".to_string();
    }
    if n.contains("/.sigil/") {
        return "sigil-legacy".to_string();
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_scope_requires_an_exact_configured_home_identity() {
        let configured = Path::new("/configured/.tachi");
        for path in [
            configured.join(memcore::MEMORY_DB_FILENAME),
            configured.join(memcore::LEGACY_MEMORY_DB_FILENAME),
            configured.join("global").join(memcore::MEMORY_DB_FILENAME),
            configured
                .join("global")
                .join(memcore::LEGACY_MEMORY_DB_FILENAME),
        ] {
            assert_eq!(scope_hint_for_in_home(&path, configured), "global");
        }

        assert_ne!(
            scope_hint_for_in_home(Path::new("/repo/.tachi/global/memory.db"), configured),
            "global"
        );
        assert_ne!(
            scope_hint_for_in_home(Path::new("/repo/.tachi/memory.db"), configured),
            "global"
        );

        if let Some(user_home) = dirs::home_dir() {
            let custom_home = user_home.join("configured-custom-tachi-home-not-default");
            for default_home in [user_home.join(".tachi"), user_home.join(".sigil")] {
                for filename in [
                    memcore::MEMORY_DB_FILENAME,
                    memcore::LEGACY_MEMORY_DB_FILENAME,
                ] {
                    for path in [
                        default_home.join(filename),
                        default_home.join("global").join(filename),
                    ] {
                        assert_ne!(
                            scope_hint_for_in_home(&path, &custom_home),
                            "global",
                            "default-home DB must not override configured authority: {}",
                            path.display()
                        );
                    }
                }
            }
        }
    }
}
