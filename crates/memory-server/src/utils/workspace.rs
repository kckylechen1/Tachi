use memory_core::MemoryEntry;
use std::path::PathBuf;

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
            return Some(dir);
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
    for var in [
        "TACHI_PROJECT_ROOT",
        "TACHI_WORKSPACE_ROOT",
        "PROJECT_ROOT",
        "WORKSPACE_ROOT",
        "WORKSPACE",
        "PWD",
    ] {
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
    find_git_root()
}

pub(crate) fn resolve_home_arg(home: Option<PathBuf>) -> Result<PathBuf, String> {
    home.or_else(dirs::home_dir)
        .ok_or_else(|| "Cannot determine home directory".to_string())
}
