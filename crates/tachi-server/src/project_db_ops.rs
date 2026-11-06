use crate::server_state::MemoryServer;
use crate::tool_params::InitProjectDbParams;
use crate::utils::find_git_root_from;
use serde_json::json;
use std::path::PathBuf;

/// Repo-local project DB convention (see `path_utils/alias.rs`'s addressing
/// doc). Fixed, not caller-configurable, for the session-init auto-register
/// path: the header carries a filesystem root, not a `db_relpath` — that
/// customization stays on the explicit `tachi_init_project_db` tool.
fn workspace_root_db_relpath() -> String {
    format!(".tachi/{}", memcore::MEMORY_DB_FILENAME)
}

impl MemoryServer {
    /// Resolve an explicit directory on the daemon host without creating state.
    /// Git worktrees share their primary root; ordinary directories reuse the
    /// nearest registered or database-backed workspace before declaring a new one.
    fn resolve_workspace_root(&self, raw_root: &str) -> Result<PathBuf, String> {
        let candidate = PathBuf::from(raw_root);
        if !candidate.is_absolute() {
            return Err(format!(
                "workspace root '{raw_root}' must be an absolute path — a relative path would \
                 resolve against the DAEMON's own cwd, not the caller's workspace"
            ));
        }
        let metadata = std::fs::metadata(&candidate).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!("workspace root '{raw_root}' does not exist on the daemon host; for a remote daemon bind by X-Tachi-Project to an already-registered project")
            } else {
                format!("cannot inspect workspace root '{raw_root}': {error}")
            }
        })?;
        if !metadata.is_dir() {
            return Err(format!("workspace root '{raw_root}' must be a directory"));
        }
        let candidate = std::fs::canonicalize(&candidate)
            .map_err(|error| format!("cannot canonicalize workspace root '{raw_root}': {error}"))?;
        if let Some(root) = find_git_root_from(&candidate) {
            return Ok(root);
        }
        let home = self.tachi_home_dir();
        for ancestor in candidate.ancestors() {
            if let Some(project) = crate::path_utils::plan_c_dir_name_from_root(ancestor) {
                if Self::resolve_existing_named_project_db_path_in_home(&project, &home)?.is_some()
                {
                    return Ok(ancestor.to_path_buf());
                }
            }
            let local_dir = ancestor.join(".tachi");
            if std::fs::canonicalize(&local_dir).ok() == std::fs::canonicalize(&home).ok()
                && home.exists()
            {
                continue;
            }
            let db = local_dir.join(memcore::MEMORY_DB_FILENAME);
            match std::fs::symlink_metadata(&db) {
                Ok(_) => return Ok(ancestor.to_path_buf()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "cannot inspect workspace DB {}: {error}",
                        db.display()
                    ))
                }
            }
            let legacy = local_dir.join(memcore::LEGACY_MEMORY_DB_FILENAME);
            match std::fs::symlink_metadata(&legacy) {
                Ok(_) => return Ok(ancestor.to_path_buf()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "cannot inspect workspace DB {}: {error}",
                        legacy.display()
                    ))
                }
            }
        }
        Ok(candidate)
    }

    /// Resolve every candidate before registering anything. Multiple roots can
    /// be subdirectories or worktrees of one workspace, or aliases of one DB.
    pub(crate) fn resolve_or_register_workspaces(
        &self,
        roots: &[String],
    ) -> Result<Option<String>, String> {
        let mut candidates = std::collections::BTreeMap::new();
        for raw in roots {
            let root = self.resolve_workspace_root(raw)?;
            let project = crate::path_utils::plan_c_dir_name_from_root(&root)
                .ok_or_else(|| "workspace has no usable identity".to_string())?;
            let existing = Self::resolve_existing_named_project_db_path_in_home(
                &project,
                &self.tachi_home_dir(),
            )?;
            let identity = match existing {
                Some(path) => std::fs::canonicalize(path)
                    .map_err(|error| format!("cannot resolve workspace database: {error}"))?,
                None => root.join(workspace_root_db_relpath()),
            };
            candidates.entry(identity).or_insert(root);
        }
        if candidates.len() > 1 {
            return Err("Workspace context identifies multiple project libraries; select the current workspace with X-Tachi-Workspace-Root or tachiWorkspaceRoot".into());
        }
        candidates
            .into_values()
            .next()
            .map(|root| {
                let root = root
                    .to_str()
                    .ok_or_else(|| "workspace path is not UTF-8".to_string())?;
                self.resolve_or_register_workspace_root(root)
            })
            .transpose()
    }

    /// Resolve an already registered workspace for read-only fallback. Unlike
    /// first-contact binding, this never creates a DB, alias, or manifest entry.
    pub(crate) fn resolve_existing_workspace_project(
        &self,
        raw_root: &str,
    ) -> Result<String, String> {
        let root = self.resolve_workspace_root(raw_root)?;
        let project = crate::path_utils::plan_c_dir_name_from_root(&root)
            .ok_or_else(|| format!("workspace '{}' has no usable identity", root.display()))?;
        self.resolve_server_named_project_db_path(&project)?;
        Ok(project)
    }

    /// Bind an explicit workspace directory to its registered project library.
    /// Registration never replaces the shared daemon's static project slot.
    pub(crate) fn resolve_or_register_workspace_root(
        &self,
        raw_root: &str,
    ) -> Result<String, String> {
        let _registration = workspace_registration_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let workspace_root = self.resolve_workspace_root(raw_root)?;
        let project_name = crate::path_utils::plan_c_dir_name_from_root(&workspace_root)
            .ok_or_else(|| {
                format!(
                    "workspace root '{raw_root}' resolved to '{}' but it has no usable \
                     directory name to derive a project identity from",
                    workspace_root.display()
                )
            })?;

        // Route through the same canonical containment guard
        // `handle_tachi_init_project_db` uses below
        // (`path_utils::alias::resolve_project_db_path`) instead of a bare
        // `workspace_root.join(...)`: a repo-controlled `.tachi` symlink must not be
        // able to redirect DB creation outside `workspace_root`. The guard
        // canonicalizes the resolved parent directory and rejects anything
        // that escapes `workspace_root` after symlink resolution (review finding
        // [1], #1207).
        let workspace_root_db_relpath = workspace_root_db_relpath();
        let db_path = crate::path_utils::resolve_project_db_path(
            &workspace_root,
            std::path::Path::new(&workspace_root_db_relpath),
        )
        .map_err(|e| {
            format!(
                "workspace root '{raw_root}' resolved to '{}' but its \
                 {workspace_root_db_relpath} path is unsafe: {e}",
                workspace_root.display()
            )
        })?;
        if !crate::path_utils::canonical_db_leaf_exists_without_symlink(&db_path)? {
            let legacy = workspace_root
                .join(".tachi")
                .join(memcore::LEGACY_MEMORY_DB_FILENAME);
            match std::fs::symlink_metadata(&legacy) {
                Ok(_) => return Err(format!("workspace has a legacy DB at {}; register that existing DB explicitly before binding; refusing to create an empty replacement", legacy.display())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("cannot inspect workspace DB {}: {error}", legacy.display())),
            }
        }
        // Registration may continue only when identity lookup proves genuine
        // absence. A successful lookup must resolve to this exact repo-local
        // DB; ambiguity, manifest failure, or a same-name standalone store is
        // an error, never a reason to create/open another DB.
        let mut precommit = ProjectDbPrecommit::new(db_path.clone());
        let already_resolved = preflight_project_identity(
            &db_path,
            &workspace_root,
            &project_name,
            &self.tachi_home_dir(),
        )?;
        if !already_resolved {
            if let Err(error) = precommit.reserve_db() {
                return Err(precommit.abort(error));
            }
        }
        if let Err(error) =
            precommit.ensure_alias(&workspace_root, &project_name, &self.tachi_home_dir())
        {
            return Err(precommit.abort(error));
        }
        if already_resolved {
            precommit.commit();
            return Ok(project_name);
        }
        let open_result = if precommit.created_db() {
            precommit.open_db()
        } else {
            crate::path_utils::canonical_db_leaf_exists_without_symlink(&db_path)?;
            self.with_path_store(&db_path, |_store| Ok(()))
                .map_err(|error| format!("initialize project DB at {}: {error}", db_path.display()))
        };
        if let Err(error) = open_result {
            return Err(precommit.abort(error));
        }
        if let Err(error) = precommit.assert_owned_db_artifacts_unchanged() {
            return Err(precommit.abort(error));
        }
        // Primary registration: write a manifest entry so
        // `resolve_named_project_db_path` can find this DB by name
        // independent of the Plan C symlink below — the manifest-recorded
        // repo-local path is the addressing scheme's documented preferred
        // path (`server_methods/db.rs::resolve_named_project_db_path`'s doc
        // comment), and unlike the symlink it works on every platform (review
        // finding [2], #1207: `ensure_plan_c_symlink` is a no-op `Skipped` on
        // non-Unix hosts, so a project registered only via the symlink could
        // never be reopened there).
        let registration = register_repo_local_manifest_entry_then_in_home(
            &db_path,
            &project_name,
            &self.tachi_home_dir(),
            || {
                precommit.assert_owned_db_artifacts_unchanged()?;
                let resolved = self
                    .resolve_server_named_project_db_path(&project_name)
                    .map_err(|err| {
                        format!(
                    "project db was created at {} but is not resolvable by its derived name \
                         '{project_name}': {err}",
                    db_path.display()
                )
                    })?;
                precommit.assert_owned_db_artifacts_unchanged()?;
                Ok(resolved)
            },
        );
        if let Err(error) = registration {
            return Err(precommit.abort(error));
        }
        precommit.commit();

        // Loud by design (#1120): first-contact auto-registration is a
        // meaningful state change (a new DB file on disk) and must be visible
        // in the daemon log, not silent.
        tracing::info!(
            target: "tachi::project_db::auto_register",
            project = %project_name,
            db_path = %db_path.display(),
            workspace_root = %raw_root,
            "auto-registered project DB on first contact via X-Tachi-Workspace-Root"
        );
        eprintln!(
            "[project-db] auto-registered '{project_name}' ({}) from X-Tachi-Workspace-Root '{raw_root}'",
            db_path.display()
        );

        Ok(project_name)
    }
}

/// Register a just-created repo-local `<git_root>/.tachi/tachi-memory.db` in the
/// manifest so [`MemoryServer::resolve_named_project_db_path`] can find it by
/// name independent of the (Unix-only, best-effort) Plan C symlink — see
/// `server_methods/db.rs::resolve_named_project_db_path`'s doc comment: the
/// manifest-recorded repo-local path is the addressing scheme's PRIMARY
/// resolution path, the symlink is a legacy/secondary fallback.
///
/// Mirrors the single-entry registration shape
/// `bootstrap/tidy/migration.rs::update_manifest_after_migration` writes for
/// its own callers; unlike that helper this never removes or rewrites any
/// entry but the one it is registering, and refuses to silently fabricate a
/// fresh empty manifest over a manifest file that exists but fails to parse
/// (an unreadable/corrupt manifest is an error here, not "no entries yet" —
/// overwriting it via `load_or_empty` would silently drop every other
/// registered project's entry).
pub(crate) fn register_repo_local_manifest_entry_in_home(
    db_path: &std::path::Path,
    project_name: &str,
    tachi_home: &std::path::Path,
) -> Result<(), String> {
    let manifest_path = tachi_home.join("manifest.json");
    let _process_guard = manifest_registration_mutex()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    with_manifest_registration_file_lock(&manifest_path, || {
        register_repo_local_manifest_entry_locked(db_path, project_name, &manifest_path)
    })?;
    if let Some(root) =
        crate::path_utils::plan_c_project_root_from_local_db_in_home(db_path, tachi_home)
    {
        crate::path_utils::require_plan_c_alias_success(
            crate::path_utils::ensure_plan_c_canonical_alias_in_home(db_path, &root, tachi_home),
        )?;
    }
    Ok(())
}

pub(crate) fn register_repo_local_manifest_entry_then_in_home<T>(
    db_path: &std::path::Path,
    project_name: &str,
    tachi_home: &std::path::Path,
    after_registration: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let manifest_path = tachi_home.join("manifest.json");
    let _process_guard = manifest_registration_mutex()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    with_manifest_registration_file_lock(&manifest_path, || {
        let preimage = match std::fs::read(&manifest_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "read manifest preimage {}: {error}",
                    manifest_path.display()
                ));
            }
        };
        register_repo_local_manifest_entry_locked(db_path, project_name, &manifest_path)?;
        let written_state = std::fs::read(&manifest_path).map_err(|error| {
            format!(
                "read manifest transaction state {} after registration: {error}",
                manifest_path.display()
            )
        })?;
        let manifest_changed = preimage.as_ref() != Some(&written_state);
        match after_registration() {
            Ok(value) => Ok(value),
            Err(error) => {
                let rollback = if manifest_changed {
                    restore_manifest_preimage_if_unchanged(
                        &manifest_path,
                        &written_state,
                        preimage.as_deref(),
                    )
                } else {
                    Ok(())
                };
                match rollback {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(format!(
                        "{error}; manifest rollback at {} also failed: {rollback}",
                        manifest_path.display()
                    )),
                }
            }
        }
    })
}

fn restore_manifest_preimage_if_unchanged(
    manifest_path: &std::path::Path,
    written_state: &[u8],
    preimage: Option<&[u8]>,
) -> Result<(), String> {
    let current = match std::fs::read(manifest_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "manifest {} disappeared after this transaction wrote it; refusing to restore or remove a non-cooperating writer's state",
                manifest_path.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "read manifest {} before rollback ownership check: {error}",
                manifest_path.display()
            ));
        }
    };
    if current != written_state {
        return Err(format!(
            "manifest {} changed after this transaction wrote it; refusing to overwrite or remove non-cooperating writer data",
            manifest_path.display()
        ));
    }
    match preimage {
        Some(bytes) => crate::utils::write_owner_only_file_atomic(manifest_path, bytes),
        None => match std::fs::remove_file(manifest_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
                "manifest {} disappeared during rollback after ownership check; refusing to assume it remained transaction-owned",
                manifest_path.display()
            )),
            Err(error) => Err(format!("remove {}: {error}", manifest_path.display())),
        },
    }
}

fn manifest_registration_mutex() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Serialize first-contact registration before reservation as well as manifest
/// commit. Concurrent requests must never observe another request's empty DB.
fn workspace_registration_mutex() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(unix)]
fn with_manifest_registration_file_lock<T>(
    manifest_path: &std::path::Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;

    let lock_path = manifest_path.with_extension("json.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create manifest lock directory {}: {e}", parent.display()))?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open manifest lock {}: {e}", lock_path.display()))?;
    let fd = lock_file.as_raw_fd();
    // SAFETY: `fd` belongs to the live `lock_file`; flock receives only the
    // descriptor and integer flags and is released when this function exits.
    if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
        return Err(format!(
            "lock manifest registration {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let result = f();
    // SAFETY: the descriptor remains live. Close also releases the lock if
    // this best-effort explicit unlock fails.
    unsafe {
        libc::flock(fd, libc::LOCK_UN);
    }
    result
}

#[cfg(not(unix))]
fn with_manifest_registration_file_lock<T>(
    _manifest_path: &std::path::Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    f()
}

fn manifest_entry_matches_canonical(
    entry: &crate::manifest::DbEntry,
    canonical: &std::path::Path,
) -> bool {
    let canonical_str = canonical.display().to_string();
    entry.path == canonical_str
        || std::fs::canonicalize(std::path::Path::new(&entry.path))
            .map(|path| path == canonical)
            .unwrap_or(false)
}

fn upsert_global_manifest_entry(
    manifest: &mut crate::manifest::Manifest,
    canonical: &std::path::Path,
) -> Result<bool, String> {
    let matching = manifest
        .dbs
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            manifest_entry_matches_canonical(entry, canonical).then_some(index)
        })
        .collect::<Vec<_>>();

    let live_conflicts = manifest
        .dbs
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            (entry.role == crate::manifest::DbRole::Global
                && !matching.contains(&index)
                && std::path::Path::new(&entry.path).exists())
            .then_some(entry.path.clone())
        })
        .collect::<Vec<_>>();
    if !live_conflicts.is_empty() {
        return Err(format!(
            "global manifest authority conflict: live Global path(s) {} conflict with candidate {}",
            live_conflicts.join(", "),
            canonical.display()
        ));
    }

    let stale_globals = manifest
        .dbs
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            (entry.role == crate::manifest::DbRole::Global && !matching.contains(&index))
                .then_some(index)
        })
        .collect::<Vec<_>>();

    let target = matching
        .first()
        .copied()
        .or_else(|| stale_globals.first().copied());
    let target = if let Some(target) = target {
        target
    } else {
        manifest.dbs.push(crate::manifest::DbEntry {
            path: canonical.display().to_string(),
            role: crate::manifest::DbRole::Global,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "global".to_string(),
            notes: "auto-registered global store".to_string(),
        });
        return Ok(true);
    };

    let replacing_stale = !matching.contains(&target);
    let entry = &mut manifest.dbs[target];
    if replacing_stale {
        *entry = crate::manifest::DbEntry {
            path: canonical.display().to_string(),
            role: crate::manifest::DbRole::Global,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "global".to_string(),
            notes: "auto-registered global store".to_string(),
        };
    } else {
        let identity_path = entry.path.clone();
        *entry = crate::manifest::DbEntry {
            path: identity_path,
            role: crate::manifest::DbRole::Global,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "global".to_string(),
            notes: "auto-registered global store".to_string(),
        };
    }

    let changed = true;
    let mut remove = matching
        .into_iter()
        .filter(|index| *index != target)
        .collect::<Vec<_>>();
    remove.extend(stale_globals.into_iter().filter(|index| *index != target));
    remove.sort_unstable();
    remove.dedup();
    for index in remove.into_iter().rev() {
        manifest.dbs.remove(index);
    }
    Ok(changed)
}

fn register_repo_local_manifest_entry_locked(
    db_path: &std::path::Path,
    project_name: &str,
    manifest_path: &std::path::Path,
) -> Result<(), String> {
    let mut manifest = if manifest_path.exists() {
        crate::manifest::Manifest::load(manifest_path)
            .map_err(|e| format!("load manifest {}: {e}", manifest_path.display()))?
    } else {
        crate::manifest::Manifest::empty()
    };

    let mut manifest_changed = false;
    if let Some(parent) = manifest_path.parent() {
        let global_candidates = [
            parent.join(memcore::MEMORY_DB_FILENAME),
            parent.join(memcore::LEGACY_MEMORY_DB_FILENAME),
            parent.join("global").join(memcore::MEMORY_DB_FILENAME),
            parent
                .join("global")
                .join(memcore::LEGACY_MEMORY_DB_FILENAME),
        ];
        let mut seen_global_paths = Vec::new();
        for path in global_candidates {
            if !path.exists() {
                continue;
            }
            let canonical = std::fs::canonicalize(&path).unwrap_or(path);
            if seen_global_paths.iter().any(|seen| seen == &canonical) {
                continue;
            }
            seen_global_paths.push(canonical.clone());
        }
        if seen_global_paths.len() > 1 {
            let candidates = seen_global_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "ambiguous global store identity under {}: distinct candidate databases: {candidates}",
                parent.display()
            ));
        }
        for canonical in seen_global_paths {
            manifest_changed |= upsert_global_manifest_entry(&mut manifest, &canonical)?;
        }
    }

    let canonical = std::fs::canonicalize(db_path).unwrap_or_else(|_| db_path.to_path_buf());
    let canon_str = canonical.display().to_string();
    if let Some(entry) = manifest.dbs.iter_mut().find(|e| {
        let e_path = std::path::Path::new(&e.path);
        e.path == canon_str
            || std::fs::canonicalize(e_path)
                .map(|c| c == canonical)
                .unwrap_or(false)
    }) {
        // A physical canonical DB path is unambiguous authority. Refresh only
        // its derived identity label; never move or auto-claim an alias path.
        let canonical_scope = format!("project:{project_name}");
        if entry.scope_hint != canonical_scope {
            entry.scope_hint = canonical_scope;
            manifest_changed = true;
        }
        if manifest_changed {
            manifest.generated_at = chrono::Utc::now().to_rfc3339();
            return manifest
                .save(manifest_path)
                .map_err(|e| format!("save manifest {}: {e}", manifest_path.display()));
        }
        return Ok(());
    }

    manifest.dbs.push(crate::manifest::DbEntry {
        path: canon_str,
        role: crate::manifest::DbRole::Project,
        owner: "tachi".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: chrono::Utc::now().to_rfc3339(),
        last_classification: "healthy".to_string(),
        scope_hint: format!("project:{project_name}"),
        notes: "auto-registered via X-Tachi-Workspace-Root on first contact (#1120 PR1)"
            .to_string(),
    });
    manifest.generated_at = chrono::Utc::now().to_rfc3339();
    manifest
        .save(manifest_path)
        .map_err(|e| format!("save manifest {}: {e}", manifest_path.display()))
}

struct ProjectDbPrecommit {
    db_path: PathBuf,
    created_db_artifacts: Vec<OwnedDbArtifact>,
    created_alias: Option<OwnedSymlink>,
    created_dirs: Vec<OwnedDirectory>,
    db_artifacts_preexisting: Vec<(PathBuf, bool)>,
    finished: bool,
}

impl ProjectDbPrecommit {
    fn new(db_path: PathBuf) -> Self {
        let db_artifacts_preexisting = sqlite_owned_paths(&db_path)
            .into_iter()
            .map(|path| {
                let exists = std::fs::symlink_metadata(&path).is_ok();
                (path, exists)
            })
            .collect();
        Self {
            db_path,
            created_db_artifacts: Vec::new(),
            created_alias: None,
            created_dirs: Vec::new(),
            db_artifacts_preexisting,
            finished: false,
        }
    }

    fn ensure_alias(
        &mut self,
        project_root: &std::path::Path,
        project_name: &str,
        tachi_home: &std::path::Path,
    ) -> Result<(), String> {
        #[cfg(unix)]
        {
            let alias =
                crate::path_utils::plan_c_alias_db_for_root_in_home(project_root, tachi_home)
                    .map_err(|error| {
                        format!("resolve Plan C alias for '{project_name}': {error}")
                    })?;
            let parent = alias.parent().ok_or_else(|| {
                format!("Plan C alias {} has no parent directory", alias.display())
            })?;
            create_directories_tracked(parent, &mut self.created_dirs).map_err(|error| {
                format!(
                    "create Plan C alias parent directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        match crate::path_utils::ensure_plan_c_symlink_in_home(
            &self.db_path,
            project_root,
            tachi_home,
        ) {
            crate::path_utils::PlanCLinkOutcome::Created(path) => {
                self.created_alias = Some(OwnedSymlink::snapshot(path)?);
                Ok(())
            }
            crate::path_utils::PlanCLinkOutcome::AlreadyLinked
            | crate::path_utils::PlanCLinkOutcome::Skipped(_) => Ok(()),
            crate::path_utils::PlanCLinkOutcome::SplitBrain(issue) => {
                Err(issue.warning_message())
            }
            crate::path_utils::PlanCLinkOutcome::AliasIntegrity(issue) => {
                Err(issue.warning_message())
            }
            crate::path_utils::PlanCLinkOutcome::Failed { path, error } => Err(format!(
                "Plan C alias symlink failed at {} for project '{}': {}; refusing initialization success",
                path.display(),
                project_name,
                error
            )),
        }
    }

    fn reserve_db(&mut self) -> Result<(), String> {
        let parent = self.db_path.parent().ok_or_else(|| {
            format!(
                "project DB path {} has no parent directory",
                self.db_path.display()
            )
        })?;
        create_directories_tracked(parent, &mut self.created_dirs).map_err(|error| {
            format!(
                "create project DB parent directory {}: {error}",
                parent.display()
            )
        })?;

        if self.db_was_preexisting() {
            return Ok(());
        }
        match std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&self.db_path)
        {
            Ok(file) => {
                self.created_db_artifacts
                    .push(OwnedDbArtifact::from_open_file(self.db_path.clone(), file)?);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
                "project DB {} appeared after preflight; refusing to adopt an unreserved database",
                self.db_path.display()
            )),
            Err(error) => Err(format!(
                "atomically reserve project DB at {}: {error}",
                self.db_path.display()
            )),
        }
    }

    fn created_db(&self) -> bool {
        !self.created_db_artifacts.is_empty()
    }

    fn db_was_preexisting(&self) -> bool {
        self.db_artifacts_preexisting
            .iter()
            .find(|(path, _)| path == &self.db_path)
            .is_some_and(|(_, preexisting)| *preexisting)
    }

    fn open_db(&mut self) -> Result<(), String> {
        self.run_open_attempt(|db_path| {
            let db_path_str = db_path
                .to_str()
                .ok_or_else(|| format!("project DB path is not UTF-8: {}", db_path.display()))?;
            let store = memcore::MemoryStore::open(db_path_str).map_err(|error| {
                format!("initialize project DB at {}: {error}", db_path.display())
            })?;
            drop(store);
            Ok(())
        })
    }

    fn run_open_attempt<T>(
        &mut self,
        attempt: impl FnOnce(&std::path::Path) -> Result<T, String>,
    ) -> Result<T, String> {
        self.assert_owned_db_artifacts_unchanged()?;
        let result = attempt(&self.db_path);
        self.finish_open_attempt(result)
    }

    fn finish_open_attempt<T>(&mut self, result: Result<T, String>) -> Result<T, String> {
        if !self.created_db() {
            return result;
        }
        let observation = self.refresh_owned_db_artifacts_after_open_attempt();
        match (result, observation) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(observation_error)) => Err(observation_error),
            (Err(error), Err(observation_error)) => Err(format!(
                "{error}; post-open ownership observation also failed: {observation_error}"
            )),
        }
    }

    fn assert_owned_db_artifacts_unchanged(&self) -> Result<(), String> {
        for artifact in &self.created_db_artifacts {
            artifact.assert_unchanged()?;
        }
        Ok(())
    }

    fn refresh_owned_db_artifacts_after_open_attempt(&mut self) -> Result<(), String> {
        let db_artifact = self
            .created_db_artifacts
            .iter_mut()
            .find(|artifact| artifact.path() == self.db_path)
            .ok_or_else(|| {
                format!(
                    "reserved project DB {} lost its ownership handle during initialization; refusing cleanup",
                    self.db_path.display()
                )
            })?;
        db_artifact.refresh_same_object_state()?;

        let mut discovered = Vec::new();
        for (path, preexisting) in &self.db_artifacts_preexisting {
            if *preexisting
                || !path.exists()
                || self
                    .created_db_artifacts
                    .iter()
                    .any(|artifact| artifact.path() == path)
            {
                continue;
            }
            discovered.push(OwnedDbArtifact::snapshot(path.clone())?);
        }
        self.created_db_artifacts.extend(discovered);
        Ok(())
    }

    fn commit(mut self) {
        self.finished = true;
    }

    fn abort(mut self, error: String) -> String {
        let rollback_errors = self.rollback();
        self.finished = true;
        if rollback_errors.is_empty() {
            error
        } else {
            format!(
                "{error}; rollback also reported: {}",
                rollback_errors.join("; ")
            )
        }
    }

    fn rollback(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(alias) = self.created_alias.take() {
            if let Err(error) = alias.remove_if_unchanged() {
                errors.push(error);
            }
        }

        for artifact in std::mem::take(&mut self.created_db_artifacts) {
            if let Err(error) = artifact.remove_if_unchanged() {
                errors.push(error);
            }
        }

        for directory in self.created_dirs.drain(..).rev() {
            if let Err(error) = directory.remove_if_empty_and_unchanged() {
                errors.push(error);
            }
        }
        errors
    }
}

impl Drop for ProjectDbPrecommit {
    fn drop(&mut self) {
        if !self.finished {
            for error in self.rollback() {
                eprintln!("[project-db] rollback failure: {error}");
            }
        }
    }
}

fn create_directories_tracked(
    path: &std::path::Path,
    created: &mut Vec<OwnedDirectory>,
) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_directories_tracked(parent, created)?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => {
            created
                .push(OwnedDirectory::snapshot(path.to_path_buf()).map_err(std::io::Error::other)?);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileObjectIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    len: u64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

fn file_object_identity(metadata: &std::fs::Metadata) -> FileObjectIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        FileObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        FileObjectIdentity {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

#[derive(Debug)]
struct OwnedFile {
    path: PathBuf,
    object_identity: FileObjectIdentity,
    contents: Vec<u8>,
    handle: std::fs::File,
}

impl OwnedFile {
    fn snapshot(path: PathBuf) -> Result<Self, String> {
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(|error| {
                format!(
                    "open created DB artifact {} for ownership: {error}",
                    path.display()
                )
            })?;
        Self::from_open_file(path, handle)
    }

    fn from_open_file(path: PathBuf, handle: std::fs::File) -> Result<Self, String> {
        let object_identity = file_object_identity(&handle.metadata().map_err(|error| {
            format!(
                "inspect held created DB artifact {}: {error}",
                path.display()
            )
        })?);
        let mut owned = Self {
            path,
            object_identity,
            contents: Vec::new(),
            handle,
        };
        owned.refresh_same_object_state()?;
        Ok(owned)
    }

    fn stable_current_contents(&self) -> Result<Vec<u8>, String> {
        let held_identity = file_object_identity(&self.handle.metadata().map_err(|error| {
            format!(
                "inspect held created DB artifact {}: {error}",
                self.path.display()
            )
        })?);
        let path_identity_before = regular_file_identity(&self.path)?;
        if held_identity != self.object_identity || path_identity_before != self.object_identity {
            return Err(format!(
                "created DB artifact {} no longer resolves to this transaction's held object",
                self.path.display()
            ));
        }

        let read_contents = || -> Result<Vec<u8>, String> {
            use std::io::{Read, Seek};

            let mut reader = self.handle.try_clone().map_err(|error| {
                format!(
                    "clone held created DB artifact {} for reading: {error}",
                    self.path.display()
                )
            })?;
            reader.rewind().map_err(|error| {
                format!(
                    "rewind held created DB artifact {}: {error}",
                    self.path.display()
                )
            })?;
            let mut contents = Vec::new();
            reader.read_to_end(&mut contents).map_err(|error| {
                format!(
                    "read held created DB artifact {}: {error}",
                    self.path.display()
                )
            })?;
            Ok(contents)
        };
        let contents = read_contents()?;
        let verified_contents = read_contents()?;
        let path_identity_after = regular_file_identity(&self.path)?;
        if contents != verified_contents || path_identity_after != self.object_identity {
            return Err(format!(
                "created DB artifact {} changed while recording held ownership state",
                self.path.display()
            ));
        }
        Ok(contents)
    }

    fn refresh_same_object_state(&mut self) -> Result<(), String> {
        self.contents = self.stable_current_contents()?;
        Ok(())
    }

    fn assert_unchanged(&self) -> Result<(), String> {
        match self.stable_current_contents() {
            Ok(current) if current == self.contents => Ok(()),
            Ok(_) => Err(format!(
                "created DB artifact {} no longer matches this transaction's object and contents; preserving foreign data",
                self.path.display()
            )),
            Err(error) => Err(format!(
                "cannot prove created DB artifact {} is still transaction-owned ({error}); preserving foreign data",
                self.path.display()
            )),
        }
    }

    fn remove_if_unchanged(self) -> Result<(), String> {
        self.assert_unchanged()?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove transaction-owned DB artifact {}: {error}",
                self.path.display()
            )),
        }
    }
}

#[derive(Debug)]
enum OwnedDbArtifact {
    File(OwnedFile),
    Symlink(OwnedSymlink),
}

impl OwnedDbArtifact {
    fn from_open_file(path: PathBuf, file: std::fs::File) -> Result<Self, String> {
        OwnedFile::from_open_file(path, file).map(Self::File)
    }

    fn snapshot(path: PathBuf) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect created DB artifact {}: {error}", path.display()))?;
        if metadata.file_type().is_file() {
            return OwnedFile::snapshot(path).map(Self::File);
        }
        if metadata.file_type().is_symlink() {
            return OwnedSymlink::snapshot(path).map(Self::Symlink);
        }
        Err(format!(
            "created DB artifact {} is neither a regular file nor a symlink; preserving foreign data",
            path.display()
        ))
    }

    fn path(&self) -> &std::path::Path {
        match self {
            Self::File(file) => &file.path,
            Self::Symlink(symlink) => &symlink.path,
        }
    }

    fn refresh_same_object_state(&mut self) -> Result<(), String> {
        match self {
            Self::File(file) => file.refresh_same_object_state(),
            Self::Symlink(symlink) => {
                let current = OwnedSymlink::snapshot(symlink.path.clone())?;
                if current.object_identity != symlink.object_identity
                    || current.target != symlink.target
                {
                    return Err(format!(
                        "created DB artifact {} changed identity during initialization; refusing ownership guess",
                        symlink.path.display()
                    ));
                }
                Ok(())
            }
        }
    }

    fn assert_unchanged(&self) -> Result<(), String> {
        match self {
            Self::File(file) => file.assert_unchanged(),
            Self::Symlink(symlink) => symlink.assert_unchanged(),
        }
    }

    fn remove_if_unchanged(self) -> Result<(), String> {
        match self {
            Self::File(file) => file.remove_if_unchanged(),
            Self::Symlink(symlink) => symlink.remove_if_unchanged(),
        }
    }
}

#[derive(Clone, Debug)]
struct OwnedSymlink {
    path: PathBuf,
    object_identity: FileObjectIdentity,
    target: PathBuf,
}

impl OwnedSymlink {
    fn snapshot(path: PathBuf) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect created alias {}: {error}", path.display()))?;
        if !metadata.file_type().is_symlink() {
            return Err(format!(
                "created alias {} is no longer a symlink; preserving foreign data",
                path.display()
            ));
        }
        let object_identity = file_object_identity(&metadata);
        let target = std::fs::read_link(&path)
            .map_err(|error| format!("read created alias {}: {error}", path.display()))?;
        let verified = std::fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "re-inspect created alias {} to verify ownership snapshot: {error}",
                path.display()
            )
        })?;
        if !verified.file_type().is_symlink() || object_identity != file_object_identity(&verified)
        {
            return Err(format!(
                "created alias {} changed while recording ownership; preserving foreign data",
                path.display()
            ));
        }
        Ok(Self {
            path,
            object_identity,
            target,
        })
    }

    fn assert_unchanged(&self) -> Result<(), String> {
        match Self::snapshot(self.path.clone()) {
            Ok(current)
                if current.object_identity == self.object_identity && current.target == self.target =>
            {
                Ok(())
            }
            Ok(_) => Err(format!(
                "created alias {} no longer matches this transaction's symlink state; preserving foreign data",
                self.path.display()
            )),
            Err(error) => Err(format!(
                "cannot prove created alias {} is still transaction-owned ({error}); preserving foreign data",
                self.path.display()
            )),
        }
    }

    fn remove_if_unchanged(self) -> Result<(), String> {
        self.assert_unchanged()?;
        std::fs::remove_file(&self.path).map_err(|error| {
            format!(
                "remove transaction-owned alias {}: {error}",
                self.path.display()
            )
        })
    }
}

#[derive(Clone, Debug)]
struct OwnedDirectory {
    path: PathBuf,
    object_identity: FileObjectIdentity,
}

impl OwnedDirectory {
    fn snapshot(path: PathBuf) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect created directory {}: {error}", path.display()))?;
        if !metadata.is_dir() {
            return Err(format!(
                "created directory {} is no longer a directory; preserving foreign data",
                path.display()
            ));
        }
        Ok(Self {
            path,
            object_identity: file_object_identity(&metadata),
        })
    }

    fn remove_if_empty_and_unchanged(self) -> Result<(), String> {
        let current = Self::snapshot(self.path.clone()).map_err(|error| {
            format!(
                "cannot prove created directory {} is still transaction-owned ({error}); preserving foreign data",
                self.path.display()
            )
        })?;
        if current.object_identity != self.object_identity {
            return Err(format!(
                "created directory {} changed identity; preserving foreign data",
                self.path.display()
            ));
        }
        let mut entries = std::fs::read_dir(&self.path).map_err(|error| {
            format!("inspect created directory {}: {error}", self.path.display())
        })?;
        if entries.next().is_some() {
            return Err(format!(
                "created directory {} is no longer empty; preserving foreign data",
                self.path.display()
            ));
        }
        match std::fs::remove_dir(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove transaction-owned directory {}: {error}",
                self.path.display()
            )),
        }
    }
}

fn regular_file_identity(path: &std::path::Path) -> Result<FileObjectIdentity, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    Ok(file_object_identity(&metadata))
}

fn sqlite_owned_paths(db_path: &std::path::Path) -> Vec<PathBuf> {
    let mut wal = db_path.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = db_path.as_os_str().to_os_string();
    shm.push("-shm");
    let mut journal = db_path.as_os_str().to_os_string();
    journal.push("-journal");
    let mut marker = db_path.as_os_str().to_os_string();
    marker.push(".migration-marker");
    let mut paths = vec![
        PathBuf::from(wal),
        PathBuf::from(shm),
        PathBuf::from(journal),
        PathBuf::from(marker),
        db_path.to_path_buf(),
    ];
    if db_path.file_name().and_then(|name| name.to_str()) == Some(memcore::MEMORY_DB_FILENAME) {
        paths.push(db_path.with_file_name(memcore::LEGACY_MEMORY_DB_FILENAME));
    }
    paths
}

/// Prove that every existing identity source for `project_name` points to the
/// intended repo-local DB before any store is opened. Returns `true` when the
/// intended DB is already registered and usable, `false` only for genuine
/// absence. Manifest errors, broken/divergent aliases, and standalone-store
/// collisions are all fatal.
fn preflight_project_identity(
    db_path: &std::path::Path,
    project_root: &std::path::Path,
    project_name: &str,
    tachi_home: &std::path::Path,
) -> Result<bool, String> {
    let resolved =
        MemoryServer::resolve_existing_named_project_db_path_in_home(project_name, tachi_home)?;
    let already_resolved = resolved.is_some();
    let alias_exists =
        match crate::path_utils::inspect_plan_c_alias_in_home(db_path, project_root, tachi_home) {
            crate::path_utils::PlanCAliasInspection::Absent => false,
            crate::path_utils::PlanCAliasInspection::MatchingSymlink => true,
            crate::path_utils::PlanCAliasInspection::SplitBrain(issue) => {
                return Err(issue.warning_message());
            }
            crate::path_utils::PlanCAliasInspection::Integrity(issue) => {
                return Err(issue.warning_message());
            }
        };
    let has_existing_evidence = resolved.is_some() || alias_exists;
    if !has_existing_evidence {
        return Ok(false);
    }
    if !crate::path_utils::canonical_db_leaf_exists_without_symlink(db_path)? {
        return Err(format!(
            "project identity '{project_name}' already has an alias or registered DB, but intended repo DB {} does not exist; refusing ownership guess",
            db_path.display()
        ));
    }
    let expected = std::fs::canonicalize(db_path).map_err(|err| {
        format!(
            "intended project DB cannot be canonicalized at {}: {err}",
            db_path.display()
        )
    })?;
    if let Some(existing) = resolved {
        let existing = std::fs::canonicalize(&existing).map_err(|err| {
            format!(
                "registered project '{project_name}' cannot be canonicalized at {}: {err}",
                existing.display()
            )
        })?;
        if existing != expected {
            return Err(format!(
                "project identity '{project_name}' resolves to unrelated DB {}; expected {}",
                existing.display(),
                expected.display()
            ));
        }
    }
    Ok(already_resolved)
}

pub(crate) async fn handle_tachi_init_project_db(
    server: &MemoryServer,
    params: InitProjectDbParams,
) -> Result<String, String> {
    let _registration = workspace_registration_mutex()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // #1120 PR2: an omitted `project_root` used to fall back to
    // `find_git_root()` — the SERVER process's own cwd. For a shared daemon
    // (any HTTP direct-connect or auto-spawned stdio-proxy daemon) that cwd is
    // unrelated to the calling session's actual workspace (for `--daemon`
    // mode specifically it is the detached `${app_home}/runtime`, which is
    // never a git repo at all) — silently resolving "the wrong repo's
    // project DB, or none" from it was exactly the caller-cwd-blindness
    // #1120 exists to close (kckylechen1/tachi#1120 gap #1). `project_root`
    // is now always required; the actionable alternative for a bound
    // HTTP/stdio session is to skip this tool entirely and let
    // `X-Tachi-Workspace-Root` auto-register the project DB at session init
    // instead (#1120 PR1, `MemoryServer::resolve_or_register_workspace_root`).
    let Some(raw_project_root) = params.project_root.as_deref() else {
        return Err(
            "project_root is required — tachi_init_project_db no longer falls back to the \
             server process's own cwd (that cwd belongs to the DAEMON, not the calling session, \
             and silently resolving the wrong repo's project DB from it was the exact routing \
             bug #1120 exists to close). Pass project_root explicitly, or for a bound HTTP/stdio \
             session skip this tool entirely: declaring X-Tachi-Workspace-Root at session init \
             now auto-registers the project DB for you."
                .to_string(),
        );
    };
    let project_root = server.resolve_workspace_root(raw_project_root)?;

    // Derive and validate identity before creating a directory, opening a DB,
    // or activating a store. An unusable root must fail without leaving any
    // persistent state behind.
    let project_name = crate::path_utils::plan_c_dir_name_from_root(&project_root)
        .ok_or_else(|| "project root has no usable directory identity".to_string())?;

    let rel = PathBuf::from(&params.db_relpath);
    let db_path = crate::path_utils::resolve_project_db_path(&project_root, &rel)?;
    let existed = crate::path_utils::canonical_db_leaf_exists_without_symlink(&db_path)?;
    let mut precommit = ProjectDbPrecommit::new(db_path.clone());
    preflight_project_identity(
        &db_path,
        &project_root,
        &project_name,
        &server.tachi_home_dir(),
    )?;
    if let Err(error) = precommit.reserve_db() {
        return Err(precommit.abort(error));
    }
    if let Err(error) =
        precommit.ensure_alias(&project_root, &project_name, &server.tachi_home_dir())
    {
        return Err(precommit.abort(error));
    }
    // Manifest registration remains rollback-capable until hot activation
    // succeeds. Activation is the final project-state mutation.
    let activation = register_repo_local_manifest_entry_then_in_home(
        &db_path,
        &project_name,
        &server.tachi_home_dir(),
        || {
            crate::path_utils::canonical_db_leaf_exists_without_symlink(&db_path)?;
            precommit.assert_owned_db_artifacts_unchanged()?;
            let activation = server.activate_project_db(db_path.clone());
            if precommit.created_db() {
                precommit.finish_open_attempt(activation)
            } else {
                activation
            }
        },
    );
    let was_new_activation = match activation {
        Ok(value) => value,
        Err(error) => return Err(precommit.abort(error)),
    };
    precommit.commit();

    let plan_c_note = if cfg!(unix) {
        Some(format!(
            "Global symlink: {} -> {}",
            crate::path_utils::plan_c_global_db_path_in_home(
                &server.tachi_home_dir(),
                &project_name,
            )
            .display(),
            db_path.display()
        ))
    } else {
        Some("Plan C global symlink skipped on non-Unix hosts; use db_path directly.".to_string())
    };

    let activation_note = if was_new_activation {
        "Project DB is now active on this server instance. No restart needed."
    } else {
        "Project DB was already active; re-opened with latest state."
    };
    let note = match plan_c_note {
        Some(plan_c) => format!("{activation_note} {plan_c}"),
        None => activation_note.to_string(),
    };

    let plan_c_split_brain = match crate::path_utils::inspect_plan_c_alias_in_home(
        &db_path,
        &project_root,
        &server.tachi_home_dir(),
    ) {
        crate::path_utils::PlanCAliasInspection::SplitBrain(issue) => Some(issue),
        crate::path_utils::PlanCAliasInspection::Absent
        | crate::path_utils::PlanCAliasInspection::MatchingSymlink
        | crate::path_utils::PlanCAliasInspection::Integrity(_) => None,
    };
    Ok(serde_json::to_string(&json!({
        "initialized": true,
        "created": !existed,
        "active": true,
        "hot_activated": was_new_activation,
        "project_root": project_root.display().to_string(),
        "project": project_name,
        "db_path": db_path.display().to_string(),
        "db_relpath": rel.display().to_string(),
        "plan_c_split_brain": plan_c_split_brain,
        "note": note,
    }))
    .expect("serializing a serde_json::Value cannot fail"))
}

#[cfg(test)]
#[path = "project_db_ops/tests.rs"]
mod resolve_or_register_workspace_root_tests;
