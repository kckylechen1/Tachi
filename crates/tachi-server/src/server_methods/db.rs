use crate::server_state::{DbScope, MemoryServer};
use memcore::MemoryStore;
use std::path::{Path, PathBuf};

impl MemoryServer {
    /// Check if a project DB is available (static startup or hot-swapped).
    pub(crate) fn has_project_db(&self) -> bool {
        self.db.has_project_db()
    }

    /// Hot-activate a project database on a running server.
    /// Called by `tachi_init_project_db` to make the created DB immediately usable.
    pub(crate) fn activate_project_db(&self, db_path: PathBuf) -> Result<bool, String> {
        self.db.activate_project_db(db_path)
    }

    pub(crate) fn with_path_store<T>(
        &self,
        db_path: &Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_path_store(db_path, f)
    }

    pub(crate) fn with_path_store_read<T>(
        &self,
        db_path: &Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_path_store_read(db_path, f)
    }

    pub(crate) fn with_global_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_global_store(f)
    }

    pub(crate) fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_global_store_read(f)
    }

    pub(crate) fn with_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_project_store(f)
    }

    pub(crate) fn with_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_project_store_read(f)
    }

    pub(crate) fn with_store_for_scope<T>(
        &self,
        scope: DbScope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_store_for_scope(scope, f)
    }

    pub(crate) fn with_store_for_scope_read<T>(
        &self,
        scope: DbScope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_store_for_scope_read(scope, f)
    }

    /// Addressing convention for named-project recall:
    ///   * `<repo>/.tachi/memory.db` is the per-repo source of truth (real data).
    ///   * `~/.tachi/global/memory.db` is the machine-global store.
    ///   * `~/.tachi/projects/<name>/` is an addressing alias, not a data store.
    ///
    /// Resolution prefers the manifest-recorded repo-local path for a project so
    /// it does NOT depend on the Plan C symlink existing (which is Unix-only and
    /// can be stale). It falls back to the `~/.tachi/projects/<name>/` alias for
    /// backward compatibility (e.g. named stores like `wiki` that have no repo).
    pub(crate) fn resolve_named_project_db_path(project_name: &str) -> Result<PathBuf, String> {
        let safe_name = Self::validate_named_project(project_name)?;

        // Prefer the manifest-recorded repo-local DB for this project name. This
        // makes repo-local addressing primary and removes the hard dependency on
        // the Plan C symlink (which does not exist on non-Unix hosts).
        if let Some(repo_local) = Self::manifest_repo_local_db_for_project(&safe_name) {
            if repo_local.exists() {
                return Ok(repo_local);
            }
        }

        let db_path = crate::path_utils::plan_c_global_db_path(&safe_name);
        if !db_path.exists() {
            if db_path.is_symlink() {
                let target_str = match std::fs::read_link(&db_path) {
                    Ok(target) => target.display().to_string(),
                    Err(_) => "<unknown>".to_string(),
                };
                Err(format!(
                    "Project '{}' database symlink is broken: {} -> (target missing: {})",
                    project_name,
                    db_path.display(),
                    target_str
                ))
            } else {
                Err(format!(
                    "Project '{}' not found (expected DB at {})",
                    project_name,
                    db_path.display()
                ))
            }
        } else {
            Ok(db_path)
        }
    }

    /// Resolve a project name to the canonical identity of an existing DB
    /// without opening, creating, repairing, or registering anything.
    ///
    /// This is deliberately stricter than ordinary named-project routing: a
    /// malformed/unreadable manifest, a matching missing DB, or a legacy alias
    /// that matches multiple physical repo-local DBs is an error. Callers use
    /// this at write-isolation gates, where uncertainty must fail closed.
    pub(crate) fn resolve_named_project_db_identity(project_name: &str) -> Result<PathBuf, String> {
        let safe_name = Self::validate_named_project(project_name)?;
        let manifest_matches = Self::strict_manifest_repo_local_dbs_for_project(&safe_name)?;

        if !manifest_matches.is_empty() {
            let mut identities = Vec::with_capacity(manifest_matches.len());
            for db_path in manifest_matches {
                let canonical = std::fs::canonicalize(&db_path).map_err(|err| {
                    format!(
                        "Project '{project_name}' manifest DB cannot be canonicalized at {}: {err}",
                        db_path.display()
                    )
                })?;
                Self::require_regular_project_db(project_name, &canonical)?;
                identities.push(canonical);
            }
            identities.sort();
            identities.dedup();
            return match identities.as_slice() {
                [identity] => Ok(identity.clone()),
                _ => Err(format!(
                    "Project '{project_name}' alias is ambiguous across {} repo-local databases",
                    identities.len()
                )),
            };
        }

        let db_path = crate::path_utils::plan_c_global_db_path(&safe_name);
        if !db_path.exists() {
            if db_path.is_symlink() {
                let target_str = match std::fs::read_link(&db_path) {
                    Ok(target) => target.display().to_string(),
                    Err(_) => "<unknown>".to_string(),
                };
                return Err(format!(
                    "Project '{}' database symlink is broken: {} -> (target missing: {})",
                    project_name,
                    db_path.display(),
                    target_str
                ));
            }
            return Err(format!(
                "Project '{}' not found (expected DB at {})",
                project_name,
                db_path.display()
            ));
        }

        let canonical = std::fs::canonicalize(&db_path).map_err(|err| {
            format!(
                "Project '{project_name}' database cannot be canonicalized at {}: {err}",
                db_path.display()
            )
        })?;
        Self::require_regular_project_db(project_name, &canonical)?;
        Ok(canonical)
    }

    fn validate_named_project(project_name: &str) -> Result<String, String> {
        // Guard: reject names that could escape the projects/ directory.
        if project_name.is_empty()
            || project_name.contains('/')
            || project_name.contains('\\')
            || project_name.contains("..")
            || project_name.starts_with('.')
        {
            return Err(format!("Invalid project name '{project_name}'"));
        }
        Ok(crate::utils::sanitize_safe_path_name(project_name))
    }

    fn require_regular_project_db(project_name: &str, db_path: &Path) -> Result<(), String> {
        let metadata = std::fs::metadata(db_path).map_err(|err| {
            format!(
                "Project '{project_name}' database metadata failed at {}: {err}",
                db_path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "Project '{project_name}' resolved to a non-file database path: {}",
                db_path.display()
            ));
        }
        Ok(())
    }

    fn strict_manifest_repo_local_dbs_for_project(safe_name: &str) -> Result<Vec<PathBuf>, String> {
        let manifest_path = crate::path_utils::tachi_home().join("manifest.json");
        let manifest = match crate::manifest::Manifest::load(&manifest_path) {
            Ok(manifest) => manifest,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(format!(
                    "Project manifest cannot be read at {}: {err}",
                    manifest_path.display()
                ));
            }
        };

        Ok(manifest
            .dbs
            .iter()
            .filter_map(|entry| {
                let db_path = Path::new(&entry.path);
                let project_root = crate::path_utils::plan_c_project_root_from_local_db(db_path)?;
                let matches = crate::path_utils::plan_c_dir_name_from_root(&project_root)
                    .as_deref()
                    == Some(safe_name)
                    || crate::path_utils::plan_c_legacy_dir_name_from_root(&project_root)
                        .as_deref()
                        == Some(safe_name);
                matches.then(|| db_path.to_path_buf())
            })
            .collect())
    }

    /// Resolve a (sanitized) project name to a repo-local `<repo>/.tachi/memory.db`
    /// recorded in the manifest, if any. This is the symlink-independent primary
    /// addressing path: for each manifest entry shaped like `<repo>/.tachi/memory.db`,
    /// we recompute the repo's alias dir name (hashed, or its legacy un-hashed
    /// form for backward compatibility) and match it against `safe_name`.
    ///
    /// Returns the first matching repo-local path. Returns `None` when the
    /// manifest is missing/unreadable or no entry matches — callers then fall
    /// back to the `~/.tachi/projects/<name>/` alias.
    fn manifest_repo_local_db_for_project(safe_name: &str) -> Option<PathBuf> {
        // The runtime manifest lives at `<tachi_home>/manifest.json` (see
        // `bootstrap/serve.rs`, which uses `app_home == tachi_home()`).
        let manifest_path = crate::path_utils::tachi_home().join("manifest.json");
        let manifest = crate::manifest::Manifest::load(&manifest_path).ok()?;
        for entry in &manifest.dbs {
            let db_path = std::path::Path::new(&entry.path);
            // Only consider repo-local `<repo>/.tachi/memory.db` shapes.
            let Some(project_root) = crate::path_utils::plan_c_project_root_from_local_db(db_path)
            else {
                continue;
            };
            let matches = crate::path_utils::plan_c_dir_name_from_root(&project_root).as_deref()
                == Some(safe_name)
                || crate::path_utils::plan_c_legacy_dir_name_from_root(&project_root).as_deref()
                    == Some(safe_name);
            if matches {
                return Some(db_path.to_path_buf());
            }
        }
        None
    }

    /// Open a named project's DB for a read-only operation.
    pub(crate) fn with_named_project_store_read<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = Self::resolve_named_project_db_path(project_name)?;
        self.db.with_path_store_read_with_label(
            &db_path,
            &format!("named-project:{project_name}"),
            f,
        )
    }

    /// Open a named project's DB for a write operation.
    pub(crate) fn with_named_project_store<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = Self::resolve_named_project_db_path(project_name)?;
        self.db
            .with_path_store_with_label(&db_path, project_name, f)
    }

    pub(crate) fn resolve_write_scope(&self, requested: &str) -> (DbScope, Option<String>) {
        if requested == "global" {
            (DbScope::Global, None)
        } else if self.has_project_db() {
            (DbScope::Project, None)
        } else {
            (
                DbScope::Global,
                Some("No project DB available; saved to global".to_string()),
            )
        }
    }
}

#[cfg(test)]
mod resolve_named_project_tests {
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
    fn resolve_named_project_uses_workspace_data_tachi_home() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tmp");
            let saved_cwd = std::env::current_dir().expect("cwd");
            let saved_home = std::env::var_os("TACHI_HOME");
            let saved_sigil = std::env::var_os("SIGIL_HOME");
            let saved_app = std::env::var_os("TACHI_APP_HOME");
            std::env::remove_var("TACHI_HOME");
            std::env::remove_var("SIGIL_HOME");
            std::env::remove_var("TACHI_APP_HOME");

            let repo = tmp.path().join("Quant_Analyzer_2026");
            let nested = repo.join("engine/v8");
            let named_db = repo.join("data/tachi/projects/hyperion/memory.db");
            std::fs::create_dir_all(&nested).expect("nested");
            std::fs::create_dir_all(named_db.parent().unwrap()).expect("named parent");
            std::fs::write(&named_db, b"").expect("named db placeholder");

            std::env::set_current_dir(&nested).expect("set cwd");
            let named_db = std::fs::canonicalize(named_db).expect("canonical named db");
            let resolved =
                MemoryServer::resolve_named_project_db_path("hyperion").expect("resolve");
            assert_eq!(resolved, named_db);

            std::env::set_current_dir(saved_cwd).expect("restore cwd");
            restore_env("TACHI_HOME", saved_home);
            restore_env("SIGIL_HOME", saved_sigil);
            restore_env("TACHI_APP_HOME", saved_app);
        });
    }

    /// Change #2: named-project resolution prefers the manifest-recorded
    /// repo-local DB and does NOT depend on the Plan C symlink existing.
    #[test]
    fn resolve_prefers_manifest_repo_local_db_without_symlink() {
        with_env_lock(|| {
            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let saved = std::env::var_os("TACHI_HOME");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            std::env::set_var("TACHI_HOME", &tachi_home);

            // Repo-local source of truth — NO Plan C alias/symlink created.
            let repo = tmp.path().join("My Service");
            let local_db = repo.join(".tachi/memory.db");
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
            std::fs::write(&local_db, b"").expect("local db");
            crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);

            // Record the repo-local DB in the manifest at <tachi_home>/manifest.json.
            let entry = serde_json::json!({
                "path": local_db.to_string_lossy(),
                "role": "project",
                "owner": "project:My_Service",
                "schema_kind": "tachi",
                "vec_enabled": false,
                "allow_write": true,
                "last_doctor_at": "1970-01-01T00:00:00Z",
                "last_classification": "healthy",
                "scope_hint": "tachi-other"
            });
            let manifest = serde_json::json!({
                "schema_version": 1,
                "generated_at": "1970-01-01T00:00:00Z",
                "comment": "test",
                "dbs": [entry]
            });
            std::fs::write(
                tachi_home.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .expect("write manifest");

            // The hashed alias name for this repo resolves to the repo-local DB
            // even though no ~/.tachi/projects/<name>/ alias exists on disk.
            let name = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("name");
            let resolved = MemoryServer::resolve_named_project_db_path(&name).expect("resolve");
            assert_eq!(resolved, local_db);

            restore_env("TACHI_HOME", saved);
        });
    }
}
