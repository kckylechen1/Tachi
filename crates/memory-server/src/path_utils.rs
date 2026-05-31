use std::path::{Path, PathBuf};

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
        let path = PathBuf::from("/home/u/.tachi/projects/sigil/memory.db");
        assert_eq!(
            named_project_from_path(&path).as_deref(),
            Some("sigil")
        );
    }

    #[test]
    fn named_project_from_path_rejects_external_projects_dir() {
        let path = PathBuf::from("/data/tachi/projects/hyperion/memory.db");
        assert!(named_project_from_path(&path).is_none());
    }
}
