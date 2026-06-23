use std::path::PathBuf;

/// Resolve the owning repo for a real, home-resident project DB.
///
/// Strategy (no mutation):
///   1. If a manifest entry's `scope_hint` is `project:<name>` AND its recorded
///      `path` lives under a repo `.tachi/` (not under `~/.tachi/projects/`),
///      use that repo root.
///   2. Else, if `<git_roots>` contains a repo whose basename equals `<name>`
///      (case-insensitive), use it.
///
/// Returns the repo root (the directory that should contain `.tachi/memory.db`).
pub fn resolve_owning_repo(
    project_name: &str,
    manifest_project_paths: &[(String, String)], // (scope_hint, path)
    candidate_git_roots: &[PathBuf],
) -> Option<PathBuf> {
    let scope = format!("project:{project_name}");
    // (1) Manifest-recorded repo-local path for this project name.
    for (hint, path) in manifest_project_paths {
        if hint != &scope {
            continue;
        }
        let norm = path.replace('\\', "/");
        // Must be a repo-local `.tachi/memory.db`, NOT the central
        // `~/.tachi/projects/<name>/memory.db` we are auditing.
        if norm.contains("/.tachi/projects/") {
            continue;
        }
        if let Some(idx) = norm.rfind("/.tachi/") {
            let repo = &norm[..idx];
            if !repo.is_empty() {
                return Some(PathBuf::from(repo));
            }
        }
    }
    // (2) Git-root basename match.
    let want = project_name.to_ascii_lowercase();
    for root in candidate_git_roots {
        if let Some(base) = root.file_name().and_then(|s| s.to_str()) {
            if base.to_ascii_lowercase() == want {
                return Some(root.clone());
            }
        }
    }
    None
}
