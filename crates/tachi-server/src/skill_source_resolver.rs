use std::path::{Component, Path, PathBuf};

/// Resolve a vendored-skill file from its repo-relative path.
///
/// Resolution order is the cached Git root, cwd, Cargo workspace root, then
/// the central skills root. Absolute paths and parent traversal are rejected
/// before any filesystem probe.
pub(crate) fn resolve_vendored_skill_path(rel_path: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel_path);
    if rel_path.is_absolute()
        || rel_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }

    let mut roots = Vec::new();
    if let Some(root) = crate::path_utils::cached_git_root() {
        roots.push(root.clone());
    }
    roots.push(PathBuf::new());
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        roots.push(PathBuf::from(manifest_dir).join("..").join(".."));
    }
    if let Some(root) = central_skills_root() {
        roots.push(root);
    }
    first_existing_path(&roots, rel_path)
}

fn first_existing_path(roots: &[PathBuf], rel_path: &Path) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join(rel_path))
        .find(|candidate| candidate.exists())
}

fn central_skills_root() -> Option<PathBuf> {
    std::env::var("TACHI_SKILLS_ROOT")
        .ok()
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".agents").join("vendored-skills"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SkillsRootGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous_skills_root: Option<std::ffi::OsString>,
        previous_home: Option<std::ffi::OsString>,
        previous_manifest_dir: Option<std::ffi::OsString>,
    }

    impl SkillsRootGuard {
        fn acquire() -> Self {
            let lock = crate::utils::global_test_lock()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            Self {
                _lock: lock,
                previous_skills_root: std::env::var_os("TACHI_SKILLS_ROOT"),
                previous_home: std::env::var_os("HOME"),
                previous_manifest_dir: std::env::var_os("CARGO_MANIFEST_DIR"),
            }
        }

        fn set_skills_root(&self, value: &Path) {
            // SAFETY: the process-global test lock serializes environment mutation.
            unsafe { std::env::set_var("TACHI_SKILLS_ROOT", value) }
        }

        fn set_home(&self, value: &Path) {
            // SAFETY: the process-global test lock serializes environment mutation.
            unsafe { std::env::set_var("HOME", value) }
        }

        fn remove_skills_root(&self) {
            // SAFETY: the process-global test lock serializes environment mutation.
            unsafe { std::env::remove_var("TACHI_SKILLS_ROOT") }
        }

        fn set_manifest_dir(&self, value: &Path) {
            // SAFETY: the process-global test lock serializes environment mutation.
            unsafe { std::env::set_var("CARGO_MANIFEST_DIR", value) }
        }
    }

    impl Drop for SkillsRootGuard {
        fn drop(&mut self) {
            // SAFETY: the process-global test lock remains held during restoration.
            unsafe {
                match &self.previous_skills_root {
                    Some(value) => std::env::set_var("TACHI_SKILLS_ROOT", value),
                    None => std::env::remove_var("TACHI_SKILLS_ROOT"),
                }
                match &self.previous_home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match &self.previous_manifest_dir {
                    Some(value) => std::env::set_var("CARGO_MANIFEST_DIR", value),
                    None => std::env::remove_var("CARGO_MANIFEST_DIR"),
                }
            }
        }
    }

    #[test]
    fn resolves_from_explicit_central_root_after_local_roots_miss() {
        let env = SkillsRootGuard::acquire();
        let unique = uuid::Uuid::new_v4();
        let rel_path = format!("skill/__resolver_fixture_{unique}/SKILL.md");
        let root = std::env::temp_dir().join(format!("tachi-skill-resolver-{unique}"));
        let expected = root.join(&rel_path);
        std::fs::create_dir_all(&root).unwrap();
        env.set_skills_root(&root);

        assert!(resolve_vendored_skill_path(&rel_path).is_none());

        std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
        std::fs::write(&expected, "# fixture\n").unwrap();
        assert_eq!(resolve_vendored_skill_path(&rel_path), Some(expected));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_central_root_precedes_home_fallback() {
        let env = SkillsRootGuard::acquire();
        let unique = uuid::Uuid::new_v4();
        let rel_path = format!("skill/__resolver_precedence_{unique}/SKILL.md");
        let base = std::env::temp_dir().join(format!("tachi-skill-roots-{unique}"));
        let explicit_root = base.join("explicit");
        let home = base.join("home");
        let explicit_path = explicit_root.join(&rel_path);
        let home_path = home.join(".agents").join("vendored-skills").join(&rel_path);
        for path in [&explicit_path, &home_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "# fixture\n").unwrap();
        }
        env.set_home(&home);
        env.set_skills_root(&explicit_root);

        assert_eq!(resolve_vendored_skill_path(&rel_path), Some(explicit_path));

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn home_fallback_resolves_when_explicit_root_is_absent() {
        let env = SkillsRootGuard::acquire();
        let unique = uuid::Uuid::new_v4();
        let rel_path = format!("skill/__resolver_home_{unique}/SKILL.md");
        let home = std::env::temp_dir().join(format!("tachi-skill-home-{unique}"));
        let expected = home.join(".agents").join("vendored-skills").join(&rel_path);
        std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
        std::fs::write(&expected, "# fixture\n").unwrap();
        env.remove_skills_root();
        env.set_home(&home);

        assert_eq!(resolve_vendored_skill_path(&rel_path), Some(expected));

        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn manifest_workspace_root_precedes_central_root() {
        let env = SkillsRootGuard::acquire();
        let unique = uuid::Uuid::new_v4();
        let rel_path = format!("skill/__resolver_manifest_{unique}/SKILL.md");
        let base = std::env::temp_dir().join(format!("tachi-skill-manifest-{unique}"));
        let workspace = base.join("workspace");
        let manifest_dir = workspace.join("crates").join("tachi-server");
        let central = base.join("central");
        let workspace_path = workspace.join(&rel_path);
        let central_path = central.join(&rel_path);
        std::fs::create_dir_all(&manifest_dir).unwrap();
        for path in [&workspace_path, &central_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "# fixture\n").unwrap();
        }
        env.set_manifest_dir(&manifest_dir);
        env.set_skills_root(&central);

        assert_eq!(
            resolve_vendored_skill_path(&rel_path),
            Some(manifest_dir.join("..").join("..").join(&rel_path))
        );

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_absolute_and_parent_paths_before_probing() {
        assert!(resolve_vendored_skill_path("/etc/passwd").is_none());
        assert!(resolve_vendored_skill_path("skill/../../../etc/passwd").is_none());
        assert!(resolve_vendored_skill_path("../outside/SKILL.md").is_none());
    }
}
