use rusqlite::OpenFlags;
use std::fs;
use std::path::{Path, PathBuf};

use super::scan::is_backup_filename;
use super::{DbClassification, DoctorFinding, JobBreakdown};

// ─── Classification ──────────────────────────────────────────────────────────

pub fn classify_one(path: &Path) -> DoctorFinding {
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
            jobs: JobBreakdown::default(),
            schema_kind: "empty".to_string(),
            error: None,
            scope_hint,
        };
    }

    // 3. Try a read-only, immutable open (no WAL writes).
    //    URI form: file:<path>?mode=ro&immutable=1
    let uri = make_immutable_uri(path);
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;

    // Register sqlite-vec extension before opening so vec_version()/virtual table reads work.
    memory_core::db::register_sqlite_vec();

    let conn = match rusqlite::Connection::open_with_flags(&uri, flags) {
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
                jobs: JobBreakdown::default(),
                schema_kind: "unknown".to_string(),
                error: Some(format!("open failed: {e}")),
                scope_hint,
            };
        }
    };
    let _ = memory_core::db::try_load_sqlite_vec(&conn);

    // Validate this is actually a SQLite file before any further probing.
    // pragma schema_version is cheap and fails immediately on garbage bytes
    // ("file is not a database" / "not a database").
    if let Err(e) = conn.query_row("pragma schema_version", [], |r| r.get::<_, i64>(0)) {
        return DoctorFinding {
            path: path_str,
            classification: DbClassification::Corrupt,
            file_size,
            has_wal,
            mem_count: None,
            vec_rowid_count: None,
            none_domain_count: None,
            jobs: JobBreakdown::default(),
            schema_kind: "unknown".to_string(),
            error: Some(format!("schema_version probe failed: {e}")),
            scope_hint,
        };
    }

    // Detect schema kind.
    let has_memories = table_exists(&conn, "memories");
    let has_chunks = table_exists(&conn, "chunks");
    let has_memories_vec = table_exists(&conn, "memories_vec");

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
        scalar_count(&conn, "select count(*) from memories")
    } else if has_chunks {
        scalar_count(&conn, "select count(*) from chunks")
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
        match scalar_count(&conn, "select count(*) from memories_vec") {
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
    let none_domain_count = scalar_count(
        &conn,
        "select count(*) from memories where domain is null or domain=''",
    )
    .ok();
    let jobs = job_breakdown(&conn);

    DoctorFinding {
        path: path_str,
        classification,
        file_size,
        has_wal,
        mem_count,
        vec_rowid_count,
        none_domain_count,
        jobs,
        schema_kind,
        error: None,
        scope_hint,
    }
}

fn make_immutable_uri(path: &Path) -> String {
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

fn table_exists(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row(
        "select 1 from sqlite_master where type in ('table','view') and name = ?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

fn scalar_count(conn: &rusqlite::Connection, sql: &str) -> rusqlite::Result<usize> {
    let n: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(n.max(0) as usize)
}

fn job_breakdown(conn: &rusqlite::Connection) -> JobBreakdown {
    let mut b = JobBreakdown::default();
    if !table_exists(conn, "foundry_jobs") {
        return b;
    }
    if let Ok(total) = scalar_count(conn, "select count(*) from foundry_jobs") {
        b.total = total;
    }
    let by_status = |status: &str| -> usize {
        conn.query_row(
            "select count(*) from foundry_jobs where lower(status) = ?1",
            [status],
            |row| row.get::<_, i64>(0).map(|n| n.max(0) as usize),
        )
        .unwrap_or(0)
    };
    b.completed = by_status("completed");
    b.skipped = by_status("skipped");
    b.failed = by_status("failed");
    b.pending = by_status("pending");
    let known = b.completed + b.skipped + b.failed + b.pending;
    b.other = b.total.saturating_sub(known);
    b
}

pub(crate) fn scope_hint_for(path: &Path) -> String {
    let n = path.to_string_lossy().replace('\\', "/");
    if n.contains("/.tachi/global/") {
        return "global".to_string();
    }
    if let Some((_, rest)) = n.split_once("/.tachi/projects/") {
        let proj = rest.split('/').next().unwrap_or("unknown");
        return format!("project:{proj}");
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
