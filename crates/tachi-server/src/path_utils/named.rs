use super::alias::plan_c_existing_alias_db_for_root;
use super::home::tachi_home;
use std::path::Path;

/// Extract a project name from `<tachi_home>/projects/<name>/tachi-memory.db`
/// (or the pre-#1132 `.../memory.db`, still accepted here so paths sourced
/// from a not-yet-migrated manifest entry keep resolving).
pub(crate) fn named_project_from_path(db_path: &Path) -> Option<String> {
    if !db_path
        .file_name()
        .and_then(|f| f.to_str())
        .map(memcore::is_memory_db_filename)
        .unwrap_or(false)
    {
        return None;
    }
    let project_dir = db_path.parent()?;
    let projects_dir = tachi_home().join("projects");
    let rel = project_dir.strip_prefix(&projects_dir).ok()?;
    if rel.components().count() != 1 {
        return None;
    }
    rel.to_str().map(str::to_string)
}

/// Resolve any DB path that is addressable through Plan C named-project
/// routing (`~/.tachi/projects/<name>/tachi-memory.db`) back to `<name>`.
///
/// This accepts both the Plan C path itself and repo-local
/// `.tachi/tachi-memory.db` targets when the Plan C symlink points at the
/// same canonical DB (also the pre-#1132 `memory.db` name, for paths sourced
/// from a not-yet-migrated manifest entry).
pub(crate) fn named_project_for_db_path(db_path: &Path) -> Option<String> {
    if let Some(name) = named_project_from_path(db_path) {
        return Some(name);
    }

    let canonical = std::fs::canonicalize(db_path).ok()?;

    let db_path_is_memory_db = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(memcore::is_memory_db_filename)
        .unwrap_or(false);
    if db_path_is_memory_db {
        if let Some(project_root) = db_path.parent().and_then(|parent| {
            (parent.file_name().and_then(|name| name.to_str()) == Some(".tachi"))
                .then(|| parent.parent())
                .flatten()
        }) {
            // Fast-path: check the alias that would actually be used for this
            // root (hashed, falling back to a pre-existing legacy un-hashed dir).
            if let Ok(Some(named_path)) = plan_c_existing_alias_db_for_root(project_root) {
                if std::fs::canonicalize(&named_path)
                    .map(|path| path == canonical)
                    .unwrap_or(false)
                {
                    if let Some(name) = named_path
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                    {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }

    let projects_dir = tachi_home().join("projects");
    let entries = std::fs::read_dir(projects_dir).ok()?;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        // Check the canonical (post-#1132) name first, then the legacy name —
        // an alias dir not yet touched by an `open()` call since the rename
        // shipped may still only have the legacy-named symlink/file on disk.
        for name in [
            memcore::MEMORY_DB_FILENAME,
            memcore::LEGACY_MEMORY_DB_FILENAME,
        ] {
            let candidate = entry.path().join(name);
            if std::fs::canonicalize(&candidate)
                .map(|path| path == canonical)
                .unwrap_or(false)
            {
                return entry.file_name().to_str().map(str::to_string);
            }
        }
    }

    None
}

/// List named project names under `<tachi_home>/projects/` that contain a
/// `tachi-memory.db` (regular file or a Plan-C symlink to a live repo DB),
/// or the pre-#1132 `memory.db` for aliases not yet touched by an `open()`
/// call since the rename shipped. Used by the daemon's periodic WAL
/// checkpoint so busy named projects (e.g. hyperion) get their `-wal`
/// reclaimed too — not just the global + workspace-project stores.
pub(crate) fn list_named_projects() -> Vec<String> {
    let projects_dir = tachi_home().join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        // `exists()` follows the symlink, so Plan-C aliases pointing at a live
        // repo DB count; broken aliases are skipped. Check both names since a
        // not-yet-migrated alias may still be the legacy-named symlink only.
        let has_db = [
            memcore::MEMORY_DB_FILENAME,
            memcore::LEGACY_MEMORY_DB_FILENAME,
        ]
        .iter()
        .any(|name| entry.path().join(name).exists());
        if has_db {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names
}
