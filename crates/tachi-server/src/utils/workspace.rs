use memcore::MemoryEntry;
use std::path::{Path, PathBuf};

pub(crate) fn is_active_global_rule(entry: &MemoryEntry) -> bool {
    entry
        .metadata
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("DRAFT")
        == "ACTIVE"
}

pub(crate) fn find_git_root_from(path: impl AsRef<std::path::Path>) -> Option<PathBuf> {
    let mut dir = path.as_ref().canonicalize().ok()?;
    if dir.is_file() {
        dir.pop();
    }
    loop {
        if dir.join(".git").exists() {
            return normalize_git_root(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub(crate) fn find_git_root() -> Option<PathBuf> {
    find_git_root_from(std::env::current_dir().ok()?)
}

pub(crate) fn find_project_git_root() -> Option<PathBuf> {
    for var in ["TACHI_PROJECT_ROOT", "TACHI_WORKSPACE_ROOT"] {
        let Some(value) = std::env::var_os(var) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        if let Some(root) = find_git_root_from(PathBuf::from(value)) {
            return Some(root);
        }
    }
    find_git_root().or_else(|| {
        for var in ["PROJECT_ROOT", "WORKSPACE_ROOT", "WORKSPACE", "PWD"] {
            let Some(value) = std::env::var_os(var) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            if let Some(root) = find_git_root_from(PathBuf::from(value)) {
                return Some(root);
            }
        }
        None
    })
}

fn normalize_git_root(root: PathBuf) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(root);
    }
    if !dot_git.is_file() {
        return Some(root);
    }

    if let Some(primary) = linked_worktree_primary_root(&root, &dot_git) {
        return Some(primary);
    }
    Some(root)
}

fn linked_worktree_primary_root(root: &Path, dot_git: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = raw.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = resolve_relative_path(root, gitdir).canonicalize().ok()?;
    let commondir_raw = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let commondir = resolve_relative_path(&gitdir, commondir_raw.trim())
        .canonicalize()
        .ok()?;
    if commondir.file_name().and_then(|name| name.to_str()) == Some(".git") {
        commondir.parent()?.canonicalize().ok()
    } else {
        None
    }
}

fn resolve_relative_path(base: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

pub(crate) fn resolve_home_arg(home: Option<PathBuf>) -> Result<PathBuf, String> {
    home.or_else(dirs::home_dir)
        .ok_or_else(|| "Cannot determine home directory".to_string())
}
