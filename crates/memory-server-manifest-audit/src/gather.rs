use super::resolve::resolve_owning_repo;
use super::types::ProjectDbInput;
use std::path::{Path, PathBuf};

/// Canonical memory-database filename (post-#1132). Kept as a local constant so
/// this tiny audit crate need not depend on `memcore` for two strings; the
/// single source of truth is `memcore::db::filename::MEMORY_DB_FILENAME` and
/// these MUST stay in lockstep with it.
const MEMORY_DB_FILENAME: &str = "tachi-memory.db";
/// Pre-#1132 filename, still present during the one-release compat window (as a
/// real file on un-migrated stores, or a `-> tachi-memory.db` compat symlink on
/// migrated ones). Mirrors `memcore::db::filename::LEGACY_MEMORY_DB_FILENAME`.
const LEGACY_MEMORY_DB_FILENAME: &str = "memory.db";

/// Enumerate `~/.tachi/projects/*/{tachi-memory.db,memory.db}`, gather
/// filesystem facts, resolve owning repos from the manifest + candidate git
/// roots, and return the inputs.
///
/// This is the I/O shell; it performs only reads (`symlink_metadata`,
/// `read_link`, `exists`). It NEVER moves or deletes anything.
pub fn gather_project_inputs(
    projects_dir: &Path,
    manifest_project_paths: &[(String, String)],
    candidate_git_roots: &[PathBuf],
) -> std::io::Result<Vec<ProjectDbInput>> {
    let mut out = Vec::new();
    if !projects_dir.exists() {
        return Ok(out);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(projects_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for proj_dir in entries {
        if !proj_dir.is_dir() {
            continue;
        }
        let project_name = match proj_dir.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // #1132 compat window: the in-use store is the canonical
        // `tachi-memory.db`; a migrated dir also carries a `memory.db ->
        // tachi-memory.db` compat symlink, and an un-migrated dir still has a
        // real `memory.db`. Prefer the canonical name (that's the authoritative
        // real file post-migration) and only fall back to the legacy name when
        // no canonical file is present — this mirrors the store-open and MCP
        // "canonical-first, legacy-fallback" routing and avoids double-counting
        // a migrated dir's real file plus its compat symlink.
        let canonical_path = proj_dir.join(MEMORY_DB_FILENAME);
        let legacy_path = proj_dir.join(LEGACY_MEMORY_DB_FILENAME);
        // `symlink_metadata` does NOT follow the final symlink, so we can tell a
        // link apart from a real file.
        let (db_path, meta) = match std::fs::symlink_metadata(&canonical_path) {
            Ok(m) => (canonical_path, m),
            Err(_) => match std::fs::symlink_metadata(&legacy_path) {
                Ok(m) => (legacy_path, m),
                Err(_) => continue, // no memory DB (either generation) here
            },
        };
        let is_symlink = meta.file_type().is_symlink();
        let (symlink_target, symlink_target_exists) = if is_symlink {
            match std::fs::read_link(&db_path) {
                Ok(t) => {
                    // Resolve relative links against the project dir.
                    let resolved = if t.is_absolute() {
                        t.clone()
                    } else {
                        proj_dir.join(&t)
                    };
                    let exists = resolved.exists();
                    (Some(resolved), exists)
                }
                Err(_) => (None, false),
            }
        } else {
            (None, false)
        };
        // Only resolve an owning repo for real files (the only relocatable case).
        let owning_repo = if !is_symlink {
            resolve_owning_repo(&project_name, manifest_project_paths, candidate_git_roots)
        } else {
            None
        };
        out.push(ProjectDbInput {
            project_name,
            db_path,
            is_symlink,
            symlink_target,
            symlink_target_exists,
            owning_repo,
        });
    }
    Ok(out)
}
