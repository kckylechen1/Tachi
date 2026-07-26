use super::*;

/// Decide how to route jobs found in `db_path`. The daemon's own global +
/// project DBs route via the existing `with_store_for_scope` path; any DB
/// under `~/.tachi/projects/<name>/memory.db` routes as a named project;
/// everything else is currently treated as orphan.
pub(super) fn classify_route(
    entry: &crate::manifest::DbEntry,
    db_path: &Path,
    own_global: &Path,
    own_project: Option<&Path>,
) -> Route {
    classify_route_in_home(
        entry,
        db_path,
        own_global,
        own_project,
        &crate::path_utils::tachi_home(),
    )
}

pub(super) fn classify_route_in_home(
    entry: &crate::manifest::DbEntry,
    db_path: &Path,
    own_global: &Path,
    own_project: Option<&Path>,
    tachi_home: &Path,
) -> Route {
    if paths_equal(db_path, own_global) {
        return Route::Global;
    }
    if let Some(proj) = own_project {
        if paths_equal(db_path, proj) {
            return Route::Project;
        }
    }
    if let Some(name) = crate::path_utils::named_project_for_db_path_in_home(db_path, tachi_home) {
        return Route::NamedProject(name);
    }
    if entry.allow_write
        && entry.schema_kind == "tachi"
        && matches!(
            entry.role,
            DbRole::Agent | DbRole::Foundry | DbRole::Unknown
        )
    {
        return Route::Path;
    }
    // Use scope_hint to give the operator a more readable orphan reason.
    let reason: &'static str = match entry.scope_hint.as_str() {
        "agent" => "agent_db",
        "foundry" => "foundry_db",
        "vault" => "vault_db",
        "hub" => "hub_db",
        "" => "unscoped",
        _ => "unrouted",
    };
    Route::Orphan(reason)
}

pub(super) fn paths_equal(a: &Path, b: &Path) -> bool {
    // Manifest paths are pre-canonicalized by the doctor / manifest
    // pipeline; we compare via std::fs::canonicalize when possible to
    // tolerate symlinks but fall back to lexical equality.
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Build a short label for logs / status. Prefers manifest-supplied
/// scope_hint, falls back to the parent dir + filename.
pub(super) fn manifest_label_for(db_path: &Path, scope_hint: &str) -> String {
    if !scope_hint.is_empty() {
        return scope_hint.to_string();
    }
    let file = db_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?.db");
    let parent = db_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("?");
    format!("{parent}/{file}")
}

/// Cheap deterministic 64-bit hash of a path. Used only for startup
/// jitter; not security-sensitive.
pub(super) fn path_hash(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    h.finish()
}
