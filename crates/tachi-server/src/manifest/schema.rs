use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Schema kind detected by inspecting `sqlite_master`. Used by the GC pass to
/// reclassify entries that were tagged `tachi` by historical bugs but actually
/// hold legacy OpenClaw `chunks` tables (audit B8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaKind {
    /// `memories` table with the canonical Tachi columns present.
    Tachi,
    /// `chunks` table present, no `memories` table — legacy OpenClaw store.
    /// Wire form is `openclaw_legacy` for backward compatibility with the
    /// historical `schema_kind` string in `~/.tachi/manifest.json`.
    #[serde(rename = "openclaw_legacy")]
    OpenclawChunks,
    /// Neither table found, or open/probe failed.
    Unknown,
}

impl SchemaKind {
    /// Stable string form used in the manifest JSON. Matches the historical
    /// `schema_kind` values so existing entries deserialize without churn.
    pub fn as_str(&self) -> &'static str {
        match self {
            // Historical manifest used `tachi` / `openclaw_legacy` / `unknown`.
            // Keep `openclaw_legacy` on the wire for backward compatibility;
            // the enum variant is named after the table for clarity.
            SchemaKind::Tachi => "tachi",
            SchemaKind::OpenclawChunks => "openclaw_legacy",
            SchemaKind::Unknown => "unknown",
        }
    }
}

/// Canonicalize a DB path. Falls back to the input path if the file does not
/// exist (e.g. during GC of an entry whose file was moved). Never errors.
///
/// Used as the dedup key everywhere a manifest entry is added or compared.
pub fn canonicalize_db_path(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Returns `Some(reason)` if the path looks like a test fixture / vendored
/// artifact and should NOT be tracked in the live manifest. Patterns:
///
///   * `/node_modules/.../tmp/*.db` (vite zread fixtures, audit B12)
///   * `/node_modules/` with parent dir named `tmp`/`tests`/`test-fixtures`
///   * filename matches `*.test.db` or `*.fixture.db`
///   * filename is `feature-daemon-global.db` AND path contains `vitejs` or
///     `zread` (belt-and-suspenders for the vite zread fixture pattern)
///
/// Path-based archival/backup heuristics. Complements filename rules in
/// [`crate::doctor::is_backup_filename`]: run snapshots, tidy archives, and
/// explicit backup directories should never enter the manifest.
pub fn is_archival_db_path(p: &Path) -> bool {
    let path_str = p.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let markers = [
        "/backups/",
        "/.tachi/cleanup-backups/",
        "/.openclaw/backups/",
        "/claude-code-runs/",
        "/tidy/manual-cleanup-",
        "/data/backup/",
        ".pre-timestamp-fix",
        ".checkpointed.db",
    ];
    markers.iter().any(|m| path_str.contains(m))
}

pub fn should_skip_path(p: &Path) -> Option<&'static str> {
    if is_archival_db_path(p) {
        return Some("archival/backup path");
    }
    let path_str = p.to_string_lossy().replace('\\', "/");
    let file_name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent_name = p
        .parent()
        .and_then(|pp| pp.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // node_modules + tmp anywhere in path
    if path_str.contains("/node_modules/") && path_str.contains("/tmp/") {
        return Some("node_modules tmp fixture");
    }
    // node_modules .db with fixture-shaped parent
    if path_str.contains("/node_modules/")
        && file_name.ends_with(".db")
        && matches!(parent_name.as_str(), "tmp" | "tests" | "test-fixtures")
    {
        return Some("node_modules test fixture");
    }
    // Short-lived smoke/UX workspaces often create Tachi project DBs under
    // /tmp or /private/tmp. They should not become daemon-maintained DBs.
    let path_lower = path_str.to_ascii_lowercase();
    let under_tmp = path_lower.starts_with("/tmp/") || path_lower.starts_with("/private/tmp/");
    if under_tmp
        && (path_lower.contains("/.tachi/memory.db")
            || path_lower.contains("/.tachi/tachi-memory.db")
            || path_lower.contains("/.tachi/project/memory.db")
            || path_lower.contains("/.tachi/project/tachi-memory.db")
            || path_lower.contains("/tachi-recall-smoke"))
    {
        return Some("temporary tachi workspace");
    }
    // generic *.test.db / *.fixture.db
    if file_name.ends_with(".test.db") {
        return Some("*.test.db");
    }
    if file_name.ends_with(".fixture.db") {
        return Some("*.fixture.db");
    }
    // vite zread fixture (B12 belt + suspenders)
    if file_name == "feature-daemon-global.db"
        && (path_str.contains("vitejs") || path_str.contains("zread"))
    {
        return Some("vite zread feature-daemon-global.db fixture");
    }
    None
}

/// Open the SQLite file read-only and decide its schema kind by inspecting
/// `sqlite_master`. Returns `Unknown` if the file cannot be opened or probed.
///
/// Tachi requires both `memories` table presence AND at least the canonical
/// columns (`id`, `text`, `path`, `timestamp`) to call it Tachi — this avoids
/// classifying an OpenClaw legacy DB that happens to also have a stub
/// `memories` view as Tachi.
pub fn classify_db_schema(path: &Path) -> SchemaKind {
    use rusqlite::OpenFlags;
    if !path.exists() {
        return SchemaKind::Unknown;
    }
    // Read-only, no WAL writes. URI form mirrors doctor.rs.
    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        path.to_string_lossy().replace('?', "%3F")
    );
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = match rusqlite::Connection::open_with_flags(&uri, flags) {
        Ok(c) => c,
        Err(_) => return SchemaKind::Unknown,
    };

    let has_memories: bool = conn
        .query_row(
            "select count(*) from sqlite_master where type='table' and name='memories'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    let has_chunks: bool = conn
        .query_row(
            "select count(*) from sqlite_master where type='table' and name='chunks'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);

    if has_memories {
        // Verify canonical columns are present before declaring Tachi.
        let cols: std::collections::HashSet<String> = conn
            .prepare("pragma table_info(memories)")
            .and_then(|mut stmt| {
                let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
                let mut out = std::collections::HashSet::new();
                for row in rows.flatten() {
                    out.insert(row);
                }
                Ok(out)
            })
            .unwrap_or_default();
        let required = ["id", "text", "path", "timestamp"];
        if required.iter().all(|c| cols.contains(*c)) {
            return SchemaKind::Tachi;
        }
        // memories table exists but missing canonical columns → fall through.
    }
    if has_chunks {
        return SchemaKind::OpenclawChunks;
    }
    SchemaKind::Unknown
}
