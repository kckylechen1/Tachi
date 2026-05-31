use std::path::{Path, PathBuf};

pub(crate) fn tachi_home() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for key in ["TACHI_HOME", "SIGIL_HOME"] {
        if let Ok(raw) = std::env::var(key) {
            if raw == "~" {
                return home;
            }
            if let Some(rest) = raw.strip_prefix("~/") {
                return home.join(rest);
            }
            if !raw.trim().is_empty() {
                return PathBuf::from(raw);
            }
        }
    }
    home.join(".tachi")
}

/// Extract a project name from `~/.tachi/projects/<name>/memory.db`.
pub(crate) fn named_project_from_path(db_path: &Path) -> Option<String> {
    // Only canonical Tachi project DBs should route as named projects:
    //   .../.tachi/projects/<name>/memory.db
    // Similar-looking external paths such as ".../data/tachi/projects/<name>/memory.db"
    // must stay on explicit path routing so background workers write back to
    // the original DB instead of inventing ~/.tachi/projects/<name>/memory.db.
    let parent = db_path.parent()?;
    let name = parent.file_name()?.to_str()?;
    let grand = parent.parent()?;
    let grand_name = grand.file_name()?.to_str()?;
    let root = grand.parent()?;
    let root_name = root.file_name()?.to_str()?;
    if grand_name == "projects"
        && root_name == ".tachi"
        && db_path.file_name()?.to_str()? == "memory.db"
    {
        Some(name.to_string())
    } else {
        None
    }
}
