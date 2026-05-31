use std::path::{Component, Path, PathBuf};

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
    home.join(".tachi")
}

/// Sanitized directory name for Plan C: `~/.tachi/projects/<name>/memory.db`.
pub(crate) fn plan_c_dir_name_from_root(project_root: &Path) -> Option<String> {
    let raw = project_root.file_name()?.to_str()?;
    Some(crate::utils::sanitize_safe_path_name(raw))
}

pub(crate) fn plan_c_global_db_path(project_dir_name: &str) -> PathBuf {
    tachi_home()
        .join("projects")
        .join(project_dir_name)
        .join("memory.db")
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

/// Global Plan C symlink for a repo-local project DB (Unix only).
#[cfg(unix)]
pub(crate) fn ensure_plan_c_symlink(local_db: &Path, project_root: &Path) {
    let Some(dir_name) = plan_c_dir_name_from_root(project_root) else {
        return;
    };
    let projects_root = tachi_home().join("projects");
    if local_db.starts_with(&projects_root) {
        return;
    }
    let global_project_dir = projects_root.join(&dir_name);
    if std::fs::create_dir_all(&global_project_dir).is_err() {
        return;
    }
    let global_link = global_project_dir.join("memory.db");
    let link_is_correct = global_link.is_symlink()
        && std::fs::read_link(&global_link)
            .map(|target| target == local_db)
            .unwrap_or(false);
    if link_is_correct {
        return;
    }
    if global_link.is_symlink() {
        let _ = std::fs::remove_file(&global_link);
    } else if global_link.exists() {
        tracing::warn!(
            path = %global_link.display(),
            "Plan C global link exists as a regular file; skipping symlink"
        );
        return;
    }
    if let Err(e) = std::os::unix::fs::symlink(local_db, &global_link) {
        tracing::warn!(error = %e, path = %global_link.display(), "Failed to create Plan C symlink");
    }
}

#[cfg(not(unix))]
pub(crate) fn ensure_plan_c_symlink(_local_db: &Path, _project_root: &Path) {}

/// Extract a project name from `<tachi_home>/projects/<name>/memory.db`.
pub(crate) fn named_project_from_path(db_path: &Path) -> Option<String> {
    if db_path.file_name()?.to_str()? != "memory.db" {
        return None;
    }
    let project_dir = db_path.parent()?;
    let projects_dir = tachi_home().join("projects");
    let rel = project_dir.strip_prefix(&projects_dir).ok()?;
    if rel.components().count() != 1 {
        return None;
    }
    rel.to_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env_lock<F: FnOnce()>(f: F) {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        f();
    }

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        if let Some(v) = value {
            std::env::set_var(name, v);
        } else {
            std::env::remove_var(name);
        }
    }

    #[test]
    fn tachi_home_defaults_to_dot_tachi_under_user_home() {
        with_env_lock(|| {
            let saved_home = std::env::var_os("TACHI_HOME");
            let saved_sigil = std::env::var_os("SIGIL_HOME");
            let saved_app = std::env::var_os("TACHI_APP_HOME");
            std::env::remove_var("TACHI_HOME");
            std::env::remove_var("SIGIL_HOME");
            std::env::remove_var("TACHI_APP_HOME");

            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            assert_eq!(tachi_home(), home.join(".tachi"));

            restore_env("TACHI_HOME", saved_home);
            restore_env("SIGIL_HOME", saved_sigil);
            restore_env("TACHI_APP_HOME", saved_app);
        });
    }

    #[test]
    fn tachi_home_honors_tilde_and_tilde_slash() {
        with_env_lock(|| {
            let saved_home = std::env::var_os("TACHI_HOME");
            let saved_sigil = std::env::var_os("SIGIL_HOME");
            let saved_app = std::env::var_os("TACHI_APP_HOME");
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

            std::env::set_var("TACHI_HOME", "~");
            assert_eq!(tachi_home(), home);

            std::env::set_var("TACHI_HOME", "~/custom-tachi");
            assert_eq!(tachi_home(), home.join("custom-tachi"));

            restore_env("TACHI_HOME", saved_home);
            restore_env("SIGIL_HOME", saved_sigil);
            restore_env("TACHI_APP_HOME", saved_app);
        });
    }

    #[test]
    fn tachi_home_falls_back_to_tachi_app_home() {
        with_env_lock(|| {
            let saved_home = std::env::var_os("TACHI_HOME");
            let saved_sigil = std::env::var_os("SIGIL_HOME");
            let saved_app = std::env::var_os("TACHI_APP_HOME");
            std::env::remove_var("TACHI_HOME");
            std::env::remove_var("SIGIL_HOME");
            std::env::set_var("TACHI_APP_HOME", "/tmp/legacy-app-home");

            assert_eq!(tachi_home(), PathBuf::from("/tmp/legacy-app-home"));

            restore_env("TACHI_HOME", saved_home);
            restore_env("SIGIL_HOME", saved_sigil);
            restore_env("TACHI_APP_HOME", saved_app);
        });
    }

    #[test]
    fn named_project_from_path_accepts_canonical_layout() {
        with_env_lock(|| {
            let saved = std::env::var_os("TACHI_HOME");
            std::env::set_var("TACHI_HOME", "/tmp/tachi-test-home");
            let path = PathBuf::from("/tmp/tachi-test-home/projects/sigil/memory.db");
            assert_eq!(named_project_from_path(&path).as_deref(), Some("sigil"));
            restore_env("TACHI_HOME", saved);
        });
    }

    #[test]
    fn named_project_from_path_honors_custom_tachi_home() {
        with_env_lock(|| {
            let saved = std::env::var_os("TACHI_HOME");
            std::env::set_var("TACHI_HOME", "/tmp/custom-tachi-root");
            let path = PathBuf::from("/tmp/custom-tachi-root/projects/my_app/memory.db");
            assert_eq!(named_project_from_path(&path).as_deref(), Some("my_app"));
            restore_env("TACHI_HOME", saved);
        });
    }

    #[test]
    fn named_project_from_path_rejects_external_projects_dir() {
        let path = PathBuf::from("/data/tachi/projects/hyperion/memory.db");
        assert!(named_project_from_path(&path).is_none());
    }

    #[test]
    fn validate_project_db_relpath_rejects_parent_dir() {
        assert!(validate_project_db_relpath(Path::new("../secrets.db")).is_err());
    }

    #[test]
    fn plan_c_dir_name_sanitizes_spaces() {
        let root = PathBuf::from("/tmp/My Cool Repo");
        assert_eq!(
            plan_c_dir_name_from_root(&root).as_deref(),
            Some("My_Cool_Repo")
        );
    }
}
