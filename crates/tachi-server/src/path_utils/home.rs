use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TachiHomeSource {
    ExplicitEnv(&'static str),
    WorkspaceData,
    UserDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TachiHomeResolution {
    pub(crate) path: PathBuf,
    pub(crate) source: TachiHomeSource,
}

pub(crate) fn tachi_home() -> PathBuf {
    resolve_tachi_home().path
}

pub(crate) fn resolve_tachi_home() -> TachiHomeResolution {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for key in ["TACHI_HOME", "SIGIL_HOME", "TACHI_APP_HOME"] {
        if let Ok(raw) = std::env::var(key) {
            let path = if raw == "~" {
                Some(home.clone())
            } else if let Some(rest) = raw.strip_prefix("~/") {
                Some(home.join(rest))
            } else if !raw.trim().is_empty() {
                Some(PathBuf::from(raw))
            } else {
                None
            };
            if let Some(path) = path {
                return TachiHomeResolution {
                    path,
                    source: TachiHomeSource::ExplicitEnv(key),
                };
            }
        }
    }
    if let Some(workspace_home) = workspace_data_tachi_home() {
        return TachiHomeResolution {
            path: workspace_home,
            source: TachiHomeSource::WorkspaceData,
        };
    }
    TachiHomeResolution {
        path: home.join(".tachi"),
        source: TachiHomeSource::UserDefault,
    }
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
    let global = path.join("global");
    global.join(memcore::MEMORY_DB_FILENAME).exists()
        || global.join(memcore::LEGACY_MEMORY_DB_FILENAME).exists()
        || path.join("projects").is_dir()
}
