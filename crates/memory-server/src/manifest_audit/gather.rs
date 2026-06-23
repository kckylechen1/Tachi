use super::resolve::resolve_owning_repo;
use super::types::ProjectDbInput;
use std::path::{Path, PathBuf};

/// Enumerate `~/.tachi/projects/*/memory.db`, gather filesystem facts, resolve
/// owning repos from the manifest + candidate git roots, and return the inputs.
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
        let db_path = proj_dir.join("memory.db");
        // `symlink_metadata` does NOT follow the final symlink, so we can tell a
        // link apart from a real file.
        let meta = match std::fs::symlink_metadata(&db_path) {
            Ok(m) => m,
            Err(_) => continue, // no memory.db in this project dir
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
