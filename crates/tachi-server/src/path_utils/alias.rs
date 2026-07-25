use super::home::tachi_home;
use blake2::{Blake2s256, Digest};
use std::path::{Component, Path, PathBuf};

const PROJECT_ID_PREFIX_MAX_LEN: usize = 48;
const PROJECT_ID_HASH_HEX_LEN: usize = 24;

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
/// Stable root-derived directory identity for the Plan C alias
/// (`~/.tachi/projects/<name>/tachi-memory.db`).
///
/// The name is `<bounded-readable-prefix>-<hash24>`, where `<hash24>` is the
/// first 24 hex characters of a BLAKE2s digest of the canonical absolute
/// git-root identity. The hash suffix keeps
/// two different repos that share a basename (e.g. `~/work/api` and `~/oss/api`)
/// from colliding on a single alias directory (which previously produced a
/// split-brain warning only). For repos that already have a legacy un-hashed alias
/// dir on disk, resolution falls back to it via
/// [`plan_c_existing_alias_db_for_root`] so existing data is never orphaned.
pub(crate) fn plan_c_dir_name_from_root(project_root: &Path) -> Option<String> {
    let canonical_root = std::fs::canonicalize(project_root).ok()?;
    let raw = canonical_root.file_name()?.to_str()?;
    let sanitized = crate::utils::sanitize_safe_path_name(raw);
    let readable = if sanitized == "unnamed" && raw.trim() != "unnamed" {
        "project"
    } else {
        sanitized.as_str()
    };
    let prefix = readable
        .chars()
        .take(PROJECT_ID_PREFIX_MAX_LEN)
        .collect::<String>();
    let identity = plan_c_alias_identity(&canonical_root);
    let hash = format!("{:x}", Blake2s256::digest(&identity));
    Some(format!("{prefix}-{}", &hash[..PROJECT_ID_HASH_HEX_LEN]))
}

/// Gen-3 (case-folded FNV-8) `<lossy-prefix>-<fnv8>` identity, the immediate
/// pre-#1228 scheme (added by #493). It remains a read/alias compatibility
/// candidate only; new registrations never emit it.
pub(crate) fn plan_c_previous_dir_name_from_root(project_root: &Path) -> Option<String> {
    let raw = project_root.file_name()?.to_str()?;
    let base = crate::utils::sanitize_safe_path_name(raw);
    let canonical = std::fs::canonicalize(project_root)
        .ok()?
        .to_string_lossy()
        .to_string();
    let identity = plan_c_casefold_alias_identity(&canonical);
    let hash = crate::utils::stable_hash(&identity);
    Some(format!("{base}-{}", &hash[..8]))
}

/// Gen-2 (raw-canonical FNV-8) `<lossy-prefix>-<fnv8>` identity — the ORIGINAL
/// hashed scheme introduced by #424 (commit b1c3bb26), before #493 case-folded
/// the canonical path. On case-insensitive hosts (macOS/Windows) a repo whose
/// path contains ASCII uppercase produced a DIFFERENT suffix than the gen-3
/// (case-folded) form, so this generation is physically distinct on disk and
/// must be its own compatibility candidate. Omitting it (as the salvage draft
/// did) means gen-2 alias data is invisible to resolution: the caller then
/// registers a fresh empty DB under the new identity and the real data is
/// silently orphaned — a fail-closed violation (#1228 invariant #5). It remains
/// a read/alias compatibility candidate only; new registrations never emit it.
pub(crate) fn plan_c_previous_raw_dir_name_from_root(project_root: &Path) -> Option<String> {
    let raw = project_root.file_name()?.to_str()?;
    let base = crate::utils::sanitize_safe_path_name(raw);
    // Gen-2 hashed the RAW canonical path string, not the case-folded one.
    let canonical = std::fs::canonicalize(project_root)
        .ok()?
        .to_string_lossy()
        .to_string();
    let hash = crate::utils::stable_hash(&canonical);
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

fn plan_c_alias_identity(canonical: &Path) -> Vec<u8> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let mut bytes = canonical.as_os_str().as_encoded_bytes().to_vec();
        bytes.make_ascii_lowercase();
        bytes
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        canonical.as_os_str().as_encoded_bytes().to_vec()
    }
}

/// Legacy (pre-hash) sanitized alias directory name: just the sanitized basename.
///
/// Retained so we can resolve and keep using alias directories created before the
/// stable-hash suffix was introduced (backward compatibility — do not orphan).
pub(crate) fn plan_c_legacy_dir_name_from_root(project_root: &Path) -> Option<String> {
    let raw = project_root.file_name()?.to_str()?;
    if !raw.is_ascii() {
        return None;
    }
    let legacy = crate::utils::sanitize_safe_path_name(raw);
    // A bare `unnamed` path has no provenance that can distinguish a literal
    // ASCII project from historical lossy collapse. Never auto-claim it.
    (legacy != "unnamed").then_some(legacy)
}

/// Return the alias `tachi-memory.db` path that should be USED for a given repo root,
/// preferring the hashed dir but falling back to a pre-existing legacy un-hashed
/// dir when the hashed one does not yet exist. Used when creating/resolving the
/// alias so repos that predate the hash suffix keep addressing their old data.
///
/// Returns an error when the root cannot produce a stable identity or when
/// compatibility aliases disagree about the physical DB.
pub(crate) fn plan_c_alias_db_for_root(project_root: &Path) -> Result<PathBuf, String> {
    let current = plan_c_dir_name_from_root(project_root).ok_or_else(|| {
        format!(
            "project root '{}' cannot be canonicalized into a stable identity",
            project_root.display()
        )
    })?;
    if let Some(existing) = existing_compatible_alias_for_root(project_root, &current)? {
        return Ok(existing);
    }
    Ok(plan_c_global_db_path(&current))
}

/// Like [`plan_c_alias_db_for_root`] but only returns a path when an alias DB
/// (hashed or legacy) actually exists on disk. Used by reverse lookups.
pub(crate) fn plan_c_existing_alias_db_for_root(
    project_root: &Path,
) -> Result<Option<PathBuf>, String> {
    let current = plan_c_dir_name_from_root(project_root).ok_or_else(|| {
        format!(
            "project root '{}' cannot be canonicalized into a stable identity",
            project_root.display()
        )
    })?;
    existing_compatible_alias_for_root(project_root, &current)
}

fn existing_compatible_alias_for_root(
    project_root: &Path,
    current: &str,
) -> Result<Option<PathBuf>, String> {
    // Reconcile every historical naming generation as one candidate set so no
    // pre-#1228 alias data is ever orphaned into a fresh empty DB:
    //   * `current`      — gen-4, BLAKE2s-24 (this PR).
    //   * gen-3          — case-folded FNV-8 (#493).
    //   * gen-2          — raw-canonical FNV-8 (#424, b1c3bb26).
    //   * gen-1 (legacy) — bare sanitized basename, no hash.
    let mut names = vec![current.to_string()];
    for candidate in [
        plan_c_previous_dir_name_from_root(project_root),
        plan_c_previous_raw_dir_name_from_root(project_root),
        plan_c_legacy_dir_name_from_root(project_root),
    ]
    .into_iter()
    .flatten()
    {
        if !names.contains(&candidate) {
            names.push(candidate);
        }
    }

    let mut paths = Vec::new();
    for name in names {
        let dir = tachi_home().join("projects").join(name);
        for filename in [
            memcore::MEMORY_DB_FILENAME,
            memcore::LEGACY_MEMORY_DB_FILENAME,
        ] {
            paths.push(dir.join(filename));
        }
    }
    resolve_existing_alias_paths(paths)
}

pub(crate) fn plan_c_existing_named_alias_db(
    project_name: &str,
) -> Result<Option<PathBuf>, String> {
    let dir = tachi_home().join("projects").join(project_name);
    resolve_existing_alias_paths([
        dir.join(memcore::MEMORY_DB_FILENAME),
        dir.join(memcore::LEGACY_MEMORY_DB_FILENAME),
    ])
}

fn resolve_existing_alias_paths(
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let mut candidates = Vec::new();
    for path in paths {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let identity = std::fs::canonicalize(&path).map_err(|err| {
                    format!(
                        "Plan C alias candidate is broken or unreadable at {}: {err}",
                        path.display()
                    )
                })?;
                candidates.push((path, identity));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(format!(
                    "Plan C alias candidate cannot be inspected at {}: {err}",
                    path.display()
                ));
            }
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    let first_identity = &candidates[0].1;
    if candidates
        .iter()
        .any(|(_, identity)| identity != first_identity)
    {
        let paths = candidates
            .iter()
            .map(|(path, _)| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Plan C project identity is ambiguous across divergent aliases: {paths}"
        ));
    }
    Ok(Some(candidates.remove(0).0))
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

pub(crate) fn canonical_db_leaf_exists_without_symlink(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "canonical repo DB path {} must not be a symlink; the documented Plan C alias is a separate managed path",
            path.display()
        )),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "inspect canonical repo DB path {} without following links: {error}",
            path.display()
        )),
    }
}

/// Resolve `project_root` + `db_relpath` and ensure the result stays inside `project_root`.
pub(crate) fn resolve_project_db_path(project_root: &Path, rel: &Path) -> Result<PathBuf, String> {
    validate_project_db_relpath(rel)?;
    let joined = project_root.join(rel);
    let joined_exists = canonical_db_leaf_exists_without_symlink(&joined)?;
    let root_canon = std::fs::canonicalize(project_root)
        .map_err(|e| format!("canonicalize project_root: {e}"))?;
    let resolved = if joined_exists {
        std::fs::canonicalize(&joined).map_err(|e| format!("canonicalize db_path: {e}"))?
    } else if let Some(parent) = joined.parent() {
        let mut existing_ancestor = parent;
        while !existing_ancestor.exists() {
            existing_ancestor = existing_ancestor
                .parent()
                .ok_or_else(|| "db_relpath has no existing ancestor".to_string())?;
        }
        let ancestor_canon = std::fs::canonicalize(existing_ancestor)
            .map_err(|e| format!("canonicalize project db ancestor: {e}"))?;
        let suffix = joined.strip_prefix(existing_ancestor).map_err(|_| {
            "db_relpath cannot be reconstructed from its existing ancestor".to_string()
        })?;
        ancestor_canon.join(suffix)
    } else {
        return Err("db_relpath must include a file name".to_string());
    };
    if !resolved.starts_with(&root_canon) {
        return Err("db_relpath escapes project_root".to_string());
    }
    canonical_db_leaf_exists_without_symlink(&joined)?;
    Ok(resolved)
}
