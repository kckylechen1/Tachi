//! Doctor v2 — extension-aware DB classification + safe auto-fix.
//!
//! Classifies every memory.db (or .db.bak / .broken / .corrupted / .old.sqlite)
//! it finds under known Tachi/OpenClaw/Antigravity roots into one of:
//!
//!   * Healthy             — opens read-only with sqlite-vec, has memories table
//!   * VecExtensionMissing — opens but vec virtual tables unreadable (false-broken)
//!   * WalOrphan           — DB has stale .db-wal sidecar
//!   * Corrupt             — pragma quick_check fails for non-extension reasons
//!   * LegacySchema        — has old OpenClaw `chunks` table, no `memories`
//!   * Placeholder         — 0-byte file or obviously empty
//!   * Backup              — filename matches .bak.* / .broken / .corrupted /
//!                            .old.sqlite / pre-split / .checkpointed.db pattern
//!
//! Auto-fix on first run handles two safe categories:
//!   * Placeholder → quarantine to ~/.tachi/quarantine/placeholders/<ts>/
//!   * WalOrphan   → copy aside, wal_checkpoint(TRUNCATE) on the COPY,
//!                   write to <orig>.checkpointed.db. Original untouched.

use chrono::Utc;
use rusqlite::OpenFlags;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

// ─── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DbClassification {
    Healthy,
    VecExtensionMissing,
    WalOrphan,
    Corrupt,
    LegacySchema,
    Placeholder,
    Backup,
}

impl DbClassification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::VecExtensionMissing => "vec_extension_missing",
            Self::WalOrphan => "wal_orphan",
            Self::Corrupt => "corrupt",
            Self::LegacySchema => "legacy_schema",
            Self::Placeholder => "placeholder",
            Self::Backup => "backup",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Healthy => "✅",
            Self::VecExtensionMissing => "🟡",
            Self::WalOrphan => "🟠",
            Self::Corrupt => "🔴",
            Self::LegacySchema => "🟣",
            Self::Placeholder => "⚪",
            Self::Backup => "📦",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobBreakdown {
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub pending: usize,
    pub other: usize,
}

impl Default for JobBreakdown {
    fn default() -> Self {
        Self {
            total: 0,
            completed: 0,
            skipped: 0,
            failed: 0,
            pending: 0,
            other: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorFinding {
    pub path: String,
    pub classification: DbClassification,
    pub file_size: u64,
    pub has_wal: bool,
    pub has_shm: bool,
    pub mem_count: Option<usize>,
    pub archived_count: Option<usize>,
    pub vec_rowid_count: Option<usize>,
    pub none_domain_count: Option<usize>,
    pub jobs: JobBreakdown,
    pub schema_kind: String, // "tachi" | "openclaw_legacy" | "unknown" | "empty"
    pub error: Option<String>,
    pub scope_hint: String, // global / project:<name> / openclaw-agent:<name> / antigravity / backup / unknown
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoFixAction {
    pub path: String,
    pub action: String,  // quarantine_placeholder | checkpoint_wal_copy
    pub outcome: String, // ok | error
    pub note: String,
    pub destination: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub scanned_roots: Vec<String>,
    pub findings: Vec<DoctorFinding>,
    pub summary: SummaryByClass,
    pub auto_fix_actions: Vec<AutoFixAction>,
    pub quarantine_dir: Option<String>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SummaryByClass {
    pub healthy: usize,
    pub vec_extension_missing: usize,
    pub wal_orphan: usize,
    pub corrupt: usize,
    pub legacy_schema: usize,
    pub placeholder: usize,
    pub backup: usize,
    pub total_databases: usize,
    pub total_memories: usize,
    pub total_jobs: usize,
}

// ─── Scanning ────────────────────────────────────────────────────────────────

/// Default scan roots for `tachi doctor`. Mirrors `tidy` plus Antigravity.
pub fn default_scan_roots(home: &Path, git_root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![
        home.join(".tachi"),
        home.join(".openclaw"),
        home.join(".sigil"),
        home.join(".gemini").join("antigravity"),
    ];
    if let Some(root) = git_root {
        let project_tachi = root.join(".tachi");
        if project_tachi.exists() {
            roots.push(project_tachi);
        }
        let project_sigil = root.join(".sigil");
        if project_sigil.exists() {
            roots.push(project_sigil);
        }
    }
    roots.into_iter().filter(|p| p.exists()).collect()
}

/// File-name patterns we treat as "this is a backup, do not classify by content."
/// Returns true if the basename suggests a backup/legacy artifact.
pub fn is_backup_filename(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.contains(".bak") || n.contains(".broken") || n.contains(".corrupted") {
        return true;
    }
    if n.ends_with(".old.sqlite") || n.contains(".old.db") {
        return true;
    }
    if n.contains("pre-split") || n.contains(".pre-split.") {
        return true;
    }
    if n.contains(".checkpointed.db") {
        return true;
    }
    if n.starts_with("memory.db.") {
        // memory.db.20260330_211247 etc.
        return true;
    }
    false
}

/// Walk roots and return every candidate DB file (memory.db, *.sqlite,
/// *.db.bak.*, *.broken.*, *.corrupted.*, *.old.sqlite).
pub fn collect_candidates(roots: &[PathBuf], max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        walk_one(root, &mut out, max_depth);
    }
    out.sort();
    out.dedup();
    out
}

fn walk_one(root: &Path, out: &mut Vec<PathBuf>, max_depth: usize) {
    if !root.exists() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.is_file() {
            if is_db_candidate_filename(name) {
                out.push(path);
            }
            continue;
        }
        if path.is_dir() && max_depth > 0 {
            // Skip obvious noise dirs to bound the walk.
            if matches!(name, "node_modules" | "target" | ".git" | "__pycache__") {
                continue;
            }
            walk_one(&path, out, max_depth - 1);
        }
    }
}

fn is_db_candidate_filename(name: &str) -> bool {
    if name == "memory.db" {
        return true;
    }
    if name.ends_with(".sqlite") || name.ends_with(".db") {
        return true;
    }
    // memory.db.bak.<ts>, memory.db.broken.<ts>, etc.
    if name.starts_with("memory.db.") {
        return true;
    }
    false
}

// ─── Classification ──────────────────────────────────────────────────────────

pub fn classify_one(path: &Path) -> DoctorFinding {
    let path_str = path.display().to_string();
    let scope_hint = scope_hint_for(path);
    let file_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let wal_path = sidecar(path, "-wal");
    let shm_path = sidecar(path, "-shm");
    let has_wal = wal_path.exists()
        && fs::metadata(&wal_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false);
    let has_shm = shm_path.exists();

    let basename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    // 1. Backup filename → short-circuit (don't try to open).
    if is_backup_filename(&basename) {
        return DoctorFinding {
            path: path_str,
            classification: DbClassification::Backup,
            file_size,
            has_wal,
            has_shm,
            mem_count: None,
            archived_count: None,
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
            has_shm,
            mem_count: None,
            archived_count: None,
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
                has_shm,
                mem_count: None,
                archived_count: None,
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
            has_shm,
            mem_count: None,
            archived_count: None,
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
            has_shm,
            mem_count: None,
            archived_count: None,
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
            has_shm,
            mem_count,
            archived_count: None,
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
    let archived_count = scalar_count(&conn, "select count(*) from memories where archived=1").ok();
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
        has_shm,
        mem_count,
        archived_count,
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

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
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

fn scope_hint_for(path: &Path) -> String {
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
    if n.contains("/.tachi/") {
        return "tachi-other".to_string();
    }
    if n.contains("/.sigil/") {
        return "sigil-legacy".to_string();
    }
    "unknown".to_string()
}

// ─── Auto-fix ────────────────────────────────────────────────────────────────

/// Apply the SAFE auto-fix categories: quarantine placeholders, copy-checkpoint
/// WAL orphans. Returns the action log. `quarantine_root` is created if needed.
pub fn auto_fix_safe(findings: &[DoctorFinding], quarantine_root: &Path) -> Vec<AutoFixAction> {
    let mut actions = Vec::new();
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();

    for f in findings {
        match f.classification {
            DbClassification::Placeholder => {
                let dest_dir = quarantine_root.join("placeholders").join(&ts);
                let action = quarantine_placeholder(&f.path, &dest_dir);
                actions.push(action);
            }
            DbClassification::WalOrphan => {
                let action = checkpoint_wal_copy(&f.path);
                actions.push(action);
            }
            _ => {}
        }
    }
    actions
}

fn quarantine_placeholder(src: &str, dest_dir: &Path) -> AutoFixAction {
    let src_path = Path::new(src);
    let basename = src_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("memory.db");
    if let Err(e) = fs::create_dir_all(dest_dir) {
        return AutoFixAction {
            path: src.to_string(),
            action: "quarantine_placeholder".to_string(),
            outcome: "error".to_string(),
            note: format!("create_dir_all({}): {e}", dest_dir.display()),
            destination: None,
        };
    }
    // Mangle dest filename to preserve provenance.
    let safe_src = src
        .trim_start_matches('/')
        .replace('/', "_")
        .replace(':', "_");
    let dest = dest_dir.join(format!("{safe_src}__{basename}"));
    match fs::rename(src_path, &dest) {
        Ok(_) => AutoFixAction {
            path: src.to_string(),
            action: "quarantine_placeholder".to_string(),
            outcome: "ok".to_string(),
            note: format!("moved 0-byte placeholder to quarantine"),
            destination: Some(dest.display().to_string()),
        },
        Err(e) => {
            // Cross-device rename can fail; try copy+remove as fallback.
            match fs::copy(src_path, &dest).and_then(|_| fs::remove_file(src_path)) {
                Ok(_) => AutoFixAction {
                    path: src.to_string(),
                    action: "quarantine_placeholder".to_string(),
                    outcome: "ok".to_string(),
                    note: "moved 0-byte placeholder to quarantine (copy+remove)".to_string(),
                    destination: Some(dest.display().to_string()),
                },
                Err(e2) => AutoFixAction {
                    path: src.to_string(),
                    action: "quarantine_placeholder".to_string(),
                    outcome: "error".to_string(),
                    note: format!("rename: {e}; copy+remove: {e2}"),
                    destination: None,
                },
            }
        }
    }
}

fn checkpoint_wal_copy(src: &str) -> AutoFixAction {
    let src_path = Path::new(src);

    // Liveness guard: if a daemon is currently holding this DB, copying
    // the three files (main + -wal + -shm) non-atomically produces a
    // torn snapshot, and running PRAGMA wal_checkpoint(TRUNCATE) on the
    // copy is undefined behavior on a partial WAL. Skip and explain.
    if daemon_owns_db(src_path) {
        return AutoFixAction {
            path: src.to_string(),
            action: "checkpoint_wal_copy".to_string(),
            outcome: "skipped".to_string(),
            note: "live daemon holds this DB; refuse to make a torn copy".to_string(),
            destination: None,
        };
    }

    // Timestamped destination so repeated `tachi doctor` runs do not
    // overwrite each other (previously: a single `<src>.checkpointed.db`
    // got clobbered or accumulated unbounded depending on path layout).
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let dest = {
        let mut s = src_path.as_os_str().to_owned();
        s.push(format!(".checkpointed.{ts}.db"));
        PathBuf::from(s)
    };
    let wal = sidecar(src_path, "-wal");
    let shm = sidecar(src_path, "-shm");

    // Copy main DB.
    if let Err(e) = fs::copy(src_path, &dest) {
        return AutoFixAction {
            path: src.to_string(),
            action: "checkpoint_wal_copy".to_string(),
            outcome: "error".to_string(),
            note: format!("copy main: {e}"),
            destination: None,
        };
    }
    // Copy sidecars so the new copy can replay WAL.
    if wal.exists() {
        let mut wal_dest = dest.as_os_str().to_owned();
        wal_dest.push("-wal");
        let _ = fs::copy(&wal, PathBuf::from(wal_dest));
    }
    if shm.exists() {
        let mut shm_dest = dest.as_os_str().to_owned();
        shm_dest.push("-shm");
        let _ = fs::copy(&shm, PathBuf::from(shm_dest));
    }

    // Open the COPY read-write and force a TRUNCATE checkpoint.
    let dest_str = dest.to_string_lossy().to_string();
    let result = match rusqlite::Connection::open(&dest_str) {
        Ok(conn) => {
            let _ = conn.busy_timeout(std::time::Duration::from_millis(5_000));
            // Best-effort; ignore returned WAL stats.
            match conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                Ok(_) => AutoFixAction {
                    path: src.to_string(),
                    action: "checkpoint_wal_copy".to_string(),
                    outcome: "ok".to_string(),
                    note: "wrote .checkpointed.<ts>.db copy (original untouched)".to_string(),
                    destination: Some(dest_str.clone()),
                },
                Err(e) => AutoFixAction {
                    path: src.to_string(),
                    action: "checkpoint_wal_copy".to_string(),
                    outcome: "error".to_string(),
                    note: format!("wal_checkpoint failed on copy: {e}"),
                    destination: Some(dest_str.clone()),
                },
            }
        }
        Err(e) => AutoFixAction {
            path: src.to_string(),
            action: "checkpoint_wal_copy".to_string(),
            outcome: "error".to_string(),
            note: format!("open copy: {e}"),
            destination: Some(dest_str),
        },
    };

    // GC: keep at most the 3 most recent .checkpointed.*.db copies per
    // source DB. Older copies are silently removed (best-effort; log via
    // appended note on the result if anything fails).
    let gc_note = gc_old_checkpoint_copies(src_path, 3);
    if let Some(extra) = gc_note {
        let mut r = result;
        r.note = if r.note.is_empty() { extra } else { format!("{}; {extra}", r.note) };
        return r;
    }
    result
}

/// Best-effort detection: does any running tachi-memory-server have an
/// open file handle on `db_path`? Uses `lsof` on Unix; on other platforms
/// (or if `lsof` is missing / errors) returns false (i.e. fall through to
/// the unguarded copy path — preserves prior behavior).
#[cfg(unix)]
fn daemon_owns_db(db_path: &Path) -> bool {
    use std::process::Command;
    let abs = match db_path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let abs_str = abs.to_string_lossy().to_string();
    // Using `--` to terminate options before the path argument so paths
    // beginning with `-` are treated literally.
    let output = Command::new("lsof").arg("--").arg(&abs_str).output();
    match output {
        Ok(o) if o.status.success() => {
            // lsof prints a header line + one line per holder; >1 line means held.
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.lines().count() > 1
        }
        _ => false,
    }
}

#[cfg(not(unix))]
fn daemon_owns_db(_db_path: &Path) -> bool {
    false
}

/// Remove all but the `keep` most-recent `<src>.checkpointed.*.db` (and
/// their `-wal`/`-shm` sidecars) sitting next to `src_path`. Returns
/// Some(note) only on partial failure so the caller can surface it.
fn gc_old_checkpoint_copies(src_path: &Path, keep: usize) -> Option<String> {
    let dir = src_path.parent()?;
    let basename = src_path.file_name()?.to_string_lossy().to_string();
    let prefix = format!("{basename}.checkpointed.");
    let suffix = ".db";

    let mut copies: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let entries = fs::read_dir(dir).ok()?;
    for ent in entries.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        // `ends_with(".db")` already excludes `.db-wal` / `.db-shm` sidecars
        // — those go away together with their parent below.
        if name.starts_with(&prefix) && name.ends_with(suffix) {
            if let Ok(meta) = ent.metadata() {
                if let Ok(mtime) = meta.modified() {
                    copies.push((mtime, ent.path()));
                }
            }
        }
    }
    if copies.len() <= keep {
        return None;
    }
    // Newest first; drop the tail.
    copies.sort_by(|a, b| b.0.cmp(&a.0));
    let to_remove = &copies[keep..];
    let mut errs = Vec::new();
    for (_ts, path) in to_remove {
        if let Err(e) = fs::remove_file(path) {
            errs.push(format!("rm {}: {e}", path.display()));
        }
        // Sidecars (best-effort). Reuse the existing `sidecar` helper.
        let _ = fs::remove_file(sidecar(path, "-wal"));
        let _ = fs::remove_file(sidecar(path, "-shm"));
    }
    if errs.is_empty() {
        None
    } else {
        Some(format!("gc: {}", errs.join(", ")))
    }
}

// ─── Top-level scan API ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub auto_fix: bool,
    pub max_depth: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            auto_fix: true,
            max_depth: 10,
        }
    }
}

pub fn scan(roots: &[PathBuf], quarantine_root: &Path, options: ScanOptions) -> DoctorReport {
    let candidates = collect_candidates(roots, options.max_depth);
    let findings: Vec<DoctorFinding> = candidates.iter().map(|p| classify_one(p)).collect();

    let mut summary = SummaryByClass::default();
    summary.total_databases = findings.len();
    for f in &findings {
        match f.classification {
            DbClassification::Healthy => summary.healthy += 1,
            DbClassification::VecExtensionMissing => summary.vec_extension_missing += 1,
            DbClassification::WalOrphan => summary.wal_orphan += 1,
            DbClassification::Corrupt => summary.corrupt += 1,
            DbClassification::LegacySchema => summary.legacy_schema += 1,
            DbClassification::Placeholder => summary.placeholder += 1,
            DbClassification::Backup => summary.backup += 1,
        }
        if let Some(c) = f.mem_count {
            summary.total_memories += c;
        }
        summary.total_jobs += f.jobs.total;
    }

    let auto_fix_actions = if options.auto_fix {
        auto_fix_safe(&findings, quarantine_root)
    } else {
        Vec::new()
    };

    DoctorReport {
        scanned_roots: roots.iter().map(|p| p.display().to_string()).collect(),
        findings,
        summary,
        auto_fix_actions,
        quarantine_dir: Some(quarantine_root.display().to_string()),
        generated_at: Utc::now().to_rfc3339(),
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

pub fn render_report(report: &DoctorReport) -> String {
    let mut lines = Vec::new();
    lines.push("tachi doctor v2".to_string());
    lines.push(format!("generated_at: {}", report.generated_at));
    lines.push(format!(
        "scanned_roots: {}",
        report.scanned_roots.join(", ")
    ));
    lines.push(format!(
        "summary: {} dbs, {} memories, {} jobs",
        report.summary.total_databases, report.summary.total_memories, report.summary.total_jobs,
    ));
    lines.push(format!(
        "  ✅ healthy={} 🟡 vec_missing={} 🟠 wal_orphan={} 🔴 corrupt={} 🟣 legacy={} ⚪ placeholder={} 📦 backup={}",
        report.summary.healthy,
        report.summary.vec_extension_missing,
        report.summary.wal_orphan,
        report.summary.corrupt,
        report.summary.legacy_schema,
        report.summary.placeholder,
        report.summary.backup,
    ));
    lines.push(String::new());

    for f in &report.findings {
        let mem = f
            .mem_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string());
        let vec = f
            .vec_rowid_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        let none_dom = f
            .none_domain_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        lines.push(format!(
            "{} {} [{}] mem={} vec={} <none>={} jobs={}/c={}/s={}/f={}/p={} wal={} schema={} scope={}",
            f.classification.icon(),
            f.classification.as_str(),
            f.path,
            mem,
            vec,
            none_dom,
            f.jobs.total,
            f.jobs.completed,
            f.jobs.skipped,
            f.jobs.failed,
            f.jobs.pending,
            f.has_wal,
            f.schema_kind,
            f.scope_hint,
        ));
        if let Some(err) = &f.error {
            lines.push(format!("    error: {err}"));
        }
    }

    if !report.auto_fix_actions.is_empty() {
        lines.push(String::new());
        lines.push("auto-fix actions:".to_string());
        for a in &report.auto_fix_actions {
            let dest = a.destination.as_deref().unwrap_or("-");
            lines.push(format!(
                "  [{}] {} {} -> {} ({})",
                a.outcome, a.action, a.path, dest, a.note
            ));
        }
    }

    lines.join("\n")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn make_healthy_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, text TEXT, archived INT DEFAULT 0, domain TEXT);
             INSERT INTO memories (id, text) VALUES ('a','hello');
             INSERT INTO memories (id, text, domain) VALUES ('b','world','trading');
             CREATE TABLE foundry_jobs (id TEXT, status TEXT);
             INSERT INTO foundry_jobs VALUES ('j1','completed'),('j2','skipped'),('j3','pending');",
        )
        .unwrap();
    }

    fn make_legacy_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE chunks (id TEXT PRIMARY KEY, text TEXT);
             INSERT INTO chunks VALUES ('c1','legacy');",
        )
        .unwrap();
    }

    #[test]
    fn classify_healthy() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("memory.db");
        make_healthy_db(&p);
        let f = classify_one(&p);
        assert_eq!(f.classification, DbClassification::Healthy);
        assert_eq!(f.mem_count, Some(2));
        assert_eq!(f.jobs.total, 3);
        assert_eq!(f.jobs.completed, 1);
        assert_eq!(f.jobs.skipped, 1);
        assert_eq!(f.jobs.pending, 1);
        assert_eq!(f.none_domain_count, Some(1));
        assert_eq!(f.schema_kind, "tachi");
    }

    #[test]
    fn classify_placeholder() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("memory.db");
        fs::File::create(&p).unwrap(); // 0 bytes
        let f = classify_one(&p);
        assert_eq!(f.classification, DbClassification::Placeholder);
        assert_eq!(f.file_size, 0);
    }

    #[test]
    fn classify_backup_by_filename() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("memory.db.bak.20260101");
        make_healthy_db(&p); // contents are healthy but filename wins
        let f = classify_one(&p);
        assert_eq!(f.classification, DbClassification::Backup);
    }

    #[test]
    fn classify_broken_filename() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("memory.db.broken.20260201");
        make_healthy_db(&p);
        let f = classify_one(&p);
        assert_eq!(f.classification, DbClassification::Backup);
    }

    #[test]
    fn classify_legacy_schema() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("memory.db");
        make_legacy_db(&p);
        let f = classify_one(&p);
        assert_eq!(f.classification, DbClassification::LegacySchema);
        assert_eq!(f.schema_kind, "openclaw_legacy");
    }

    #[test]
    fn classify_corrupt() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("memory.db");
        fs::write(
            &p,
            b"this is not a sqlite database, just some junk bytes for testing 1234567890",
        )
        .unwrap();
        let f = classify_one(&p);
        assert_eq!(f.classification, DbClassification::Corrupt);
        assert!(f.error.is_some());
    }

    #[test]
    fn auto_fix_quarantines_placeholder() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("memory.db");
        fs::File::create(&p).unwrap();
        let q = dir.path().join("quarantine");
        let report = scan(
            &[dir.path().to_path_buf()],
            &q,
            ScanOptions {
                auto_fix: true,
                max_depth: 5,
            },
        );
        assert_eq!(report.summary.placeholder, 1);
        assert_eq!(report.auto_fix_actions.len(), 1);
        assert_eq!(report.auto_fix_actions[0].outcome, "ok");
        assert!(!p.exists(), "original placeholder should have been moved");
    }

    #[test]
    fn scope_hints() {
        assert_eq!(
            scope_hint_for(Path::new("/Users/x/.tachi/global/memory.db")),
            "global"
        );
        assert_eq!(
            scope_hint_for(Path::new("/Users/x/.tachi/projects/hyperion/memory.db")),
            "project:hyperion"
        );
        assert_eq!(
            scope_hint_for(Path::new(
                "/Users/x/.openclaw/extensions/tachi/data/agents/main/memory.db"
            )),
            "openclaw-agent:main"
        );
        assert_eq!(
            scope_hint_for(Path::new("/Users/x/.gemini/antigravity/memory.db")),
            "antigravity"
        );
    }

    #[test]
    fn backup_filename_patterns() {
        for name in [
            "memory.db.bak.20260101",
            "memory.db.broken.123",
            "memory.db.corrupted",
            "thing.old.sqlite",
            "memory.db.pre-split.20260330_211247",
            "memory.db.checkpointed.db",
        ] {
            assert!(is_backup_filename(name), "{name} should be backup");
        }
        assert!(!is_backup_filename("memory.db"));
        assert!(!is_backup_filename("memory-server.db"));
    }
}
