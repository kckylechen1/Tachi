use crate::server_state::{
    configured_memory_read_pool_size, DbScope, MemoryServer, ProjectDbState, ReadStorePool,
};
use crate::utils::{lock_or_recover, read_or_recover, write_or_recover};
use memory_core::MemoryStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::RwLock as StdRwLock;

impl MemoryServer {
    /// Check if a project DB is available (static startup or hot-swapped).
    pub(crate) fn has_project_db(&self) -> bool {
        if self.project_db_path.is_some() {
            return true;
        }
        // Check hot-swapped project DB
        self.hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Hot-activate a project database on a running server.
    /// Called by `tachi_init_project_db` to make the created DB immediately usable.
    pub(crate) fn activate_project_db(&self, db_path: PathBuf) -> Result<bool, String> {
        let db_str = db_path.to_str().ok_or_else(|| {
            format!(
                "Project DB path contains invalid UTF-8: {}",
                db_path.display()
            )
        })?;
        let project_label = db_path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|os| os.to_str())
            .unwrap_or("project")
            .to_string();
        let store = MemoryStore::open_with_label(db_str, &project_label)
            .map_err(|e| format!("open project db: {e}"))?;
        let read_pool = ReadStorePool::open_read_only(db_str, configured_memory_read_pool_size())
            .map_err(|e| format!("open project read db: {e}"))?;
        let state = ProjectDbState {
            store: Arc::new(StdMutex::new(store)),
            read_pool,
            rw_gate: Arc::new(StdRwLock::new(())),
            db_path: Arc::new(db_path),
        };

        let mut guard = self
            .hot_project_db
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let was_none = guard.is_none();
        *guard = Some(state);
        Ok(was_none)
    }

    /// Run a write closure against the hot-swapped project DB.
    pub(crate) fn with_hot_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| "No hot-swapped project database available".to_string())?;
        let _gate = write_or_recover(&state.rw_gate, "hot_project_rw_gate");
        let mut store = lock_or_recover(&state.store, "hot_project_store");
        f(&mut store)
    }

    /// Run a read closure against the hot-swapped project DB.
    pub(crate) fn with_hot_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| "No hot-swapped project database available".to_string())?;
        let _gate = read_or_recover(&state.rw_gate, "hot_project_rw_gate");
        state.read_pool.with_store("hot_project_read_pool", f)
    }

    fn open_read_store(db_path: &PathBuf, label: &str) -> Result<MemoryStore, String> {
        let db_str = db_path.to_str().ok_or_else(|| {
            format!(
                "{} DB path contains invalid UTF-8: {}",
                label,
                db_path.display()
            )
        })?;
        MemoryStore::open_read_only(db_str).map_err(|e| format!("open {label} read store: {e}"))
    }

    pub(crate) fn with_path_store<T>(
        &self,
        db_path: &PathBuf,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_str = db_path
            .to_str()
            .ok_or_else(|| format!("DB path contains invalid UTF-8: {}", db_path.display()))?;
        let label = db_path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|os| os.to_str())
            .unwrap_or("path");
        let _gate = write_or_recover(&self.global_rw_gate, "path_db_rw_gate");
        let mut store = MemoryStore::open_with_label(db_str, label)
            .map_err(|e| format!("open path store {}: {e}", db_path.display()))?;
        f(&mut store)
    }

    pub(crate) fn with_path_store_read<T>(
        &self,
        db_path: &PathBuf,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = read_or_recover(&self.global_rw_gate, "path_db_rw_gate");
        let mut store = Self::open_read_store(db_path, "path")?;
        f(&mut store)
    }

    pub(crate) fn with_global_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = write_or_recover(&self.global_rw_gate, "global_rw_gate");
        let mut store = lock_or_recover(&self.global_store, "global_store");
        f(&mut store)
    }

    pub(crate) fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = read_or_recover(&self.global_rw_gate, "global_rw_gate");
        self.global_read_pool.with_store("global_read_pool", f)
    }

    pub(crate) fn with_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        // Hot-activated DB (e.g. tachi_init_project_db) overrides the boot-time store.
        if self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return self.with_hot_project_store(f);
        }
        if let Some(ref store_arc) = self.project_store {
            let gate = self
                .project_rw_gate
                .as_ref()
                .ok_or_else(|| "No project lock available".to_string())?;
            let _gate = write_or_recover(gate, "project_rw_gate");
            let mut store = lock_or_recover(store_arc, "project_store");
            return f(&mut store);
        }
        Err("No project database available".to_string())
    }

    pub(crate) fn with_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        if self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return self.with_hot_project_store_read(f);
        }
        if let Some(ref read_pool) = self.project_read_pool {
            let gate = self
                .project_rw_gate
                .as_ref()
                .ok_or_else(|| "No project lock available".to_string())?;
            let _gate = read_or_recover(gate, "project_rw_gate");
            return read_pool.with_store("project_read_pool", f);
        }
        Err("No project database available".to_string())
    }

    pub(crate) fn with_store_for_scope<T>(
        &self,
        scope: DbScope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        match scope {
            DbScope::Global => self.with_global_store(f),
            DbScope::Project => self.with_project_store(f),
        }
    }

    pub(crate) fn with_store_for_scope_read<T>(
        &self,
        scope: DbScope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        match scope {
            DbScope::Global => self.with_global_store_read(f),
            DbScope::Project => self.with_project_store_read(f),
        }
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
        // Guard: reject names that could escape the projects/ directory.
        if project_name.is_empty()
            || project_name.contains('/')
            || project_name.contains('\\')
            || project_name.contains("..")
            || project_name.starts_with('.')
        {
            return Err(format!("Invalid project name '{project_name}'"));
        }
        let safe_name = crate::utils::sanitize_safe_path_name(project_name);

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
        // Use global rw_gate for reader concurrency protection (prevents schema swap while reading)
        let _gate = read_or_recover(&self.global_rw_gate, "named_project_rw_gate");
        let mut store =
            Self::open_read_store(&db_path, &format!("named-project:{}", project_name))?;
        f(&mut store)
    }

    /// Open a named project's DB for a write operation.
    pub(crate) fn with_named_project_store<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = Self::resolve_named_project_db_path(project_name)?;
        let db_str = db_path.to_str().ok_or_else(|| {
            format!(
                "Project DB path contains invalid UTF-8: {}",
                db_path.display()
            )
        })?;

        let _gate = write_or_recover(&self.global_rw_gate, "named_project_rw_gate");

        let store_arc = {
            let mut cache = self
                .named_project_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = cache.get(project_name) {
                existing.clone()
            } else {
                let store = MemoryStore::open_with_label(db_str, project_name)
                    .map_err(|e| format!("open named project store: {e}"))?;
                let arc = Arc::new(StdMutex::new(store));
                cache.insert(project_name.to_string(), arc.clone());
                arc
            }
        };

        let mut store = store_arc.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut store)
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
            let tmp = tempfile::tempdir().expect("tmp");
            let saved = std::env::var_os("TACHI_HOME");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            std::env::set_var("TACHI_HOME", &tachi_home);

            // Repo-local source of truth — NO Plan C alias/symlink created.
            let repo = tmp.path().join("My Service");
            let local_db = repo.join(".tachi/memory.db");
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
            std::fs::write(&local_db, b"").expect("local db");

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
