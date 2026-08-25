use crate::server_state::{DbScope, MemoryServer};
use memcore::MemoryStore;
use std::path::{Path, PathBuf};

fn run_identity_checked_store_action<T>(
    store: &mut MemoryStore,
    db_path: &Path,
    store_label: &str,
    operation: &str,
    action: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    let verify = |store: &MemoryStore, phase: &str| {
        store
            .verify_opened_physical_db_identity(db_path)
            .map_err(|error| {
                format!("{store_label} physical identity check failed {phase} {operation}: {error}")
            })
    };
    verify(store, "before")?;
    let result = action(store);
    let post = verify(store, "after");
    match (result, post) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(identity_error)) => Err(format!(
            "{error}; additionally, the physical identity invariant failed after the operation: {identity_error}"
        )),
    }
}

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

    pub(crate) fn with_global_store_identity_checked<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = self.global_db_path_buf();
        self.db.with_global_store(|store| {
            run_identity_checked_store_action(store, &db_path, "global", "write", f)
        })
    }

    pub(crate) fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_global_store_read(f)
    }

    pub(crate) fn with_global_store_read_identity_checked<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = self.global_db_path_buf();
        self.db.with_global_store_read(|store| {
            run_identity_checked_store_action(store, &db_path, "global", "read", f)
        })
    }

    /// Recording twin of [`Self::with_global_store_read`]: identical
    /// read-gate + pool-checkout semantics, additionally returning the
    /// `ReadPoolCheckoutReceipt` so a sampled caller can carry a MEASURED
    /// `pool_checkout_wait` instead of leaving it `LayerAvailability::Unavailable`
    /// (kckylechen1/tachi#1125). Thin passthrough to
    /// `DbRuntime::with_global_store_read_recording` — no new pooling/locking
    /// behavior lives here. First consumer: tachi#1145's auto-link
    /// pool-checkout-wait separation (`memory_search_ops/auto_link.rs`).
    pub(crate) fn with_global_store_read_recording<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<(T, memory_server_runtime::ReadPoolCheckoutReceipt), String> {
        self.db.with_global_store_read_recording(f)
    }

    pub(crate) fn with_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_project_store(f)
    }

    pub(crate) fn with_project_store_identity_checked<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = self
            .project_db_path_buf()
            .ok_or_else(|| "project database is unavailable".to_string())?;
        self.db.with_project_store(|store| {
            run_identity_checked_store_action(store, &db_path, "project", "write", f)
        })
    }

    pub(crate) fn with_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.db.with_project_store_read(f)
    }

    pub(crate) fn with_project_store_read_identity_checked<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = self
            .project_db_path_buf()
            .ok_or_else(|| "project database is unavailable".to_string())?;
        self.db.with_project_store_read(|store| {
            run_identity_checked_store_action(store, &db_path, "project", "read", f)
        })
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
    ///   * `<repo>/.tachi/tachi-memory.db` is the per-repo source of truth (real data).
    ///   * `~/.tachi/global/tachi-memory.db` is the machine-global store.
    ///   * `~/.tachi/projects/<name>/` is an addressing alias, not a data store.
    ///
    /// Resolution prefers the manifest-recorded repo-local path for a project so
    /// it does NOT depend on the Plan C symlink existing (which is Unix-only and
    /// can be stale). It falls back to the `~/.tachi/projects/<name>/` alias for
    /// backward compatibility (e.g. named stores like `wiki` that have no repo).
    pub(crate) fn resolve_named_project_db_path(project_name: &str) -> Result<PathBuf, String> {
        Self::resolve_named_project_db_path_in_home(project_name, &crate::path_utils::tachi_home())
    }

    pub(crate) fn resolve_server_named_project_db_path(
        &self,
        project_name: &str,
    ) -> Result<PathBuf, String> {
        Self::resolve_named_project_db_path_in_home(project_name, self.home_dir.as_path())
    }

    pub(crate) fn resolve_named_project_db_path_in_home(
        project_name: &str,
        tachi_home: &Path,
    ) -> Result<PathBuf, String> {
        if let Some(path) =
            Self::resolve_existing_named_project_db_path_in_home(project_name, tachi_home)?
        {
            return Ok(path);
        }
        let safe_name = Self::validate_named_project(project_name)?;
        let db_path =
            crate::path_utils::plan_c_global_db_path_existing_in_home(tachi_home, &safe_name);
        Err(format!(
            "Project '{}' not found (expected DB at {})",
            project_name,
            db_path.display()
        ))
    }

    /// Three-state named-project lookup: `Some` is one unambiguous physical
    /// DB, `None` is a genuine absence, and `Err` is incomplete or conflicting
    /// identity evidence. Registration may continue only from `None`.
    pub(crate) fn resolve_existing_named_project_db_path_in_home(
        project_name: &str,
        tachi_home: &Path,
    ) -> Result<Option<PathBuf>, String> {
        let safe_name = Self::validate_named_project(project_name)?;

        // Prefer the manifest-recorded repo-local DB for this project name. This
        // makes repo-local addressing primary and removes the hard dependency on
        // the Plan C symlink (which does not exist on non-Unix hosts).
        let repo_local = Self::manifest_repo_local_db_for_project(&safe_name, tachi_home)?;
        if let Some(repo_local) = repo_local {
            if repo_local.exists() {
                Self::reconcile_named_project_candidate(
                    project_name,
                    &repo_local,
                    true,
                    tachi_home,
                )?;
                return Ok(Some(repo_local));
            }
        }

        let alias =
            crate::path_utils::plan_c_existing_named_alias_db_in_home(tachi_home, &safe_name)?;
        if let Some(alias) = alias {
            Self::reconcile_named_project_candidate(project_name, &alias, false, tachi_home)?;
            return Ok(Some(alias));
        }
        Ok(None)
    }

    fn reconcile_named_project_candidate(
        project_name: &str,
        db_path: &Path,
        manifest_owned: bool,
        tachi_home: &Path,
    ) -> Result<(), String> {
        let canonical_db = std::fs::canonicalize(db_path).map_err(|err| {
            format!(
                "Project '{project_name}' database cannot be canonicalized at {}: {err}",
                db_path.display()
            )
        })?;
        let project_root = crate::path_utils::plan_c_project_root_from_local_db(&canonical_db)
            .or_else(|| {
                let root = crate::utils::find_git_root_from(&canonical_db)?;
                if manifest_owned {
                    return Some(root);
                }
                let is_named_symlink =
                    crate::path_utils::named_project_from_path_in_home(db_path, tachi_home)
                        .is_some()
                        && std::fs::symlink_metadata(db_path)
                            .map(|metadata| metadata.file_type().is_symlink())
                            .unwrap_or(false);
                let is_root_alias = [
                    crate::path_utils::plan_c_dir_name_from_root(&root),
                    crate::path_utils::plan_c_previous_dir_name_from_root(&root),
                    crate::path_utils::plan_c_previous_raw_dir_name_from_root(&root),
                    crate::path_utils::plan_c_legacy_dir_name_from_root(&root),
                ]
                .into_iter()
                .flatten()
                .any(|name| name == project_name);
                (is_named_symlink && is_root_alias).then_some(root)
            });
        let Some(project_root) = project_root else {
            return Ok(());
        };
        if let Some(compatible_alias) =
            crate::path_utils::plan_c_existing_alias_db_for_root_in_home(&project_root, tachi_home)?
        {
            let alias_identity = std::fs::canonicalize(&compatible_alias).map_err(|err| {
                format!(
                    "Project '{project_name}' compatibility alias cannot be canonicalized at {}: {err}",
                    compatible_alias.display()
                )
            })?;
            if canonical_db != alias_identity {
                return Err(format!(
                    "Project '{project_name}' identity is ambiguous between DB {} and compatibility alias {}",
                    db_path.display(),
                    compatible_alias.display()
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn resolve_server_named_project_binding(
        &self,
        project_name: &str,
    ) -> Result<(String, PathBuf), String> {
        Self::resolve_named_project_binding_in_home(project_name, self.home_dir.as_path())
    }

    fn resolve_named_project_binding_in_home(
        project_name: &str,
        tachi_home: &Path,
    ) -> Result<(String, PathBuf), String> {
        let db_path = Self::resolve_named_project_db_path_in_home(project_name, tachi_home)?;
        let canonical_db = std::fs::canonicalize(&db_path).map_err(|err| {
            format!(
                "Project '{project_name}' database cannot be canonicalized at {}: {err}",
                db_path.display()
            )
        })?;
        let project_root = crate::path_utils::plan_c_project_root_from_local_db(&canonical_db)
            .or_else(|| {
                // `TACHI_HOME/projects/<name>` is an explicit standalone
                // named-store topology even when TACHI_HOME itself lives
                // inside a Git repository (for example Hyperion). A symlink
                // in that topology is treated as a repo compatibility alias
                // only when its name is one of the target root's known
                // identities; arbitrary named stores must not be absorbed by
                // an enclosing repository.
                let root = crate::utils::find_git_root_from(&canonical_db)?;
                let is_named_store =
                    crate::path_utils::named_project_from_path_in_home(&db_path, tachi_home)
                        .is_some();
                let is_symlink = std::fs::symlink_metadata(&db_path)
                    .map(|metadata| metadata.file_type().is_symlink())
                    .unwrap_or(false);
                let is_root_alias = [
                    crate::path_utils::plan_c_dir_name_from_root(&root),
                    crate::path_utils::plan_c_previous_dir_name_from_root(&root),
                    crate::path_utils::plan_c_previous_raw_dir_name_from_root(&root),
                    crate::path_utils::plan_c_legacy_dir_name_from_root(&root),
                ]
                .into_iter()
                .flatten()
                .any(|name| name == project_name);
                (!is_named_store || (is_symlink && is_root_alias)).then_some(root)
            });
        let canonical_name = if let Some(root) = project_root {
            let compatible_alias =
                crate::path_utils::plan_c_alias_db_for_root_in_home(&root, tachi_home)?;
            if std::fs::symlink_metadata(&compatible_alias).is_ok() {
                let alias_identity = std::fs::canonicalize(&compatible_alias).map_err(|err| {
                    format!(
                        "project compatibility alias cannot be canonicalized at {}: {err}",
                        compatible_alias.display()
                    )
                })?;
                if alias_identity != canonical_db {
                    return Err(format!(
                        "project compatibility aliases are ambiguous: {} does not resolve to {}",
                        compatible_alias.display(),
                        canonical_db.display()
                    ));
                }
            }
            let canonical_name = crate::path_utils::plan_c_dir_name_from_root(&root)
                .ok_or_else(|| "repo-local project DB has no stable root identity".to_string())?;
            // #1061 immutable binding: a caller that bound via the current gen-4
            // canonical name, or via the human-meaningful gen-1 legacy
            // bare-basename alias, keeps that exact identity as its session
            // label. Both are first-class stable identities for this DB — the
            // legacy name resolves durably through the manifest's derived-name
            // match, independent of any Plan C symlink — so re-normalizing them
            // would silently rename a stable caller binding. Only the deprecated
            // machine-generated hash schemes (gen-3 case-folded / gen-2
            // raw-canonical FNV-8) are transient compatibility aliases that
            // migrate to the gen-4 canonical and register it so the DB stays
            // reachable once the old symlink is gone. This branch changes only
            // the session LABEL; `db_path` is unchanged and the divergent-alias
            // collision guard above still holds, so no isolation guarantee is
            // relaxed.
            // Shared rule (`path_utils::alias::is_stable_caller_identity`), so
            // the stability *criterion* here and the one the CLI display path
            // applies are one function. Note what that does and does not buy:
            // the root derivations are NOT shared — this one has the `.or_else`
            // Git-root fallback above, `canonical_identity_for_display` does
            // not. Where only this side derives a root, display falls back to
            // showing the wire alias rather than a second opinion; see that
            // function's KNOWN NARROW FACE note.
            if crate::path_utils::is_stable_caller_identity(project_name, &root, &canonical_name) {
                project_name.to_string()
            } else {
                match Self::resolve_existing_named_project_db_path_in_home(
                    &canonical_name,
                    tachi_home,
                )? {
                    Some(migrated) => {
                        let migrated = std::fs::canonicalize(&migrated).map_err(|err| {
                            format!(
                                "project identity '{canonical_name}' cannot be canonicalized at {}: {err}",
                                migrated.display()
                            )
                        })?;
                        if migrated != canonical_db {
                            return Err(format!(
                                "project identity '{canonical_name}' already resolves to unrelated DB {}",
                                migrated.display()
                            ));
                        }
                        crate::path_utils::require_plan_c_alias_success(
                            crate::path_utils::ensure_plan_c_canonical_alias_in_home(
                                &canonical_db,
                                &root,
                                tachi_home,
                            ),
                        )?;
                    }
                    None => {
                        crate::project_db_ops::register_repo_local_manifest_entry_in_home(
                            &canonical_db,
                            &canonical_name,
                            tachi_home,
                        )?;
                        crate::path_utils::require_plan_c_alias_success(
                            crate::path_utils::ensure_plan_c_canonical_alias_in_home(
                                &canonical_db,
                                &root,
                                tachi_home,
                            ),
                        )?;
                    }
                }
                canonical_name
            }
        } else {
            project_name.to_string()
        };
        Ok((canonical_name, db_path))
    }

    /// Resolve a project name to the canonical identity of an existing DB
    /// without opening, creating, repairing, or registering anything.
    ///
    /// This is deliberately stricter than ordinary named-project routing: a
    /// malformed/unreadable manifest, a matching missing DB, or a legacy alias
    /// that matches multiple physical repo-local DBs is an error. Callers use
    /// this at write-isolation gates, where uncertainty must fail closed.
    pub(crate) fn resolve_named_project_db_identity(project_name: &str) -> Result<PathBuf, String> {
        Self::resolve_named_project_db_identity_in_home(
            project_name,
            &crate::path_utils::tachi_home(),
        )
    }

    pub(crate) fn resolve_server_named_project_db_identity(
        &self,
        project_name: &str,
    ) -> Result<PathBuf, String> {
        Self::resolve_named_project_db_identity_in_home(project_name, self.home_dir.as_path())
    }

    fn resolve_named_project_db_identity_in_home(
        project_name: &str,
        tachi_home: &Path,
    ) -> Result<PathBuf, String> {
        let db_path =
            Self::resolve_existing_named_project_db_path_in_home(project_name, tachi_home)?
                .ok_or_else(|| {
                    let expected =
                        crate::path_utils::plan_c_global_db_path_in_home(tachi_home, project_name);
                    format!(
                        "Project '{}' not found (expected DB at {})",
                        project_name,
                        expected.display()
                    )
                })?;
        let canonical = std::fs::canonicalize(&db_path).map_err(|err| {
            format!(
                "Project '{project_name}' database cannot be canonicalized at {}: {err}",
                db_path.display()
            )
        })?;
        Self::require_regular_project_db(project_name, &canonical)?;
        Ok(canonical)
    }

    pub(crate) fn validate_named_project(project_name: &str) -> Result<String, String> {
        // Project names are persisted identities, not display strings. Accept
        // only an exact already-safe alias; never lossy-sanitize caller input
        // into another project's directory. `unnamed` is permanently
        // ambiguous with the historical sanitizer fallback and cannot be
        // auto-assigned safely.
        if !crate::path_utils::is_canonical_project_identity(project_name) {
            return Err(format!(
                "Invalid project identity '{project_name}': use the exact registered ASCII alias; ambiguous or lossy-normalized names are refused"
            ));
        }
        Ok(project_name.to_string())
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

    /// Resolve an exact registered project identity to its manifest-recorded
    /// DB, including contained custom paths. Canonical `project:<identity>`
    /// scope hints are authoritative; legacy `.tachi/<db-filename>` entries
    /// without that hint remain discoverable by recomputing their current and
    /// compatibility aliases from the repo root.
    ///
    /// Returns the single matching repo-local path. Multiple physical matches
    /// are an ambiguous legacy identity and fail closed rather than selecting
    /// whichever manifest entry happens to come first. A missing manifest or
    /// no match falls back to the named alias; an unreadable manifest fails
    /// closed because ownership evidence is incomplete.
    fn manifest_repo_local_db_for_project(
        safe_name: &str,
        tachi_home: &Path,
    ) -> Result<Option<PathBuf>, String> {
        // The runtime manifest lives at `<tachi_home>/manifest.json` (see
        // `bootstrap/serve.rs`, which uses `app_home == tachi_home()`).
        let manifest_path = tachi_home.join("manifest.json");
        let manifest = match crate::manifest::Manifest::load(&manifest_path) {
            Ok(manifest) => manifest,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(format!(
                    "Project manifest cannot be read at {}: {err}",
                    manifest_path.display()
                ));
            }
        };
        let mut matches = Vec::new();
        for entry in &manifest.dbs {
            let db_path = std::path::Path::new(&entry.path);
            let scope_match = entry.scope_hint.strip_prefix("project:") == Some(safe_name);
            let derived_match = crate::path_utils::plan_c_project_root_from_local_db(db_path)
                .is_some_and(|project_root| {
                    crate::path_utils::plan_c_dir_name_from_root(&project_root).as_deref()
                        == Some(safe_name)
                        || crate::path_utils::plan_c_previous_dir_name_from_root(&project_root)
                            .as_deref()
                            == Some(safe_name)
                        || crate::path_utils::plan_c_previous_raw_dir_name_from_root(&project_root)
                            .as_deref()
                            == Some(safe_name)
                        || crate::path_utils::plan_c_legacy_dir_name_from_root(&project_root)
                            .as_deref()
                            == Some(safe_name)
                });
            if derived_match || scope_match {
                crate::path_utils::manifest_db_leaf_exists(entry)?;
                let identity = std::fs::canonicalize(db_path).map_err(|err| {
                    format!(
                        "Project '{safe_name}' manifest entry is missing or unreadable at {}: {err}",
                        db_path.display()
                    )
                })?;
                matches.push(identity);
            }
        }
        matches.sort();
        matches.dedup();
        match matches.as_slice() {
            [] => Ok(None),
            [db_path] => Ok(Some(db_path.clone())),
            _ => Err(format!(
                "Project '{safe_name}' legacy identity is ambiguous across {} repo-local databases",
                matches.len()
            )),
        }
    }

    /// Open a named project's DB for a read-only operation.
    ///
    /// The label is the bare project name, exactly what
    /// [`Self::with_named_project_store`] passes on the write side.
    /// tachi#1569: it used to be `named-project:<name>` here and `<name>`
    /// there, so the two doors onto one store disagreed about its identity —
    /// and since the label is now the store's `db_label`, that disagreement
    /// would make `is_wiki_corpus_store()` answer differently for a read than
    /// for a write of the same file.
    pub(crate) fn with_named_project_store_read<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = self.resolve_named_project_db_open_path(project_name)?;
        self.db
            .with_path_store_read_with_label(&db_path, project_name, f)
    }

    /// Read a named project through its cached runtime handle while proving
    /// that the checked-out handle still addresses the current path both
    /// before and after the operation.
    pub(crate) fn with_named_project_store_read_identity_checked<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = self.resolve_named_project_db_open_path(project_name)?;
        // Bare project name, matching the write side — see
        // `with_named_project_store_read` (tachi#1569).
        self.db
            .with_path_store_read_with_label(&db_path, project_name, |store| {
                run_identity_checked_store_action(
                    store,
                    &db_path,
                    &format!("named project '{project_name}'"),
                    "read",
                    f,
                )
            })
    }

    /// Open a named project's DB for a write operation.
    pub(crate) fn with_named_project_store<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = self.resolve_named_project_db_open_path(project_name)?;
        self.db
            .with_path_store_with_label(&db_path, project_name, f)
    }

    /// Write a named project without allowing a path-keyed cached connection
    /// to report success after its database file has been replaced.
    pub(crate) fn with_named_project_store_identity_checked<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = self.resolve_named_project_db_open_path(project_name)?;
        self.db
            .with_path_store_with_label(&db_path, project_name, |store| {
                run_identity_checked_store_action(
                    store,
                    &db_path,
                    &format!("named project '{project_name}'"),
                    "write",
                    f,
                )
            })
    }

    /// Establish the write-capable named-project state before a write facade
    /// performs any read-before-write lookup. This is the only route that may
    /// consume the runtime's exact v22-to-v23 guard migration authority.
    pub(crate) fn prepare_named_project_store_for_write(
        &self,
        project_name: &str,
    ) -> Result<(), String> {
        self.with_named_project_store_identity_checked(project_name, |_| Ok(()))
    }

    fn resolve_named_project_db_open_path(&self, project_name: &str) -> Result<PathBuf, String> {
        let addressed_path = self.resolve_server_named_project_db_path(project_name)?;
        match std::fs::symlink_metadata(&addressed_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                std::fs::canonicalize(&addressed_path).map_err(|error| {
                    format!(
                        "Project '{project_name}' alias cannot be canonicalized at {}: {error}",
                        addressed_path.display()
                    )
                })
            }
            Ok(_) => Ok(addressed_path),
            Err(error) => Err(format!(
                "Project '{project_name}' database cannot be inspected at {}: {error}",
                addressed_path.display()
            )),
        }
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
    use crate::test_support::{CwdRestore, EnvRestore};

    fn with_env_lock<F: FnOnce()>(f: F) {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        f();
    }

    #[test]
    fn resolve_named_project_uses_workspace_data_tachi_home() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tmp");
            let _tachi_home = EnvRestore::remove("TACHI_HOME");
            let _sigil_home = EnvRestore::remove("SIGIL_HOME");
            let _app_home = EnvRestore::remove("TACHI_APP_HOME");

            let repo = tmp.path().join("Quant_Analyzer_2026");
            let nested = repo.join("engine/v8");
            let named_db = repo.join("data/tachi/projects/hyperion/memory.db");
            std::fs::create_dir_all(&nested).expect("nested");
            std::fs::create_dir_all(named_db.parent().unwrap()).expect("named parent");
            std::fs::write(&named_db, b"").expect("named db placeholder");

            // RAII cwd guard, not a bare set-then-restore pair — a panicking
            // assertion below must not skip the restore and leak the changed
            // cwd into later tests in this process (same class of bug
            // `with_tachi_home` was hardened against for env vars, #1096
            // leaf-2a).
            let _cwd = CwdRestore::set(&nested);
            let named_db = std::fs::canonicalize(named_db).expect("canonical named db");
            let resolved =
                MemoryServer::resolve_named_project_db_path("hyperion").expect("resolve");
            assert_eq!(resolved, named_db);
        });
    }

    /// Change #2: named-project resolution prefers the manifest-recorded
    /// repo-local DB and does NOT depend on the Plan C symlink existing.
    #[test]
    fn resolve_prefers_manifest_repo_local_db_without_symlink() {
        with_env_lock(|| {
            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

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

            let previous = crate::path_utils::plan_c_previous_dir_name_from_root(&repo)
                .expect("pre-#1228 identity");
            let (canonical_name, previous_resolved) =
                MemoryServer::resolve_named_project_binding_in_home(&previous, &tachi_home)
                    .expect("pre-#1228 identity remains a compatibility alias");
            assert_eq!(previous_resolved, local_db);
            assert_eq!(canonical_name, name);
        });
    }

    #[cfg(unix)]
    #[test]
    fn legacy_only_named_binding_persists_canonical_identity_before_returning() {
        with_env_lock(|| {
            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
            let repo = tmp.path().join("Repo");
            let local_db = repo.join(".tachi/memory.db");
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("DB parent");
            std::fs::write(&local_db, b"db").expect("DB");
            let previous = crate::path_utils::plan_c_previous_dir_name_from_root(&repo)
                .expect("previous identity");
            let previous_alias = crate::path_utils::plan_c_global_db_path(&previous);
            std::fs::create_dir_all(previous_alias.parent().unwrap()).expect("alias parent");
            std::os::unix::fs::symlink(&local_db, &previous_alias).expect("legacy alias");

            let (canonical, resolved) =
                MemoryServer::resolve_named_project_binding_in_home(&previous, &tachi_home)
                    .expect("legacy binding migration");
            assert_ne!(canonical, previous);
            assert_eq!(
                std::fs::canonicalize(resolved).unwrap(),
                std::fs::canonicalize(&local_db).unwrap()
            );
            let reopened = MemoryServer::resolve_named_project_db_path(&canonical)
                .expect("returned canonical identity must be immediately reopenable");
            assert_eq!(
                std::fs::canonicalize(reopened).unwrap(),
                std::fs::canonicalize(local_db).unwrap()
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn legacy_custom_path_binding_migrates_to_canonical_manifest_identity() {
        with_env_lock(|| {
            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
            let repo = tmp.path().join("Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("repo");
            let custom_db = repo.join("data/project.db");
            std::fs::create_dir_all(custom_db.parent().unwrap()).expect("DB parent");
            std::fs::write(&custom_db, b"db").expect("DB");
            let previous = crate::path_utils::plan_c_previous_dir_name_from_root(&repo)
                .expect("previous identity");
            let previous_alias = crate::path_utils::plan_c_global_db_path(&previous);
            std::fs::create_dir_all(previous_alias.parent().unwrap()).expect("alias parent");
            std::os::unix::fs::symlink(&custom_db, &previous_alias).expect("legacy alias");

            let (canonical, resolved) =
                MemoryServer::resolve_named_project_binding_in_home(&previous, &tachi_home)
                    .expect("custom legacy path migration");
            assert_ne!(canonical, previous);
            assert_eq!(
                std::fs::canonicalize(resolved).unwrap(),
                std::fs::canonicalize(&custom_db).unwrap()
            );
            std::fs::remove_file(previous_alias).expect("remove compatibility alias");
            let reopened = MemoryServer::resolve_named_project_db_path(&canonical)
                .expect("canonical manifest identity must survive alias loss");
            assert_eq!(
                std::fs::canonicalize(reopened).unwrap(),
                std::fs::canonicalize(custom_db).unwrap()
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn canonical_identity_conflict_does_not_mutate_manifest_during_alias_migration() {
        with_env_lock(|| {
            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

            let repo = tmp.path().join("Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("repo");
            let intended_db = repo.join("data/project.db");
            std::fs::create_dir_all(intended_db.parent().unwrap()).expect("DB parent");
            std::fs::write(&intended_db, b"intended").expect("intended DB");
            let previous = crate::path_utils::plan_c_previous_dir_name_from_root(&repo)
                .expect("previous identity");
            let previous_alias = crate::path_utils::plan_c_global_db_path(&previous);
            std::fs::create_dir_all(previous_alias.parent().unwrap()).expect("alias parent");
            std::os::unix::fs::symlink(&intended_db, &previous_alias).expect("legacy alias");

            let other_repo = tmp.path().join("Other");
            std::fs::create_dir_all(other_repo.join(".git")).expect("other repo");
            let conflicting_db = other_repo.join("data/project.db");
            std::fs::create_dir_all(conflicting_db.parent().unwrap()).expect("other DB parent");
            std::fs::write(&conflicting_db, b"conflict").expect("conflicting DB");
            let canonical =
                crate::path_utils::plan_c_dir_name_from_root(&repo).expect("canonical identity");
            let manifest_path = tachi_home.join("manifest.json");
            let manifest = serde_json::json!({
                "schema_version": 1,
                "generated_at": "1970-01-01T00:00:00Z",
                "_comment": "conflict must remain byte-identical",
                "dbs": [{
                    "path": std::fs::canonicalize(&conflicting_db).unwrap().to_string_lossy(),
                    "role": "project",
                    "owner": "tachi",
                    "schema_kind": "tachi",
                    "vec_enabled": true,
                    "allow_write": true,
                    "last_doctor_at": "1970-01-01T00:00:00Z",
                    "last_classification": "healthy",
                    "scope_hint": format!("project:{canonical}"),
                    "notes": "conflict"
                }]
            });
            let before = serde_json::to_vec_pretty(&manifest).unwrap();
            std::fs::write(&manifest_path, &before).expect("manifest");

            let error = MemoryServer::resolve_named_project_binding_in_home(&previous, &tachi_home)
                .expect_err("canonical identity conflict must block migration");
            assert!(error.contains("unrelated DB"), "{error}");
            assert_eq!(
                std::fs::read(manifest_path).unwrap(),
                before,
                "failed migration must not mutate manifest bytes"
            );
        });
    }

    #[test]
    fn standalone_named_store_inside_git_repo_keeps_its_explicit_identity() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let repo = tmp.path().join("Hyperion");
            std::fs::create_dir_all(repo.join(".git")).expect("repo");
            let tachi_home = repo.join("data/tachi");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
            let db = crate::path_utils::plan_c_global_db_path("wiki");
            std::fs::create_dir_all(db.parent().unwrap()).expect("named store parent");
            std::fs::write(&db, b"standalone").expect("named store");

            let (identity, resolved) =
                MemoryServer::resolve_named_project_binding_in_home("wiki", &tachi_home)
                    .expect("standalone store binding");
            assert_eq!(identity, "wiki");
            assert_eq!(
                std::fs::canonicalize(resolved).unwrap(),
                std::fs::canonicalize(db).unwrap()
            );
            assert!(
                !tachi_home.join("manifest.json").exists(),
                "standalone named store must not be claimed as the enclosing Git project"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn named_project_runtime_opens_valid_plan_c_alias_via_physical_db() {
        with_env_lock(|| {
            use std::os::unix::fs::MetadataExt;

            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

            let repo = tmp.path().join("Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("repo");
            let local_db = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("project DB parent");
            drop(
                MemoryStore::open(local_db.to_str().expect("project DB path"))
                    .expect("seed project DB"),
            );
            crate::test_support::with_unrestricted_fixture_connection(&local_db, |connection| {
                connection.execute_batch(
                    "CREATE TABLE alias_open_probe (value INTEGER NOT NULL);
                     INSERT INTO alias_open_probe VALUES (7);",
                )
            })
            .expect("seed alias-open probe fixture");
            let target_identity = {
                let metadata = std::fs::metadata(&local_db).expect("project DB metadata");
                (metadata.dev(), metadata.ino())
            };
            let project_name =
                crate::path_utils::plan_c_dir_name_from_root(&repo).expect("project name");
            let alias = crate::path_utils::plan_c_global_db_path(&project_name);
            std::fs::create_dir_all(alias.parent().unwrap()).expect("alias parent");
            std::os::unix::fs::symlink(&local_db, &alias).expect("Plan C alias");

            let global_db = tmp.path().join("global.db");
            let server = MemoryServer::new(global_db, None).expect("server");
            let value: i64 = server
                .with_named_project_store_read(&project_name, |store| {
                    store
                        .connection()
                        .query_row("SELECT value FROM alias_open_probe", [], |row| row.get(0))
                        .map_err(|error| error.to_string())
                })
                .expect("read through valid Plan C alias");

            assert_eq!(value, 7);
            assert_eq!(std::fs::read_link(&alias).unwrap(), local_db);
            let metadata = std::fs::metadata(&local_db).expect("project DB metadata after open");
            assert_eq!((metadata.dev(), metadata.ino()), target_identity);
        });
    }

    #[test]
    fn root_named_regular_store_inside_git_repo_is_not_a_compatibility_alias() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let repo = tmp.path().join("Hyperion");
            std::fs::create_dir_all(repo.join(".git")).expect("repo");
            let tachi_home = repo.join("data/tachi");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
            let db = crate::path_utils::plan_c_global_db_path("Hyperion");
            std::fs::create_dir_all(db.parent().unwrap()).expect("named store parent");
            std::fs::write(&db, b"standalone").expect("named store");

            let (identity, resolved) =
                MemoryServer::resolve_named_project_binding_in_home("Hyperion", &tachi_home)
                    .expect("root-named standalone store binding");
            assert_eq!(identity, "Hyperion");
            assert_eq!(
                std::fs::canonicalize(resolved).unwrap(),
                std::fs::canonicalize(db).unwrap()
            );
            assert!(
                !tachi_home.join("manifest.json").exists(),
                "a regular standalone store is not a repo compatibility alias"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn legacy_named_binding_rejects_divergent_root_wide_aliases_before_migration() {
        with_env_lock(|| {
            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
            let repo = tmp.path().join("Repo");
            let local_db = repo.join(".tachi/memory.db");
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("DB parent");
            std::fs::write(&local_db, b"repo").expect("DB");
            let previous = crate::path_utils::plan_c_previous_dir_name_from_root(&repo)
                .expect("previous identity");
            let previous_alias = crate::path_utils::plan_c_global_db_path(&previous);
            std::fs::create_dir_all(previous_alias.parent().unwrap()).expect("alias parent");
            std::os::unix::fs::symlink(&local_db, &previous_alias).expect("previous alias");
            let legacy_alias = crate::path_utils::plan_c_global_db_path("Repo");
            std::fs::create_dir_all(legacy_alias.parent().unwrap()).expect("legacy parent");
            std::fs::write(&legacy_alias, b"different").expect("legacy DB");

            let error = MemoryServer::resolve_named_project_db_path(&previous)
                .expect_err("direct routing must reject divergent compatibility aliases");
            assert!(
                error.contains("ambiguous across divergent aliases"),
                "{error}"
            );
            assert!(
                !tachi_home.join("manifest.json").exists(),
                "failed migration must not persist a canonical binding"
            );
        });
    }

    #[test]
    fn canonical_and_legacy_filenames_in_one_named_store_must_agree() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
            let dir = tachi_home.join("projects/wiki");
            std::fs::create_dir_all(&dir).expect("named store");
            std::fs::write(dir.join(memcore::MEMORY_DB_FILENAME), b"new").expect("new DB");
            std::fs::write(dir.join(memcore::LEGACY_MEMORY_DB_FILENAME), b"old").expect("old DB");

            let error = MemoryServer::resolve_named_project_db_path("wiki")
                .expect_err("two divergent filename candidates must not be first-wins");
            assert!(
                error.contains("ambiguous across divergent aliases"),
                "{error}"
            );
        });
    }

    #[test]
    fn ambiguous_legacy_alias_across_two_repo_dbs_fails_closed() {
        with_env_lock(|| {
            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

            let mut entries = Vec::new();
            for parent in ["one", "two"] {
                let db = tmp.path().join(parent).join("Shared/.tachi/memory.db");
                std::fs::create_dir_all(db.parent().unwrap()).expect("db parent");
                std::fs::write(&db, b"").expect("db");
                entries.push(serde_json::json!({
                    "path": db.to_string_lossy(),
                    "role": "project",
                    "owner": "tachi",
                    "schema_kind": "tachi",
                    "vec_enabled": false,
                    "allow_write": true,
                    "last_doctor_at": "1970-01-01T00:00:00Z",
                    "last_classification": "healthy",
                    "scope_hint": "project:Shared"
                }));
            }
            let manifest = serde_json::json!({
                "schema_version": 1,
                "generated_at": "1970-01-01T00:00:00Z",
                "comment": "test",
                "dbs": entries
            });
            std::fs::write(
                tachi_home.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .expect("manifest");

            let error = MemoryServer::resolve_named_project_db_path("Shared")
                .expect_err("a colliding unhashed legacy alias must not pick the first DB");
            assert!(error.contains("ambiguous across 2"), "{error}");
        });
    }

    #[test]
    fn legacy_repo_alias_cannot_hijack_distinct_standalone_named_store() {
        with_env_lock(|| {
            let tmp = crate::test_support::non_skipped_fixture_tempdir("server-methods-");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

            let repo_db = tmp.path().join("repo/Shared/.tachi/memory.db");
            std::fs::create_dir_all(repo_db.parent().unwrap()).expect("repo DB parent");
            std::fs::write(&repo_db, b"repo").expect("repo DB");
            let standalone = crate::path_utils::plan_c_global_db_path("Shared");
            std::fs::create_dir_all(standalone.parent().unwrap()).expect("standalone parent");
            std::fs::write(&standalone, b"standalone").expect("standalone DB");

            let manifest = serde_json::json!({
                "schema_version": 1,
                "generated_at": "1970-01-01T00:00:00Z",
                "comment": "test",
                "dbs": [{
                    "path": repo_db.to_string_lossy(),
                    "role": "project",
                    "owner": "tachi",
                    "schema_kind": "tachi",
                    "vec_enabled": false,
                    "allow_write": true,
                    "last_doctor_at": "1970-01-01T00:00:00Z",
                    "last_classification": "healthy",
                    "scope_hint": "project:Shared"
                }]
            });
            std::fs::write(
                tachi_home.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .expect("manifest");

            let error = MemoryServer::resolve_named_project_db_path("Shared")
                .expect_err("one identity must not choose between two physical DBs");
            assert!(error.contains("ambiguous between DB"), "{error}");
            assert_eq!(std::fs::read(repo_db).unwrap(), b"repo");
            assert_eq!(std::fs::read(standalone).unwrap(), b"standalone");
        });
    }

    #[test]
    fn corrupt_manifest_does_not_fall_back_to_named_alias() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
            std::fs::write(tachi_home.join("manifest.json"), b"not-json").expect("manifest");
            let alias = crate::path_utils::plan_c_global_db_path("wiki");
            std::fs::create_dir_all(alias.parent().unwrap()).expect("alias parent");
            std::fs::write(alias, b"standalone").expect("alias");

            let error = MemoryServer::resolve_named_project_db_path("wiki")
                .expect_err("incomplete manifest authority must fail closed");
            assert!(error.contains("manifest cannot be read"), "{error}");
        });
    }

    #[test]
    fn matching_missing_manifest_db_does_not_fall_back_to_named_alias() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
            let repo = tmp.path().join("Repo");
            std::fs::create_dir_all(repo.join(".tachi")).expect("repo");
            let identity = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("identity");
            let missing = repo.join(".tachi/memory.db");
            let manifest = serde_json::json!({
                "schema_version": 1,
                "generated_at": "1970-01-01T00:00:00Z",
                "comment": "test",
                "dbs": [{
                    "path": missing.to_string_lossy(),
                    "role": "project",
                    "owner": "tachi",
                    "schema_kind": "tachi",
                    "vec_enabled": false,
                    "allow_write": true,
                    "last_doctor_at": "1970-01-01T00:00:00Z",
                    "last_classification": "healthy",
                    "scope_hint": format!("project:{identity}")
                }]
            });
            std::fs::write(
                tachi_home.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .expect("manifest");
            std::fs::remove_dir_all(&repo).expect("remove missing repo root");
            let alias = crate::path_utils::plan_c_global_db_path(&identity);
            std::fs::create_dir_all(alias.parent().unwrap()).expect("alias parent");
            std::fs::write(alias, b"stale").expect("alias");

            let error = MemoryServer::resolve_named_project_db_path(&identity)
                .expect_err("a missing authoritative DB must not fall back");
            assert!(error.contains("missing or unreadable"), "{error}");
        });
    }

    #[test]
    fn alias_migration_creates_canonical_alias_and_converges_from_disk() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

            let repo = tmp.path().join("Sigil");
            let local_db = repo.join(".tachi/tachi-memory.db");
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("repo local parent");
            let repo = std::fs::canonicalize(&repo).expect("canon repo");
            let local_db = repo.join(".tachi/tachi-memory.db");
            std::fs::write(&local_db, b"sqlite-header-test-data").expect("write local db");
            let local_db = std::fs::canonicalize(&local_db).expect("canon local db");

            let gen3_name =
                crate::path_utils::plan_c_previous_dir_name_from_root(&repo).expect("gen3 name");
            let gen4_name = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("gen4 name");
            assert_ne!(gen3_name, gen4_name, "gen3 and gen4 must differ");

            // Setup: ONLY gen-3 alias exists on disk
            let gen3_dir = tachi_home.join("projects").join(&gen3_name);
            std::fs::create_dir_all(&gen3_dir).expect("create gen3 dir");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&local_db, gen3_dir.join(memcore::MEMORY_DB_FILENAME))
                .expect("gen3 symlink");

            let gen4_dir = tachi_home.join("projects").join(&gen4_name);
            assert!(
                !gen4_dir.exists(),
                "precondition: gen4 dir does not exist yet"
            );

            // 1. A caller arrives addressing the store through gen-3 alias
            let (settled_label, resolved_db) =
                MemoryServer::resolve_named_project_binding_in_home(&gen3_name, &tachi_home)
                    .expect("resolve binding");
            assert_eq!(
                settled_label, gen4_name,
                "binding settled on canonical gen-4 name"
            );
            assert_eq!(
                std::fs::canonicalize(&resolved_db).expect("canonicalize resolved_db"),
                local_db,
                "resolved db matches local db"
            );

            // 2. Convergence: on-disk canonical gen-4 alias directory must now exist!
            assert!(
                gen4_dir.exists(),
                "canonical gen4 directory must now exist on disk"
            );
            let canonical_from_disk =
                crate::path_utils::named_project_for_db_path_in_home(&local_db, &tachi_home)
                    .expect("named project lookup from disk");
            assert_eq!(
                canonical_from_disk, gen4_name,
                "disk resolution must immediately converge on gen-4 canonical identity"
            );

            // 3. Robustness: delete the legacy gen-3 directory entirely from disk
            std::fs::remove_dir_all(&gen3_dir).expect("remove legacy gen3 dir");
            assert!(!gen3_dir.exists());

            // 4. Disk resolution continues to resolve cleanly via canonical gen-4 alias
            let canonical_after_gen3_removal =
                crate::path_utils::named_project_for_db_path_in_home(&local_db, &tachi_home)
                    .expect("named project lookup after gen3 removal");
            assert_eq!(
                canonical_after_gen3_removal, gen4_name,
                "disk resolution must still succeed through canonical name after legacy removal"
            );

            let (settled_again, _) =
                MemoryServer::resolve_named_project_binding_in_home(&gen4_name, &tachi_home)
                    .expect("resolve binding after gen3 removal");
            assert_eq!(settled_again, gen4_name);
        });
    }
}
