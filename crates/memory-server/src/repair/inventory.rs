//! DB inventory helpers: walk the manifest, select by label, daemon-alive check.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::daemon_lock::{process_alive, read_pid_file};
use crate::manifest::{classify_db_schema, DbEntry, DbRole, Manifest, SchemaKind};

/// Stable human-friendly label for a DB entry.
///
/// Examples:
///   global
///   project:hapi
///   agent:openclaw-agent:jayne
///   path:/Users/kc/.openclaw/.tachi/memory.db
pub fn label_for(entry: &DbEntry) -> String {
    if !entry.scope_hint.is_empty() {
        return entry.scope_hint.clone();
    }
    match entry.role {
        crate::manifest::DbRole::Global => "global".to_string(),
        _ => format!("path:{}", entry.path),
    }
}

/// Select DBs from a manifest, filtered by an optional label/path string.
///
/// Filter accepts:
///   - `"global"` (case-insensitive)
///   - `"project:NAME"` or any value matching `entry.scope_hint`
///   - exact `entry.path`
///   - absolute path that canonicalizes to `entry.path`
///   - substring match on `entry.path` (case-insensitive) as a last resort
///
/// `None` returns ALL writable, on-disk Tachi DBs (skips non-tachi schema
/// files like openclaw_legacy `.sqlite`).
pub fn select_dbs(manifest: &Manifest, filter: Option<&str>) -> Vec<DbEntry> {
    let want = filter.map(|s| s.trim().to_string());
    let mut seen = HashSet::new();

    let mut entries: Vec<DbEntry> = manifest
        .dbs
        .iter()
        .filter(|e| {
            // Skip non-tachi schemas — they don't have memories/foundry tables.
            if e.schema_kind != "tachi" {
                return false;
            }
            // Skip files that disappeared between manifest GC and now.
            if !PathBuf::from(&e.path).exists() {
                return false;
            }
            true
        })
        .filter(|e| match &want {
            None => true,
            Some(w) => matches(e, w),
        })
        .filter(|e| {
            let key = std::fs::canonicalize(&e.path)
                .unwrap_or_else(|_| PathBuf::from(&e.path))
                .to_string_lossy()
                .to_string();
            seen.insert(key)
        })
        .cloned()
        .collect();

    if entries.is_empty() {
        if let Some(w) = want.as_deref() {
            if let Some(entry) = explicit_path_entry(w) {
                entries.push(entry);
            }
        }
    }

    entries
}

fn matches(e: &DbEntry, want: &str) -> bool {
    if want.eq_ignore_ascii_case("global") {
        return matches!(e.role, crate::manifest::DbRole::Global);
    }
    if !e.scope_hint.is_empty() && e.scope_hint.eq_ignore_ascii_case(want) {
        return true;
    }
    if e.path == want {
        return true;
    }
    // Canonicalize both sides for path comparison.
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(&e.path), std::fs::canonicalize(want)) {
        if a == b {
            return true;
        }
    }
    e.path.to_lowercase().contains(&want.to_lowercase())
}

fn explicit_path_entry(want: &str) -> Option<DbEntry> {
    let path = PathBuf::from(want);
    if !path.is_absolute() || !path.exists() {
        return None;
    }
    if classify_db_schema(&path) != SchemaKind::Tachi {
        return None;
    }
    let canonical = std::fs::canonicalize(&path).unwrap_or(path);
    let path_str = canonical.to_string_lossy().to_string();
    Some(DbEntry {
        path: path_str.clone(),
        role: DbRole::Unknown,
        owner: "external".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: String::new(),
        last_classification: "explicit_path".to_string(),
        scope_hint: format!("path:{path_str}"),
        notes: "synthetic repair entry for explicit --db path outside manifest".to_string(),
    })
}

/// Resolve a single label/path to one DbEntry. Returns None if no match or
/// ambiguous.
pub fn resolve_one(manifest: &Manifest, want: &str) -> Option<DbEntry> {
    let hits = select_dbs(manifest, Some(want));
    if hits.len() == 1 {
        hits.into_iter().next()
    } else {
        None
    }
}

/// True iff a tachi daemon is currently holding `~/.tachi/daemon.lock`.
pub fn daemon_alive(app_home: &Path) -> bool {
    let lock_path = app_home.join("daemon.lock");
    match read_pid_file(&lock_path) {
        Some(pid) => process_alive(pid),
        None => false,
    }
}
