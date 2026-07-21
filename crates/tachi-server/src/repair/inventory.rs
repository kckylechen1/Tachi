//! DB inventory helpers: walk the manifest, select by label, daemon-alive check.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::daemon_lock::{legacy_daemon_lock_path, process_alive, read_pid_file};
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

/// True iff any tachi daemon lock under `app_home` records a live PID.
///
/// Checks the legacy `daemon.lock` and every scoped `daemon-<hash>.lock`
/// (same discovery shape as `status_ops::daemon::collect_daemon_inventory`).
/// Used as a coarse “any daemon running” note for skipping R6 VACUUM — does
/// not require `global_db_path` and does not reimplement flock acquisition.
pub fn daemon_alive(app_home: &Path) -> bool {
    let legacy = legacy_daemon_lock_path(app_home);
    if lock_file_holds_live_pid(&legacy) {
        return true;
    }
    let Ok(read_dir) = std::fs::read_dir(app_home) else {
        return false;
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(scope) = name
            .strip_prefix("daemon-")
            .and_then(|value| value.strip_suffix(".lock"))
        {
            // Scoped locks are `daemon-<hex>.lock` (see daemon_scope_id).
            if !scope.is_empty()
                && scope.chars().all(|c| c.is_ascii_hexdigit())
                && lock_file_holds_live_pid(&entry.path())
            {
                return true;
            }
        }
    }
    false
}

fn lock_file_holds_live_pid(lock_path: &Path) -> bool {
    match read_pid_file(lock_path) {
        Some(pid) => process_alive(pid),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn daemon_alive_true_for_scoped_lock_with_live_self_pid() {
        let dir = tempdir().unwrap();
        let app_home = dir.path();
        // No legacy lock — only a scoped daemon-<hash>.lock with this process.
        let scoped = app_home.join("daemon-abc123def456.lock");
        std::fs::write(&scoped, format!("{}\n", std::process::id())).unwrap();
        assert!(
            !legacy_daemon_lock_path(app_home).exists(),
            "legacy lock must be absent for this discrimination"
        );
        assert!(
            daemon_alive(app_home),
            "scoped lock with live self-pid must count as daemon_alive"
        );
    }

    #[test]
    fn daemon_alive_false_when_no_lock_files() {
        let dir = tempdir().unwrap();
        assert!(!daemon_alive(dir.path()));
    }
}
