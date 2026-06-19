use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PlanCSplitBrain {
    pub(crate) project_name: String,
    pub(crate) canonical_db: PathBuf,
    pub(crate) alias_db: PathBuf,
    pub(crate) canonical_rows: Option<i64>,
    pub(crate) alias_rows: Option<i64>,
    pub(crate) canonical_bytes: Option<u64>,
    pub(crate) alias_bytes: Option<u64>,
}

impl PlanCSplitBrain {
    pub(crate) fn warning_message(&self) -> String {
        format!(
            "Plan C split-brain detected for project '{}': repo-local DB {} (rows={}, bytes={}) and alias DB {} (rows={}, bytes={}) are different regular files. Back up both, merge by id into the repo-local DB, then replace the alias with a symlink to the repo-local DB.",
            self.project_name,
            self.canonical_db.display(),
            opt_i64(self.canonical_rows),
            opt_u64(self.canonical_bytes),
            self.alias_db.display(),
            opt_i64(self.alias_rows),
            opt_u64(self.alias_bytes),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanCLinkOutcome {
    AlreadyLinked,
    Created(PathBuf),
    SplitBrain(PlanCSplitBrain),
    Skipped(&'static str),
    Failed { path: PathBuf, error: String },
}

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

pub(crate) fn plan_c_project_root_from_local_db(local_db: &Path) -> Option<PathBuf> {
    let tachi_dir = local_db.parent()?;
    if tachi_dir.file_name().and_then(|name| name.to_str()) != Some(".tachi") {
        return None;
    }
    tachi_dir.parent().map(Path::to_path_buf)
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
pub(crate) fn ensure_plan_c_symlink(local_db: &Path, project_root: &Path) -> PlanCLinkOutcome {
    let Some(dir_name) = plan_c_dir_name_from_root(project_root) else {
        return PlanCLinkOutcome::Skipped("project root has no directory name");
    };
    let projects_root = tachi_home().join("projects");
    if local_db.starts_with(&projects_root) {
        return PlanCLinkOutcome::Skipped("local db is already under the Plan C projects root");
    }
    let global_project_dir = projects_root.join(&dir_name);
    if std::fs::create_dir_all(&global_project_dir).is_err() {
        return PlanCLinkOutcome::Skipped("failed to create Plan C project directory");
    }
    let global_link = global_project_dir.join("memory.db");
    let link_is_correct = global_link.is_symlink()
        && std::fs::read_link(&global_link)
            .is_ok_and(|target| target == local_db || canonical_paths_equal(&target, local_db));
    if link_is_correct {
        return PlanCLinkOutcome::AlreadyLinked;
    }
    if global_link.is_symlink() {
        let _ = std::fs::remove_file(&global_link);
    } else if global_link.exists() {
        if let Some(split_brain) = plan_c_split_brain(local_db, project_root) {
            tracing::warn!(
                project = %split_brain.project_name,
                canonical_db = %split_brain.canonical_db.display(),
                alias_db = %split_brain.alias_db.display(),
                canonical_rows = ?split_brain.canonical_rows,
                alias_rows = ?split_brain.alias_rows,
                "Plan C global alias exists as a regular file and diverges from repo-local DB"
            );
            return PlanCLinkOutcome::SplitBrain(split_brain);
        }
        tracing::warn!(
            path = %global_link.display(),
            "Plan C global link exists as a regular file; skipping symlink"
        );
        return PlanCLinkOutcome::Skipped("Plan C global link exists as a regular file");
    }
    if let Err(e) = std::os::unix::fs::symlink(local_db, &global_link) {
        tracing::warn!(error = %e, path = %global_link.display(), "Failed to create Plan C symlink");
        return PlanCLinkOutcome::Failed {
            path: global_link,
            error: e.to_string(),
        };
    }
    PlanCLinkOutcome::Created(global_link)
}

#[cfg(not(unix))]
pub(crate) fn ensure_plan_c_symlink(_local_db: &Path, _project_root: &Path) -> PlanCLinkOutcome {
    PlanCLinkOutcome::Skipped("Plan C symlink unsupported on non-Unix hosts")
}

pub(crate) fn plan_c_split_brain_for_local_db(local_db: &Path) -> Option<PlanCSplitBrain> {
    let project_root = plan_c_project_root_from_local_db(local_db)?;
    plan_c_split_brain(local_db, &project_root)
}

pub(crate) fn plan_c_split_brain(local_db: &Path, project_root: &Path) -> Option<PlanCSplitBrain> {
    let project_name = plan_c_dir_name_from_root(project_root)?;
    let projects_root = tachi_home().join("projects");
    if local_db.starts_with(&projects_root) {
        return None;
    }
    let alias_db = plan_c_global_db_path(&project_name);
    let alias_meta = std::fs::symlink_metadata(&alias_db).ok()?;
    if !alias_meta.file_type().is_file() {
        return None;
    }
    if same_file_identity(local_db, &alias_db) {
        return None;
    }
    Some(PlanCSplitBrain {
        project_name,
        canonical_db: local_db.to_path_buf(),
        alias_db: alias_db.clone(),
        canonical_rows: active_memory_count(local_db),
        alias_rows: active_memory_count(&alias_db),
        canonical_bytes: file_len(local_db),
        alias_bytes: Some(alias_meta.len()),
    })
}

fn opt_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn opt_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|metadata| metadata.len())
}

fn active_memory_count(path: &Path) -> Option<i64> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE archived = 0",
        [],
        |row| row.get(0),
    )
    .ok()
}

fn canonical_paths_equal(left: &Path, right: &Path) -> bool {
    std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .is_some_and(|(left, right)| left == right)
}

#[cfg(unix)]
fn same_file_identity(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(left)
        .ok()
        .zip(std::fs::metadata(right).ok())
        .is_some_and(|(left, right)| left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(not(unix))]
fn same_file_identity(left: &Path, right: &Path) -> bool {
    canonical_paths_equal(left, right)
}

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

/// Resolve any DB path that is addressable through Plan C named-project
/// routing (`~/.tachi/projects/<name>/memory.db`) back to `<name>`.
///
/// This accepts both the Plan C path itself and repo-local `.tachi/memory.db`
/// targets when the Plan C symlink points at the same canonical DB.
pub(crate) fn named_project_for_db_path(db_path: &Path) -> Option<String> {
    if let Some(name) = named_project_from_path(db_path) {
        return Some(name);
    }

    let canonical = std::fs::canonicalize(db_path).ok()?;

    if db_path.file_name().and_then(|name| name.to_str()) == Some("memory.db") {
        if let Some(project_root) = db_path.parent().and_then(|parent| {
            (parent.file_name().and_then(|name| name.to_str()) == Some(".tachi"))
                .then(|| parent.parent())
                .flatten()
        }) {
            if let Some(name) = plan_c_dir_name_from_root(project_root) {
                let named_path = plan_c_global_db_path(&name);
                if std::fs::canonicalize(&named_path)
                    .map(|path| path == canonical)
                    .unwrap_or(false)
                {
                    return Some(name);
                }
            }
        }
    }

    let projects_dir = tachi_home().join("projects");
    let entries = std::fs::read_dir(projects_dir).ok()?;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let candidate = entry.path().join("memory.db");
        if std::fs::canonicalize(&candidate)
            .map(|path| path == canonical)
            .unwrap_or(false)
        {
            return entry.file_name().to_str().map(str::to_string);
        }
    }

    None
}

/// List named project names under `<tachi_home>/projects/` that contain a
/// `memory.db` (regular file or a Plan-C symlink to a live repo DB). Used by the
/// daemon's periodic WAL checkpoint so busy named projects (e.g. hyperion) get
/// their `-wal` reclaimed too — not just the global + workspace-project stores.
pub(crate) fn list_named_projects() -> Vec<String> {
    let projects_dir = tachi_home().join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        // `exists()` follows the symlink, so Plan-C aliases pointing at a live
        // repo DB count; broken aliases are skipped.
        if entry.path().join("memory.db").exists() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names
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
    fn list_named_projects_finds_dirs_with_memory_db() {
        with_env_lock(|| {
            let saved = std::env::var_os("TACHI_HOME");
            let tmp = tempfile::tempdir().unwrap();
            std::env::set_var("TACHI_HOME", tmp.path());
            let projects = tmp.path().join("projects");
            for name in ["alpha", "beta", "nodb"] {
                std::fs::create_dir_all(projects.join(name)).unwrap();
            }
            std::fs::File::create(projects.join("alpha").join("memory.db")).unwrap();
            std::fs::File::create(projects.join("beta").join("memory.db")).unwrap();
            // "nodb" has no memory.db -> excluded.
            let mut got = list_named_projects();
            got.sort();
            assert_eq!(got, vec!["alpha".to_string(), "beta".to_string()]);
            restore_env("TACHI_HOME", saved);
        });
    }

    #[test]
    fn named_project_from_path_rejects_external_projects_dir() {
        let path = PathBuf::from("/data/tachi/projects/hyperion/memory.db");
        assert!(named_project_from_path(&path).is_none());
    }

    #[test]
    fn named_project_for_db_path_accepts_plan_c_symlink_target() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tmp");
            let saved = std::env::var_os("TACHI_HOME");
            let tachi_home = tmp.path().join("home");
            std::env::set_var("TACHI_HOME", &tachi_home);

            let repo = tmp.path().join("Quant Analyzer");
            let local_db = repo.join(".tachi/memory.db");
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
            std::fs::write(&local_db, b"").expect("local db placeholder");
            ensure_plan_c_symlink(&local_db, &repo);

            assert_eq!(
                named_project_for_db_path(&local_db).as_deref(),
                Some("Quant_Analyzer")
            );

            restore_env("TACHI_HOME", saved);
        });
    }

    #[test]
    fn plan_c_regular_alias_file_reports_split_brain() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tmp");
            let saved = std::env::var_os("TACHI_HOME");
            let tachi_home = tmp.path().join("home");
            std::env::set_var("TACHI_HOME", &tachi_home);

            let repo = tmp.path().join("Split Brain Repo");
            let local_db = repo.join(".tachi/memory.db");
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
            memory_core::MemoryStore::open(local_db.to_str().expect("local db"))
                .expect("create local db");

            let alias_db = plan_c_global_db_path("Split_Brain_Repo");
            std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
            memory_core::MemoryStore::open(alias_db.to_str().expect("alias db"))
                .expect("create alias db");

            let outcome = ensure_plan_c_symlink(&local_db, &repo);
            let PlanCLinkOutcome::SplitBrain(issue) = outcome else {
                panic!("expected split-brain outcome, got {outcome:?}");
            };
            assert_eq!(issue.project_name, "Split_Brain_Repo");
            assert_eq!(issue.canonical_db, local_db);
            assert_eq!(issue.alias_db, alias_db);
            assert_eq!(issue.canonical_rows, Some(0));
            assert_eq!(issue.alias_rows, Some(0));
            assert!(issue.warning_message().contains("Plan C split-brain"));
            assert!(
                !issue.alias_db.is_symlink(),
                "regular alias file must not be silently replaced"
            );

            restore_env("TACHI_HOME", saved);
        });
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
