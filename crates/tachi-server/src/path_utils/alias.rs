use super::home::tachi_home;
use std::path::{Component, Path, PathBuf};

/// Project-DB addressing convention (read this before touching Plan C code):
///
///   * `<repo>/.tachi/tachi-memory.db` is the **per-repo source of truth** —
///     the real data file, addressed repo-locally (see `bootstrap/serve.rs`).
///   * `~/.tachi/global/tachi-memory.db` is the **machine-global** store.
///   * `~/.tachi/projects/<name>/` is an **addressing alias, not a data store**.
///     On Unix it is a symlink to the repo-local DB; named-project recall resolves
///     a name to a DB path and should prefer the manifest-recorded repo-local path
///     so resolution does not depend on the symlink existing (helps non-Unix).
///
/// (Filenames renamed `memory.db` -> `tachi-memory.db` by #1132; a
/// `memory.db` -> `tachi-memory.db` compat symlink is left behind for one
/// release window, so some of the above may still be reachable via the old
/// name.)
///
/// Sanitized directory name for the Plan C alias
/// (`~/.tachi/projects/<name>/tachi-memory.db`).
///
/// The name is `<sanitized-basename>-<hash8>`, where `<hash8>` is the first 8 hex
/// of a stable hash of the canonical absolute git-root path. The hash suffix keeps
/// two different repos that share a basename (e.g. `~/work/api` and `~/oss/api`)
/// from colliding on a single alias directory (which previously produced a
/// split-brain warning only). For repos that already have a legacy un-hashed alias
/// dir on disk, resolution falls back to it via
/// [`plan_c_existing_alias_db_for_root`] so existing data is never orphaned.
pub(crate) fn plan_c_dir_name_from_root(project_root: &Path) -> Option<String> {
    let base = plan_c_legacy_dir_name_from_root(project_root)?;
    let canonical = std::fs::canonicalize(project_root)
        .unwrap_or_else(|_| project_root.to_path_buf())
        .to_string_lossy()
        .to_string();
    let identity = plan_c_casefold_alias_identity(&canonical);
    let hash = crate::utils::stable_hash(&identity);
    Some(format!("{base}-{}", &hash[..8]))
}

/// Normalize the canonical path before hashing so the Plan C alias identity
/// is stable for case-only spelling differences on case-insensitive
/// filesystems (issue #493).
///
/// `std::fs::canonicalize` resolves symlinks and `.`/`..` but does NOT fold
/// letter case on macOS APFS / Windows NTFS defaults, so `/Users/x/Desktop/Repo`
/// and `/Users/x/desktop/repo` — the same directory on such a filesystem —
/// hashed to two different suffixes and split the project identity. Folding to
/// lowercase on those platforms makes the hash depend only on the real path,
/// not on how it was spelled.
///
/// This is a Plan C alias rule only: `stable_hash` itself is unchanged, the
/// basename is left in its legacy casing, and case-sensitive platforms keep
/// the exact prior behavior so cross-platform collision safety is preserved.
fn plan_c_casefold_alias_identity(canonical: &str) -> String {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        canonical.to_ascii_lowercase()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        canonical.to_string()
    }
}

/// Legacy (pre-hash) sanitized alias directory name: just the sanitized basename.
///
/// Retained so we can resolve and keep using alias directories created before the
/// stable-hash suffix was introduced (backward compatibility — do not orphan).
pub(crate) fn plan_c_legacy_dir_name_from_root(project_root: &Path) -> Option<String> {
    let raw = project_root.file_name()?.to_str()?;
    Some(crate::utils::sanitize_safe_path_name(raw))
}

/// Return the alias `tachi-memory.db` path that should be USED for a given repo root,
/// preferring the hashed dir but falling back to a pre-existing legacy un-hashed
/// dir when the hashed one does not yet exist. Used when creating/resolving the
/// alias so repos that predate the hash suffix keep addressing their old data.
///
/// Returns `None` only when the root has no usable directory name.
pub(crate) fn plan_c_alias_db_for_root(project_root: &Path) -> Option<PathBuf> {
    let hashed = plan_c_dir_name_from_root(project_root)?;
    let hashed_db = plan_c_global_db_path(&hashed);
    // Use the hashed path if it already exists OR no legacy dir exists.
    if hashed_db.exists() {
        return Some(hashed_db);
    }
    if let Some(legacy) = plan_c_legacy_dir_name_from_root(project_root) {
        // Avoid treating the hashed name's own (legacy==hashed is impossible
        // since hashed always carries a suffix) — only fall back to a distinct,
        // already-materialized legacy alias.
        let legacy_db = plan_c_global_db_path(&legacy);
        if legacy != hashed && legacy_db.exists() {
            return Some(legacy_db);
        }
    }
    Some(hashed_db)
}

/// Like [`plan_c_alias_db_for_root`] but only returns a path when an alias DB
/// (hashed or legacy) actually exists on disk. Used by reverse lookups.
pub(crate) fn plan_c_existing_alias_db_for_root(project_root: &Path) -> Option<PathBuf> {
    let hashed = plan_c_dir_name_from_root(project_root)?;
    let hashed_db = plan_c_global_db_path(&hashed);
    if hashed_db.exists() {
        return Some(hashed_db);
    }
    let legacy = plan_c_legacy_dir_name_from_root(project_root)?;
    if legacy != hashed {
        let legacy_db = plan_c_global_db_path(&legacy);
        if legacy_db.exists() {
            return Some(legacy_db);
        }
    }
    None
}

pub(crate) fn plan_c_global_db_path(project_dir_name: &str) -> PathBuf {
    tachi_home()
        .join("projects")
        .join(project_dir_name)
        .join(memcore::MEMORY_DB_FILENAME)
}

/// Like [`plan_c_global_db_path`] but for *resolving an existing* alias
/// rather than deciding what to name a fresh one: prefers the canonical
/// (post-#1132) filename, falling back to the pre-#1132 `memory.db` name so a
/// Plan C alias dir that hasn't been touched by an `open()` call since the
/// rename shipped (and so is still only the legacy-named symlink/file) keeps
/// resolving instead of reporting "not found" against a name that was never
/// created on disk.
pub(crate) fn plan_c_global_db_path_existing(project_dir_name: &str) -> PathBuf {
    let canonical = plan_c_global_db_path(project_dir_name);
    if canonical.exists() {
        return canonical;
    }
    let legacy = tachi_home()
        .join("projects")
        .join(project_dir_name)
        .join(memcore::LEGACY_MEMORY_DB_FILENAME);
    if legacy.exists() {
        return legacy;
    }
    canonical
}

pub(crate) fn plan_c_project_root_from_local_db(local_db: &Path) -> Option<PathBuf> {
    let tachi_dir = local_db.parent()?;
    if tachi_dir.file_name().and_then(|name| name.to_str()) != Some(".tachi") {
        return None;
    }
    tachi_dir.parent().map(Path::to_path_buf)
}

/// Reject absolute paths and `..` segments in `db_relpath`.
pub(crate) fn validate_project_db_relpath(rel: &Path) -> Result<(), String> {
    if rel.as_os_str().is_empty() {
        return Err("db_relpath must not be empty".to_string());
    }
    if rel.is_absolute() {
        return Err("db_relpath must be relative to project_root".to_string());
    }
    for component in rel.components() {
        match component {
            Component::ParentDir => {
                return Err("db_relpath must not contain '..'".to_string());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("db_relpath must be a relative path".to_string());
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

/// Resolve `project_root` + `db_relpath` and ensure the result stays inside `project_root`.
pub(crate) fn resolve_project_db_path(project_root: &Path, rel: &Path) -> Result<PathBuf, String> {
    validate_project_db_relpath(rel)?;
    let joined = project_root.join(rel);
    let root_canon = std::fs::canonicalize(project_root)
        .map_err(|e| format!("canonicalize project_root: {e}"))?;
    let resolved = if joined.exists() {
        std::fs::canonicalize(&joined).map_err(|e| format!("canonicalize db_path: {e}"))?
    } else if let Some(parent) = joined.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create project db parent dir: {e}"))?;
        let parent_canon = std::fs::canonicalize(parent)
            .map_err(|e| format!("canonicalize project db parent: {e}"))?;
        let file_name = joined
            .file_name()
            .ok_or_else(|| "db_relpath must include a file name".to_string())?;
        parent_canon.join(file_name)
    } else {
        return Err("db_relpath must include a file name".to_string());
    };
    if !resolved.starts_with(&root_canon) {
        return Err("db_relpath escapes project_root".to_string());
    }
    Ok(resolved)
}
