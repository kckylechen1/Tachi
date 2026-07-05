use std::path::PathBuf;

pub(crate) fn tachi_home() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for key in ["TACHI_HOME", "SIGIL_HOME", "TACHI_APP_HOME"] {
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
    if let Some(workspace_home) = workspace_data_tachi_home() {
        return workspace_home;
    }
    home.join(".tachi")
}

fn workspace_data_tachi_home() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join("data").join("tachi");
        if is_tachi_home_layout(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_tachi_home_layout(path: &std::path::Path) -> bool {
    path.join("global").join("memory.db").exists() || path.join("projects").is_dir()
}
