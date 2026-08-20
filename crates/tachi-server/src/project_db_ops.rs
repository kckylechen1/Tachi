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
    /// #1120 PR1: resolve an `X-Tachi-Workspace-Root` (or
    /// `_meta.tachiWorkspaceRoot`) declaration to the project identity name
    /// [`MemoryServer::resolve_named_project_db_path`] will later resolve,
    /// auto-registering the project DB (creating `<git_root>/.tachi/tachi-memory.db`
    /// plus the Plan C alias symlink) on first contact instead of requiring a
    /// prior explicit `tachi_init_project_db` call.
    ///
    /// `raw_root` is resolved against the **daemon's own filesystem** — only
    /// meaningful when the client and daemon share one (today's stdio-proxy ->
    /// localhost-daemon topology satisfies this; a client talking to a remote
    /// daemon must bind by name with `X-Tachi-Project` instead). It must be an
    /// absolute path: a relative path would silently resolve against the
    /// daemon process's own cwd (which for `--daemon` mode is the detached
    /// `${app_home}/runtime`, never the caller's actual workspace — exactly
    /// the caller-cwd-blindness #1120 exists to close), not the caller's.
    ///
    /// Deliberately does **not** call [`MemoryServer::activate_project_db`]:
    /// that swaps the daemon's single static "default project" slot, which a
    /// multi-tenant daemon (many concurrently bound sessions, each a
    /// different project) cannot share — clobbering it from one session's own
    /// `initialize` would silently misroute every OTHER unbound/implicit-scope
    /// caller, the same silent-cross-project-fallback disease class #1120
    /// exists to close. Named-project routing (`with_named_project_store*` /
    /// `resolve_named_project_db_path`) opens by path per call and does not
    /// depend on that slot at all. Fresh DBs are initialized through the
    /// reversible precommit below; preexisting DBs retain `with_path_store`
    /// so the server's configured migration authority remains authoritative.
    pub(crate) fn resolve_or_register_workspace_root(
        &self,
        raw_root: &str,
    ) -> Result<String, String> {
        let candidate = PathBuf::from(raw_root);
        if !candidate.is_absolute() {
            return Err(format!(
                "workspace root '{raw_root}' must be an absolute path — a relative path would \
                 resolve against the DAEMON's own cwd, not the caller's workspace"
            ));
        }
        if !candidate.exists() {
            return Err(format!(
                "workspace root '{raw_root}' does not exist on the daemon host. \
                 X-Tachi-Workspace-Root is resolved against the DAEMON's own filesystem \
                 (only meaningful when client and daemon share one); for a remote daemon, \
                 bind by name instead with X-Tachi-Project against an already-registered project"
            ));
        }
        let git_root = find_git_root_from(&candidate).ok_or_else(|| {
            format!(
                "workspace root '{raw_root}' is not inside a git repository (no .git found \
                 searching upward from it). Pass the git repository root or a path beneath it, \
                 or bind by name instead with X-Tachi-Project against an already-registered \
                 project"
            )
        })?;
        let project_name =
            crate::path_utils::plan_c_dir_name_from_root(&git_root).ok_or_else(|| {
                format!(
                    "workspace root '{raw_root}' resolved to git root '{}' but it has no usable \
                     directory name to derive a project identity from",
                    git_root.display()
                )
            })?;

        // Route through the same canonical containment guard
        // `handle_tachi_init_project_db` uses below
        // (`path_utils::alias::resolve_project_db_path`) instead of a bare
        // `git_root.join(...)`: a repo-controlled `.tachi` symlink must not be
        // able to redirect DB creation outside `git_root`. The guard
        // canonicalizes the resolved parent directory and rejects anything
        // that escapes `git_root` after symlink resolution (review finding
        // [1], #1207).
        let workspace_root_db_relpath = workspace_root_db_relpath();
        let db_path = crate::path_utils::resolve_project_db_path(
            &git_root,
            std::path::Path::new(&workspace_root_db_relpath),
        )
        .map_err(|e| {
            format!(
                "workspace root '{raw_root}' resolved to git root '{}' but its \
                 {workspace_root_db_relpath} path is unsafe: {e}",
                git_root.display()
            )
        })?;
        crate::path_utils::canonical_db_leaf_exists_without_symlink(&db_path)?;
        // Registration may continue only when identity lookup proves genuine
        // absence. A successful lookup must resolve to this exact repo-local
        // DB; ambiguity, manifest failure, or a same-name standalone store is
        // an error, never a reason to create/open another DB.
        let mut precommit = ProjectDbPrecommit::new(db_path.clone());
        let already_resolved =
            preflight_project_identity(&db_path, &git_root, &project_name, &self.tachi_home_dir())?;
        if !already_resolved {
            if let Err(error) = precommit.reserve_db() {
                return Err(precommit.abort(error));
            }
        }
        if let Err(error) = precommit.ensure_alias(&git_root, &project_name, &self.tachi_home_dir())
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

    let mut changed = false;
    let target = matching
        .first()
        .copied()
        .or_else(|| stale_globals.first().copied());
    if let Some(target) = target {
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
            changed = true;
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
            changed = true;
        }
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
    }

    let target = target.expect("global target established above");
    let mut remove = matching
        .into_iter()
        .filter(|index| *index != target)
        .collect::<Vec<_>>();
    remove.extend(
        stale_globals
            .into_iter()
            .filter(|index| *index != target),
    );
    remove.sort_unstable();
    remove.dedup();
    for index in remove.into_iter().rev() {
        manifest.dbs.remove(index);
        changed = true;
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
            parent
                .join("global")
                .join(memcore::MEMORY_DB_FILENAME),
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
    let project_root = PathBuf::from(raw_project_root);

    if !project_root.join(".git").exists() {
        return Err(format!(
            "Target project root '{}' is not a git repository",
            project_root.display()
        ));
    }

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
mod resolve_or_register_workspace_root_tests {
    use super::*;
    use crate::test_support::EnvRestore;

    /// Isolated `TACHI_HOME` + a repo-local-DB-fixture-safe base dir (outside
    /// `/tmp`/`/private/tmp` — see `test_support::non_skipped_fixture_tempdir`'s
    /// doc comment: production manifest logic intentionally treats
    /// `.tachi/memory.db` under those roots as a disposable fixture, which
    /// would make some of these assertions about real registration flaky).
    fn with_test_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("workspace-root-");
        let tachi_home = fixture.path().join("home");
        std::fs::create_dir_all(&tachi_home).expect("tachi home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        f(fixture.path())
    }

    fn make_server(fixture_root: &std::path::Path) -> MemoryServer {
        let global_db = fixture_root.join("home/global/memory.db");
        MemoryServer::new(global_db, None).expect("construct isolated server")
    }

    #[cfg(unix)]
    fn install_permission_denied_symlink_hook() -> crate::path_utils::PlanCSymlinkHookGuard {
        crate::path_utils::install_plan_c_symlink_hook_for_test(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected Plan C symlink permission denial",
            ))
        })
    }

    #[cfg(unix)]
    fn file_identity(path: &std::path::Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::symlink_metadata(path).expect("path metadata");
        (metadata.dev(), metadata.ino())
    }

    #[cfg(unix)]
    fn replace_file_atomically(path: &std::path::Path, bytes: &[u8]) -> (u64, u64) {
        let replacement = path.with_extension("foreign-replacement");
        std::fs::write(&replacement, bytes).expect("write foreign replacement");
        std::fs::rename(&replacement, path).expect("replace file atomically");
        file_identity(path)
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_rollback_race_foreign_db_after_preflight_is_refused() {
        with_test_home(|root| {
            let repo = root.join("Foreign-After-Preflight-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            let project =
                crate::path_utils::plan_c_dir_name_from_root(&repo).expect("project identity");
            let mut precommit = ProjectDbPrecommit::new(db_path.clone());

            assert!(
                !preflight_project_identity(
                    &db_path,
                    &repo,
                    &project,
                    &crate::path_utils::tachi_home(),
                )
                .expect("preflight genuine absence"),
                "fixture must reach the post-preflight reservation race"
            );
            std::fs::create_dir_all(db_path.parent().expect("DB parent")).expect("DB parent");
            std::fs::write(&db_path, b"foreign database after preflight")
                .expect("foreign DB appears after preflight");
            let foreign_identity = file_identity(&db_path);

            let reservation = precommit
                .reserve_db()
                .expect_err("a DB that appears after preflight must not be adopted");
            let error = precommit.abort(reservation);

            assert!(error.contains("appeared after preflight"), "{error}");
            assert_eq!(
                std::fs::read(&db_path).expect("foreign DB preserved"),
                b"foreign database after preflight"
            );
            assert_eq!(file_identity(&db_path), foreign_identity);
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_rollback_race_foreign_db_replacement_is_preserved() {
        with_test_home(|root| {
            let db_path = root.join("Replacement-Repo/.tachi/tachi-memory.db");
            let mut precommit = ProjectDbPrecommit::new(db_path.clone());
            precommit.reserve_db().expect("reserve transaction DB");
            let reserved_identity = file_identity(&db_path);
            let foreign_identity = replace_file_atomically(&db_path, b"foreign replacement");
            assert_ne!(
                reserved_identity, foreign_identity,
                "replacement must change inode"
            );

            let error = precommit.abort("injected failure after foreign replacement".to_string());

            assert!(error.contains("rollback also reported"), "{error}");
            assert_eq!(
                std::fs::read(&db_path).expect("foreign replacement preserved"),
                b"foreign replacement"
            );
            assert_eq!(file_identity(&db_path), foreign_identity);
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_open_failure_removes_owned_sqlite_state() {
        with_test_home(|root| {
            let db_path = root.join("Open-Failure-Repo/.tachi/tachi-memory.db");
            let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
            let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
            let mut precommit = ProjectDbPrecommit::new(db_path.clone());
            precommit.reserve_db().expect("reserve transaction DB");

            let failure = precommit
                .run_open_attempt(|path| {
                    let store = memcore::MemoryStore::open(path.to_str().expect("UTF-8 DB path"))
                        .expect("SQLite mutates the reserved DB before the injected failure");
                    drop(store);
                    std::fs::write(&wal_path, b"transaction WAL").expect("inject WAL residue");
                    std::fs::write(&shm_path, b"transaction SHM").expect("inject SHM residue");
                    Err::<(), _>("injected failure after SQLite mutation".to_string())
                })
                .expect_err("injected open failure");
            let error = precommit.abort(failure);

            assert_eq!(error, "injected failure after SQLite mutation", "{error}");
            assert!(!db_path.exists(), "owned DB residue must be removed");
            assert!(!wal_path.exists(), "owned WAL residue must be removed");
            assert!(!shm_path.exists(), "owned SHM residue must be removed");
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_open_failure_preserves_foreign_db_replacement() {
        with_test_home(|root| {
            let db_path = root.join("Open-Failure-Replaced-Repo/.tachi/tachi-memory.db");
            let mut precommit = ProjectDbPrecommit::new(db_path.clone());
            precommit.reserve_db().expect("reserve transaction DB");
            let reserved_identity = file_identity(&db_path);

            let mut foreign_identity = None;
            let failure = precommit
                .run_open_attempt(|path| {
                    let store = memcore::MemoryStore::open(path.to_str().expect("UTF-8 DB path"))
                        .expect("SQLite mutates the reserved DB before the injected failure");
                    drop(store);
                    foreign_identity = Some(replace_file_atomically(
                        path,
                        b"foreign replacement during failed SQLite open",
                    ));
                    Err::<(), _>("injected failure after foreign replacement".to_string())
                })
                .expect_err("injected open failure");
            let foreign_identity = foreign_identity.expect("foreign inode");
            assert_ne!(reserved_identity, foreign_identity, "replacement inode");

            let error = precommit.abort(failure);

            assert!(error.contains("rollback also reported"), "{error}");
            assert_eq!(
                std::fs::read(&db_path).expect("foreign DB preserved"),
                b"foreign replacement during failed SQLite open"
            );
            assert_eq!(file_identity(&db_path), foreign_identity);
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_rollback_race_manifest_replacement_preserves_preexisting_manifest() {
        with_test_home(|root| {
            let db_path = root.join("Manifest-Race-Repo/.tachi/tachi-memory.db");
            std::fs::create_dir_all(db_path.parent().expect("DB parent")).expect("DB parent");
            std::fs::write(&db_path, b"reserved DB").expect("DB");
            let manifest = root.join("manifest.json");
            crate::manifest::Manifest::empty()
                .save(&manifest)
                .expect("seed manifest preimage");
            let preimage = std::fs::read(&manifest).expect("manifest preimage");
            let mut foreign_identity = None;

            let error = register_repo_local_manifest_entry_then_in_home(
                &db_path,
                "ManifestRace",
                root,
                || {
                    foreign_identity = Some(replace_file_atomically(
                        &manifest,
                        b"foreign manifest replacement",
                    ));
                    Err::<(), _>("injected post-registration failure".to_string())
                },
            )
            .expect_err("foreign manifest replacement must make rollback loud");

            assert!(error.contains("manifest rollback"), "{error}");
            assert_eq!(
                std::fs::read(&manifest).expect("foreign manifest preserved"),
                b"foreign manifest replacement"
            );
            assert_ne!(std::fs::read(&manifest).unwrap(), preimage);
            assert_eq!(
                file_identity(&manifest),
                foreign_identity.expect("foreign inode")
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_rollback_race_manifest_replacement_preserves_new_manifest() {
        with_test_home(|root| {
            let db_path = root.join("New-Manifest-Race-Repo/.tachi/tachi-memory.db");
            std::fs::create_dir_all(db_path.parent().expect("DB parent")).expect("DB parent");
            std::fs::write(&db_path, b"reserved DB").expect("DB");
            let manifest = root.join("manifest.json");
            let mut foreign_identity = None;

            let error = register_repo_local_manifest_entry_then_in_home(
                &db_path,
                "NewManifestRace",
                root,
                || {
                    foreign_identity = Some(replace_file_atomically(
                        &manifest,
                        b"foreign manifest replacement",
                    ));
                    Err::<(), _>("injected post-registration failure".to_string())
                },
            )
            .expect_err("foreign manifest replacement must make rollback loud");

            assert!(error.contains("manifest rollback"), "{error}");
            assert_eq!(
                std::fs::read(&manifest).expect("foreign manifest preserved"),
                b"foreign manifest replacement"
            );
            assert_eq!(
                file_identity(&manifest),
                foreign_identity.expect("foreign inode")
            );
        });
    }

    #[test]
    fn rejects_relative_path() {
        with_test_home(|root| {
            let server = make_server(root);
            let err = server
                .resolve_or_register_workspace_root("relative/workspace/root")
                .expect_err("a relative path must be rejected");
            assert!(
                err.contains("absolute path"),
                "expected an absolute-path error, got: {err}"
            );
        });
    }

    #[test]
    fn rejects_nonexistent_path() {
        with_test_home(|root| {
            let server = make_server(root);
            let missing = root.join("does-not-exist-anywhere");
            let err = server
                .resolve_or_register_workspace_root(&missing.display().to_string())
                .expect_err("a path that does not exist on the daemon host must be rejected");
            assert!(
                err.contains("does not exist"),
                "expected a does-not-exist error, got: {err}"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_auto_permission_failure_leaves_no_project_state() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Auto-Permission-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            let manifest = crate::path_utils::tachi_home().join("manifest.json");
            let _hook = install_permission_denied_symlink_hook();

            let error = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("symlink permission failure must refuse auto-registration");

            assert!(error.contains("permission denial"), "{error}");
            assert!(!db_path.exists(), "failed precommit must remove its DB");
            assert!(
                !repo.join(".tachi").exists(),
                "failed precommit must remove its DB parent"
            );
            assert!(
                !manifest.exists(),
                "failed precommit must not leave a manifest"
            );
            assert_eq!(server.project_db_path_buf(), None);
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_auto_eexist_wrong_target_is_rollback_safe_and_retry_refuses() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Auto-Wrong-Target-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            let manifest = crate::path_utils::tachi_home().join("manifest.json");
            let user_target = root.join("user-owned.db");
            std::fs::write(&user_target, b"user-owned").expect("user target");
            let hook_target = user_target.clone();
            let _hook = crate::path_utils::install_plan_c_symlink_hook_for_test(move |alias| {
                std::os::unix::fs::symlink(&hook_target, alias)
            });

            let first = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("racing wrong-target alias must refuse auto-registration");
            assert!(first.contains("Plan C alias integrity failure"), "{first}");
            assert!(!db_path.exists(), "failed precommit must remove its DB");
            assert!(!repo.join(".tachi").exists(), "DB parent must not remain");
            assert!(!manifest.exists(), "manifest must not remain");
            assert_eq!(std::fs::read(&user_target).unwrap(), b"user-owned");
            drop(_hook);

            let retry = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("persistent wrong-target alias must refuse retry");
            assert!(retry.contains("Plan C alias integrity failure"), "{retry}");
            assert!(!db_path.exists());
            assert!(!manifest.exists());
            assert_eq!(std::fs::read(&user_target).unwrap(), b"user-owned");
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_auto_accepts_matching_concurrent_symlink() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Auto-Matching-Race-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            let hook_db = db_path.clone();
            let _hook = crate::path_utils::install_plan_c_symlink_hook_for_test(move |alias| {
                std::os::unix::fs::symlink(&hook_db, alias)
            });

            let project = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("matching concurrent alias must be accepted");

            assert!(db_path.exists());
            assert!(crate::path_utils::plan_c_global_db_path(&project).is_symlink());
            assert!(crate::path_utils::tachi_home()
                .join("manifest.json")
                .exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn alias_precommit_auto_already_resolved_still_requires_alias_confirmation() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Auto-Resolved-Alias-Gate-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            let project = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("initial registration");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            let alias = crate::path_utils::plan_c_global_db_path(&project);
            let manifest = crate::path_utils::tachi_home().join("manifest.json");
            let db_before = std::fs::read(&db_path).expect("DB preimage");
            let manifest_before = std::fs::read(&manifest).expect("manifest preimage");
            std::fs::remove_file(&alias).expect("remove alias to exercise recreation gate");
            let _hook = install_permission_denied_symlink_hook();

            let error = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("resolved project must not bypass alias confirmation");

            assert!(error.contains("permission denial"), "{error}");
            assert_eq!(std::fs::read(&db_path).unwrap(), db_before);
            assert_eq!(std::fs::read(&manifest).unwrap(), manifest_before);
            assert!(!alias.exists());
        });
    }

    #[test]
    fn concurrent_manifest_registrations_preserve_both_projects() {
        with_test_home(|root| {
            let alpha = root.join("Alpha/data/project.db");
            let beta = root.join("Beta/data/project.db");
            for db in [&alpha, &beta] {
                std::fs::create_dir_all(db.parent().unwrap()).expect("DB parent");
                std::fs::write(db, b"db").expect("DB");
            }
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
            let home = root.to_path_buf();
            let handles = [(alpha, "Alpha"), (beta, "Beta")]
                .into_iter()
                .map(|(db, project)| {
                    let barrier = std::sync::Arc::clone(&barrier);
                    let home = home.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        register_repo_local_manifest_entry_in_home(&db, project, &home)
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            for handle in handles {
                handle
                    .join()
                    .expect("registration thread")
                    .expect("register");
            }

            let manifest =
                crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
            let mut scopes = manifest
                .dbs
                .iter()
                .map(|entry| entry.scope_hint.as_str())
                .collect::<Vec<_>>();
            scopes.sort_unstable();
            assert_eq!(scopes, ["project:Alpha", "project:Beta"]);
        });
    }

    #[test]
    fn register_repo_local_manifest_entry_in_home_includes_existing_global_store() {
        with_test_home(|root| {
            let global_db = root.join("global").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("global parent");
            std::fs::write(&global_db, b"global_db").expect("global DB");

            let project_db = root.join("Alpha/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project_db").expect("project DB");

            register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
                .expect("register project db");

            let manifest =
                crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
            assert!(
                manifest.global().is_some(),
                "manifest must include the global store when present on disk"
            );
            assert_eq!(manifest.dbs.len(), 2);
            let roles: Vec<_> = manifest.dbs.iter().map(|e| e.role).collect();
            assert!(roles.contains(&crate::manifest::DbRole::Global));
            assert!(roles.contains(&crate::manifest::DbRole::Project));
        });
    }

    #[test]
    fn register_repo_local_manifest_entry_in_home_includes_root_global_store() {
        with_test_home(|root| {
            let global_db = root.join(memcore::MEMORY_DB_FILENAME);
            std::fs::write(&global_db, b"global_db").expect("root global DB");

            let project_db = root.join("Alpha/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project_db").expect("project DB");

            register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
                .expect("register project db");

            let manifest =
                crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
            let global = manifest.global().expect("root global store must be registered");
            assert_eq!(
                std::path::PathBuf::from(&global.path),
                std::fs::canonicalize(&global_db).expect("canonical root global DB")
            );
            assert_eq!(manifest.dbs.len(), 2);
        });
    }

    #[test]
    fn register_repo_local_manifest_entry_rejects_ambiguous_global_candidates() {
        with_test_home(|root| {
            let root_global = root.join(memcore::MEMORY_DB_FILENAME);
            std::fs::write(&root_global, b"root global DB").expect("root global DB");
            let nested_global = root
                .join("global")
                .join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(nested_global.parent().unwrap()).expect("global parent");
            std::fs::write(&nested_global, b"nested global DB").expect("nested global DB");

            let project_db = root.join("Alpha/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project DB").expect("project DB");

            let manifest_path = root.join("manifest.json");
            crate::manifest::Manifest::empty()
                .save(&manifest_path)
                .expect("seed manifest");
            let error = register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
                .expect_err("distinct global candidates must fail closed");
            assert!(
                error.contains("ambiguous global store identity"),
                "unexpected ambiguity error: {error}"
            );

            let manifest =
                crate::manifest::Manifest::load(&manifest_path).expect("manifest remains valid");
            assert!(
                manifest.global().is_none(),
                "ambiguous candidates must not write global authority"
            );
            assert!(
                manifest.dbs.is_empty(),
                "ambiguous candidates must fail before project registration mutation"
            );
        });
    }

    #[test]
    fn register_repo_local_manifest_entry_replaces_stale_global_authority() {
        with_test_home(|root| {
            let global_db = root.join(memcore::MEMORY_DB_FILENAME);
            std::fs::write(&global_db, b"live global DB").expect("live global DB");
            let stale_path = root.join("stale/missing-global.db");

            let mut manifest = crate::manifest::Manifest::empty();
            manifest.dbs.push(crate::manifest::DbEntry {
                path: stale_path.display().to_string(),
                role: crate::manifest::DbRole::Global,
                owner: "legacy-owner".to_string(),
                schema_kind: "legacy".to_string(),
                vec_enabled: false,
                allow_write: false,
                last_doctor_at: "stale-at".to_string(),
                last_classification: "corrupt".to_string(),
                scope_hint: "global".to_string(),
                notes: "stale metadata".to_string(),
            });
            let manifest_path = root.join("manifest.json");
            manifest.save(&manifest_path).expect("seed stale manifest");

            let project_db = root.join("Alpha/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project DB").expect("project DB");

            register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
                .expect("stale global authority should be replaceable");

            let manifest =
                crate::manifest::Manifest::load(&manifest_path).expect("manifest remains valid");
            let global = manifest.global().expect("one live global authority");
            assert_eq!(
                std::path::PathBuf::from(&global.path),
                std::fs::canonicalize(&global_db).expect("canonical live global DB")
            );
            assert_eq!(global.owner, "tachi");
            assert_eq!(global.schema_kind, "tachi");
            assert!(global.vec_enabled);
            assert!(global.allow_write);
            assert_eq!(global.last_classification, "healthy");
            assert_ne!(global.last_doctor_at, "stale-at");
            assert_eq!(global.notes, "auto-registered global store");
            assert_eq!(
                manifest
                    .dbs
                    .iter()
                    .filter(|entry| entry.role == crate::manifest::DbRole::Global)
                    .count(),
                1
            );
        });
    }

    #[test]
    fn register_repo_local_manifest_entry_rejects_live_distinct_global_authority() {
        with_test_home(|root| {
            let global_db = root.join(memcore::MEMORY_DB_FILENAME);
            std::fs::write(&global_db, b"candidate global DB").expect("candidate global DB");
            let existing_global = root.join("existing-global.db");
            std::fs::write(&existing_global, b"existing global DB")
                .expect("existing global DB");

            let mut manifest = crate::manifest::Manifest::empty();
            manifest.dbs.push(crate::manifest::DbEntry {
                path: existing_global.display().to_string(),
                role: crate::manifest::DbRole::Global,
                owner: "tachi".to_string(),
                schema_kind: "tachi".to_string(),
                vec_enabled: true,
                allow_write: true,
                last_doctor_at: String::new(),
                last_classification: "healthy".to_string(),
                scope_hint: "global".to_string(),
                notes: String::new(),
            });
            let manifest_path = root.join("manifest.json");
            manifest.save(&manifest_path).expect("seed live manifest");
            let before = std::fs::read(&manifest_path).expect("manifest bytes");

            let project_db = root.join("Alpha/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project DB").expect("project DB");

            let error = register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
                .expect_err("live distinct global authority must fail closed");
            assert!(
                error.contains("global manifest authority conflict"),
                "unexpected conflict error: {error}"
            );
            assert_eq!(
                before,
                std::fs::read(&manifest_path).expect("manifest remains unchanged")
            );
        });
    }

    #[test]
    fn register_repo_local_manifest_entry_in_home_ignores_repo_local_global_lookalike() {
        with_test_home(|root| {
            let lookalike = root
                .join("repo/.tachi/global")
                .join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(lookalike.parent().unwrap()).expect("lookalike parent");
            std::fs::write(&lookalike, b"repo-local global lookalike")
                .expect("repo-local lookalike DB");

            let project_db = root.join("Alpha/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project_db").expect("project DB");

            register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
                .expect("register project db");

            let manifest =
                crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
            assert!(
                manifest.global().is_none(),
                "a repo-local .tachi/global lookalike is not the configured global store"
            );
            assert_eq!(manifest.dbs.len(), 1);
        });
    }

    #[test]
    fn re_registering_existing_project_backfills_global_store() {
        with_test_home(|root| {
            let project_db = root.join("Alpha/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project_db").expect("project DB");

            // First registration without global store present
            register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
                .expect("register project db");
            let m1 =
                crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
            assert_eq!(m1.dbs.len(), 1);
            assert!(m1.global().is_none());

            // Create global store on disk
            let global_db = root.join("global").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("global parent");
            std::fs::write(&global_db, b"global_db").expect("global DB");

            // Re-register the same project DB
            register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
                .expect("re-register project db");

            let m2 =
                crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
            assert!(
                m2.global().is_some(),
                "re-registering project must backfill global store when present"
            );
            assert_eq!(m2.dbs.len(), 2);
        });
    }

    #[test]
    fn re_registering_repairs_drifted_global_role_without_duplication() {
        with_test_home(|root| {
            let global_db = root.join("global").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("global parent");
            std::fs::write(&global_db, b"global_db").expect("global DB");

            let canon_global = std::fs::canonicalize(&global_db).unwrap();
            let mut manifest = crate::manifest::Manifest::empty();
            manifest.dbs.push(crate::manifest::DbEntry {
                path: canon_global.display().to_string(),
                role: crate::manifest::DbRole::Unknown,
                owner: "legacy-owner".to_string(),
                schema_kind: "legacy".to_string(),
                vec_enabled: false,
                allow_write: false,
                last_doctor_at: "stale-at".to_string(),
                last_classification: "corrupt".to_string(),
                scope_hint: "legacy_global".to_string(),
                notes: "stale metadata".to_string(),
            });
            manifest.save(&root.join("manifest.json")).expect("save");

            let project_db = root.join("Beta/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project_db").expect("project DB");

            register_repo_local_manifest_entry_in_home(&project_db, "Beta", root)
                .expect("register project db");

            let reloaded =
                crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
            assert_eq!(
                reloaded.dbs.len(),
                2,
                "must not duplicate the global store entry"
            );
            assert!(reloaded.global().is_some());
            assert_eq!(reloaded.global().unwrap().scope_hint, "global");
            let global = reloaded.global().unwrap();
            assert_eq!(global.owner, "tachi");
            assert_eq!(global.schema_kind, "tachi");
            assert!(global.vec_enabled);
            assert!(global.allow_write);
            assert_eq!(global.last_classification, "healthy");
            assert_ne!(global.last_doctor_at, "stale-at");
            assert_eq!(global.notes, "auto-registered global store");
            assert_eq!(
                reloaded
                    .dbs
                    .iter()
                    .filter(|entry| entry.role == crate::manifest::DbRole::Global)
                    .count(),
                1
            );
        });
    }

    #[test]
    #[cfg(unix)]
    fn re_registering_preserves_symlinked_global_store_without_duplication() {
        with_test_home(|root| {
            let real_global = root.join("external/real_memory.db");
            std::fs::create_dir_all(real_global.parent().unwrap()).expect("real parent");
            std::fs::write(&real_global, b"global_db").expect("real global DB");

            let symlink_global = root.join("global").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(symlink_global.parent().unwrap()).expect("symlink parent");
            std::os::unix::fs::symlink(&real_global, &symlink_global).expect("symlink");

            let mut manifest = crate::manifest::Manifest::empty();
            manifest.dbs.push(crate::manifest::DbEntry {
                path: symlink_global.display().to_string(),
                role: crate::manifest::DbRole::Global,
                owner: "tachi".to_string(),
                schema_kind: "tachi".to_string(),
                vec_enabled: true,
                allow_write: true,
                last_doctor_at: String::new(),
                last_classification: "healthy".to_string(),
                scope_hint: "global".to_string(),
                notes: String::new(),
            });
            manifest.save(&root.join("manifest.json")).expect("save");

            let project_db = root.join("Gamma/data/project.db");
            std::fs::create_dir_all(project_db.parent().unwrap()).expect("project parent");
            std::fs::write(&project_db, b"project_db").expect("project DB");

            register_repo_local_manifest_entry_in_home(&project_db, "Gamma", root)
                .expect("register project db");

            let reloaded =
                crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
            assert_eq!(
                reloaded.dbs.len(),
                2,
                "must not duplicate the symlinked global store entry"
            );
            assert!(reloaded.global().is_some());
        });
    }

    #[test]
    fn rejects_path_outside_any_git_repo() {
        with_test_home(|root| {
            let server = make_server(root);
            let plain_dir = tempfile::tempdir().expect("plain dir");
            assert!(
                find_git_root_from(plain_dir.path()).is_none(),
                "plain_dir fixture must not be inside a git repository: {}",
                plain_dir.path().display()
            );
            let err = server
                .resolve_or_register_workspace_root(&plain_dir.path().display().to_string())
                .expect_err("a directory outside any git repo must be rejected");
            assert!(
                err.contains("not inside a git repository"),
                "expected a not-a-git-repo error, got: {err}"
            );
        });
    }

    #[test]
    fn rejects_path_outside_git_repo_when_cargo_target_dir_is_in_a_git_repo() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let sandbox = tempfile::tempdir().expect("sandbox");
        let repo = sandbox.path().join("fixture-repo");
        let in_repo_target = repo.join("target");
        std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
        std::fs::create_dir_all(&in_repo_target).expect("in-repo target");
        let _cargo_target = EnvRestore::set_path("CARGO_TARGET_DIR", &in_repo_target);

        let fixture = crate::test_support::non_skipped_fixture_tempdir("workspace-root-");
        let tachi_home = fixture.path().join("home");
        std::fs::create_dir_all(&tachi_home).expect("tachi home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let server = make_server(fixture.path());
        let plain_dir = fixture.path().join("not-a-git-repo");
        std::fs::create_dir_all(&plain_dir).expect("plain dir");
        let err = server
            .resolve_or_register_workspace_root(&plain_dir.display().to_string())
            .expect_err("a fixture path beneath an in-repo target must remain outside git");
        assert!(
            err.contains("not inside a git repository"),
            "expected a not-a-git-repo error, got: {err}"
        );
    }

    /// Review finding [1] (#1207): a repo-controlled `.tachi` symlink must
    /// not be able to redirect DB creation outside the git root. Route
    /// through the same canonical containment guard
    /// (`resolve_project_db_path`) `handle_tachi_init_project_db` already
    /// uses — it canonicalizes the resolved parent and rejects anything
    /// that escapes the git root after symlink resolution.
    #[cfg(unix)]
    #[test]
    fn rejects_a_tachi_symlink_that_escapes_the_git_root() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Symlink-Escape-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");

            // A repo-controlled `.tachi` symlink pointing OUTSIDE the git
            // root (e.g. committed by an attacker, or left over from a prior
            // untrusted checkout) must not redirect DB creation there.
            let escape_target = root.join("outside-the-repo");
            std::fs::create_dir_all(&escape_target).expect("escape target dir");
            std::os::unix::fs::symlink(&escape_target, repo.join(".tachi"))
                .expect("plant escaping .tachi symlink");

            let err = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("a `.tachi` symlink escaping the git root must be rejected");
            assert!(
                err.contains("escapes") || err.contains("unsafe"),
                "expected a containment-guard rejection, got: {err}"
            );
            assert!(
                !escape_target.join(memcore::MEMORY_DB_FILENAME).exists(),
                "no DB file may be created outside the git root via the symlink"
            );
        });
    }

    #[cfg(unix)]
    fn assert_canonical_db_symlink_refusal_has_no_registration_side_effects(
        repo: &std::path::Path,
        db_path: &std::path::Path,
        expected_link_target: &std::path::Path,
        manifest_before: Option<&[u8]>,
    ) {
        let project = crate::path_utils::plan_c_dir_name_from_root(repo).expect("project identity");
        let alias = crate::path_utils::plan_c_global_db_path(&project);
        let manifest = crate::path_utils::tachi_home().join("manifest.json");

        let metadata = std::fs::symlink_metadata(db_path).expect("canonical DB symlink preserved");
        assert!(
            metadata.file_type().is_symlink(),
            "canonical DB path must remain the original symlink"
        );
        assert_eq!(
            std::fs::read_link(db_path).expect("canonical DB symlink target"),
            expected_link_target
        );
        assert_eq!(
            std::fs::read(&manifest).ok().as_deref(),
            manifest_before,
            "refusal must preserve the manifest preimage"
        );
        assert!(
            std::fs::symlink_metadata(&alias).is_err(),
            "refusal must not create the separate Plan C alias"
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_db_final_component_dangling_symlink_is_refused_without_side_effects() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Canonical-Dangling-Repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            let external_target = root.join("external/dangling-target.db");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            std::fs::create_dir_all(db_path.parent().expect("DB parent")).expect("DB parent");
            std::os::unix::fs::symlink(&external_target, &db_path)
                .expect("plant dangling canonical DB symlink");
            let manifest = crate::path_utils::tachi_home().join("manifest.json");
            let manifest_before = std::fs::read(&manifest).ok();
            let symlink_identity = file_identity(&db_path);

            let error = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("canonical DB symlink must be rejected before reservation");

            assert!(
                error.contains("canonical repo DB path") && error.contains("must not be a symlink"),
                "expected canonical DB symlink refusal, got: {error}"
            );
            assert_eq!(file_identity(&db_path), symlink_identity);
            assert!(
                std::fs::symlink_metadata(&external_target).is_err(),
                "dangling external target must not be created"
            );
            assert_canonical_db_symlink_refusal_has_no_registration_side_effects(
                &repo,
                &db_path,
                &external_target,
                manifest_before.as_deref(),
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn canonical_db_final_component_wrong_target_symlink_is_refused_without_side_effects() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Canonical-Wrong-Target-Repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            let external_target = root.join("external/wrong-target.db");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            std::fs::create_dir_all(db_path.parent().expect("DB parent")).expect("DB parent");
            std::fs::create_dir_all(external_target.parent().expect("external parent"))
                .expect("external parent");
            std::fs::write(&external_target, b"foreign external database").expect("external DB");
            std::os::unix::fs::symlink(&external_target, &db_path)
                .expect("plant wrong-target canonical DB symlink");
            let manifest = crate::path_utils::tachi_home().join("manifest.json");
            let manifest_before = std::fs::read(&manifest).ok();
            let symlink_identity = file_identity(&db_path);
            let external_identity = file_identity(&external_target);

            let error = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("canonical DB symlink must be rejected before open");

            assert!(
                error.contains("canonical repo DB path") && error.contains("must not be a symlink"),
                "expected canonical DB symlink refusal, got: {error}"
            );
            assert_eq!(file_identity(&db_path), symlink_identity);
            assert_eq!(file_identity(&external_target), external_identity);
            assert_eq!(
                std::fs::read(&external_target).expect("external DB preserved"),
                b"foreign external database"
            );
            assert_canonical_db_symlink_refusal_has_no_registration_side_effects(
                &repo,
                &db_path,
                &external_target,
                manifest_before.as_deref(),
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn canonical_db_final_component_symlink_loop_is_refused_without_side_effects() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Canonical-Loop-Repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            std::fs::create_dir_all(db_path.parent().expect("DB parent")).expect("DB parent");
            std::os::unix::fs::symlink(&db_path, &db_path)
                .expect("plant looped canonical DB symlink");
            let manifest = crate::path_utils::tachi_home().join("manifest.json");
            let manifest_before = std::fs::read(&manifest).ok();
            let symlink_identity = file_identity(&db_path);

            let error = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("canonical DB symlink loop must be rejected before reservation");

            assert!(
                error.contains("canonical repo DB path") && error.contains("must not be a symlink"),
                "expected canonical DB symlink refusal, got: {error}"
            );
            assert_eq!(file_identity(&db_path), symlink_identity);
            assert_canonical_db_symlink_refusal_has_no_registration_side_effects(
                &repo,
                &db_path,
                &db_path,
                manifest_before.as_deref(),
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn canonical_db_final_component_normal_absence_registers_regular_db_and_plan_c_alias() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Canonical-Absent-Repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            assert!(
                std::fs::symlink_metadata(&db_path).is_err(),
                "canonical DB path starts genuinely absent"
            );

            let project = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("normal absent canonical DB path registers");

            let db_metadata = std::fs::symlink_metadata(&db_path).expect("created canonical DB");
            assert!(db_metadata.file_type().is_file());
            assert!(!db_metadata.file_type().is_symlink());
            let alias = crate::path_utils::plan_c_global_db_path(&project);
            assert!(
                std::fs::symlink_metadata(&alias)
                    .expect("managed Plan C alias")
                    .file_type()
                    .is_symlink(),
                "the separate documented Plan C alias remains valid"
            );
            assert_eq!(
                std::fs::canonicalize(&alias).expect("resolve Plan C alias"),
                std::fs::canonicalize(&db_path).expect("resolve canonical DB")
            );
            let manifest = crate::manifest::Manifest::load(
                &crate::path_utils::tachi_home().join("manifest.json"),
            )
            .expect("registration manifest");
            assert!(
                manifest
                    .dbs
                    .iter()
                    .any(|entry| entry.path == db_path.display().to_string()),
                "normal registration records the canonical regular DB"
            );
        });
    }

    /// Review finding [2] (#1207): auto-registration must not return a
    /// project name that later becomes unreachable once the Plan C symlink
    /// (Unix-only, best-effort) is gone — e.g. it was never created at all on
    /// a non-Unix host. The manifest entry `register_repo_local_manifest_entry_in_home`
    /// writes is the primary, symlink-independent addressing path.
    #[test]
    fn project_remains_resolvable_after_the_plan_c_symlink_is_broken() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Symlink-Independent-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");

            let name = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("auto-registration succeeds");

            // Simulate a platform where the Plan C symlink either never
            // existed (non-Unix `ensure_plan_c_symlink` is a `Skipped`
            // no-op) or was later broken, without touching the manifest
            // entry.
            let alias_name =
                crate::path_utils::plan_c_dir_name_from_root(&repo).expect("alias name");
            let alias_db = crate::path_utils::plan_c_global_db_path(&alias_name);
            let _ = std::fs::remove_file(&alias_db);
            assert!(!alias_db.exists(), "test precondition: alias severed");

            let resolved = MemoryServer::resolve_named_project_db_path(&name).expect(
                "manifest-recorded repo-local path must resolve the project even with no \
                 working Plan C symlink",
            );
            assert_eq!(
                std::fs::canonicalize(&resolved).expect("canonicalize resolved"),
                std::fs::canonicalize(repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME))
                    .expect("canonicalize expected"),
            );
        });
    }

    /// Core #1120 PR1 behavior: a workspace root the daemon has never seen
    /// before is auto-registered on first contact — the DB file is created on
    /// disk and immediately resolvable by the derived project name, with no
    /// prior explicit `tachi_init_project_db` call.
    #[test]
    fn auto_registers_new_project_db_on_first_contact() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Fresh-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");

            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            crate::test_support::assert_repo_local_db_fixture_not_skipped(&db_path);
            assert!(
                !db_path.exists(),
                "test precondition: db must not pre-exist"
            );

            let name = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("auto-registration must succeed for a never-seen git repo");

            assert!(db_path.exists(), "project db must be created on disk");
            let resolved = MemoryServer::resolve_named_project_db_path(&name)
                .expect("the just-auto-registered project must resolve by its derived name");
            assert_eq!(
                std::fs::canonicalize(&resolved).expect("canonicalize resolved"),
                std::fs::canonicalize(&db_path).expect("canonicalize db_path"),
                "resolved path must be the repo-local DB auto-registration just created"
            );
        });
    }

    #[test]
    fn auto_registered_fresh_project_survives_strict_named_read_open() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Fresh-Read-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            let db_path = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(db_path.parent().expect("project db parent"))
                .expect("create project db parent");
            std::fs::File::create(&db_path).expect("reserve empty project db path");
            let name = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("auto-register fresh named project from reserved empty db");
            drop(server);

            let reopened = make_server(root);
            let trigger_count = reopened
                .with_named_project_store_read(&name, |store| {
                    store
                        .connection()
                        .query_row(
                            "SELECT count(*) FROM main.sqlite_schema
                             WHERE type = 'trigger'
                               AND name IN (
                                   'memories_reserved_refs_insert_guard',
                                   'memories_reserved_refs_update_guard'
                               )",
                            [],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(|error| error.to_string())
                })
                .expect("strict read-open of the newly registered named project");
            assert_eq!(trigger_count, 2, "fresh named-project guard inventory");
        });
    }

    /// A caller may declare a subdirectory of the repo (its own cwd, which is
    /// rarely the repo root) — the daemon must walk up to the git root the
    /// same way `find_git_root_from` already does for every other caller.
    #[test]
    fn resolves_from_a_subdirectory_of_the_repo() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Nested-Repo");
            let nested = repo.join("src/inner");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
            std::fs::create_dir_all(&nested).expect("nested dir");

            let name = server
                .resolve_or_register_workspace_root(&nested.display().to_string())
                .expect("a subdirectory of a git repo must resolve to the repo root's project");

            let resolved = MemoryServer::resolve_named_project_db_path(&name)
                .expect("registered project resolves");
            assert_eq!(
                std::fs::canonicalize(&resolved).expect("canonicalize resolved"),
                std::fs::canonicalize(repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME))
                    .expect("canonicalize expected"),
            );
        });
    }

    /// A second `initialize` from the same workspace root (e.g. a second
    /// session opened in the same repo) must be a cheap already-registered
    /// hit, not a second create attempt that could error or disturb the DB.
    #[test]
    fn second_contact_from_the_same_root_is_idempotent() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Idempotent-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");

            let first = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("first contact registers");
            let second = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("second contact must not error");
            assert_eq!(
                first, second,
                "the same workspace root must resolve to the same project name every time"
            );
        });
    }

    /// #1120 PR1 precedence regression (integration-level twin of
    /// `server_handler`'s pure `project_binding_source` unit tests): an
    /// explicit named project's resolution codepath
    /// (`resolve_named_project_db_path`) must never be shadowed by workspace-
    /// root auto-registration touching the same alias name for an unrelated
    /// root — i.e. auto-registration must key strictly off the *resolved git
    /// root*, not off caller-controlled input that could collide.
    #[test]
    fn distinct_repos_register_distinct_projects() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo_a = root.join("workspace-a/Shared-Basename");
            let repo_b = root.join("workspace-b/Shared-Basename");
            std::fs::create_dir_all(repo_a.join(".git")).expect("repo a");
            std::fs::create_dir_all(repo_b.join(".git")).expect("repo b");

            let name_a = server
                .resolve_or_register_workspace_root(&repo_a.display().to_string())
                .expect("register repo a");
            let name_b = server
                .resolve_or_register_workspace_root(&repo_b.display().to_string())
                .expect("register repo b");

            assert_ne!(
                name_a, name_b,
                "two distinct repos sharing a basename must not collide on one project identity"
            );
            let resolved_a =
                MemoryServer::resolve_named_project_db_path(&name_a).expect("repo a resolves");
            let resolved_b =
                MemoryServer::resolve_named_project_db_path(&name_b).expect("repo b resolves");
            assert_ne!(
                resolved_a, resolved_b,
                "each name must resolve to its own DB file"
            );
        });
    }

    #[test]
    fn workspace_registration_refuses_same_identity_standalone_store_before_db_open() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Collision-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("repo");
            let identity = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("identity");
            let standalone = crate::path_utils::plan_c_global_db_path(&identity);
            std::fs::create_dir_all(standalone.parent().unwrap()).expect("alias parent");
            std::fs::write(&standalone, b"standalone").expect("standalone DB");
            let local_db = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);

            let error = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("standalone store must not be claimed as this workspace");
            assert!(error.contains("refusing ownership guess"), "{error}");
            assert!(!local_db.exists(), "no repo DB may be opened or created");
            assert_eq!(std::fs::read(standalone).unwrap(), b"standalone");
        });
    }

    #[test]
    fn workspace_registration_refuses_ambiguous_alias_identity_before_db_open() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Ambiguous-Alias-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("repo");
            let current =
                crate::path_utils::plan_c_dir_name_from_root(&repo).expect("current identity");
            let previous = crate::path_utils::plan_c_previous_dir_name_from_root(&repo)
                .expect("previous identity");
            assert_ne!(
                current, previous,
                "fixture needs distinct alias generations"
            );
            for (name, contents) in [
                (&current, b"current".as_slice()),
                (&previous, b"previous".as_slice()),
            ] {
                let alias = crate::path_utils::plan_c_global_db_path(name);
                std::fs::create_dir_all(alias.parent().unwrap()).expect("alias parent");
                std::fs::write(alias, contents).expect("divergent alias DB");
            }
            let local_db = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);

            let error = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect_err("ambiguous aliases must refuse auto-registration");
            assert!(error.contains("ambiguous"), "{error}");
            assert!(
                !local_db.exists(),
                "identity refusal must occur before repo-local DB creation"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn previous_hash_alias_is_migrated_to_restart_stable_canonical_registration() {
        with_test_home(|root| {
            let server = make_server(root);
            let repo = root.join("Legacy-Repo");
            std::fs::create_dir_all(repo.join(".git")).expect("repo");
            let local_db = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("DB parent");
            memcore::MemoryStore::open(local_db.to_str().unwrap()).expect("local DB");
            let previous = crate::path_utils::plan_c_previous_dir_name_from_root(&repo)
                .expect("previous identity");
            let previous_alias = crate::path_utils::plan_c_global_db_path(&previous);
            std::fs::create_dir_all(previous_alias.parent().unwrap()).expect("alias parent");
            std::os::unix::fs::symlink(&local_db, &previous_alias).expect("previous alias");

            let current = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("compatibility alias must migrate without orphaning the DB");
            assert_ne!(current, previous);
            let resolved = MemoryServer::resolve_named_project_db_path(&current)
                .expect("canonical identity must resolve after migration");
            assert_eq!(
                std::fs::canonicalize(resolved).unwrap(),
                std::fs::canonicalize(local_db).unwrap()
            );
        });
    }

    #[test]
    fn non_ascii_projects_register_distinct_stable_identities_across_restart() {
        with_test_home(|root| {
            let repo_a = root.join("workspace/量化");
            let repo_b = root.join("workspace/研究");
            std::fs::create_dir_all(repo_a.join(".git")).expect("repo a");
            std::fs::create_dir_all(repo_b.join(".git")).expect("repo b");

            let first_server = make_server(root);
            let name_a = first_server
                .resolve_or_register_workspace_root(&repo_a.display().to_string())
                .expect("register repo a");
            let name_b = first_server
                .resolve_or_register_workspace_root(&repo_b.display().to_string())
                .expect("register repo b");
            assert!(name_a.starts_with("project-"), "{name_a}");
            assert!(name_b.starts_with("project-"), "{name_b}");
            assert_ne!(name_a, name_b);
            drop(first_server);

            let restarted = make_server(root);
            for (name, repo) in [(&name_a, &repo_a), (&name_b, &repo_b)] {
                let resolved = MemoryServer::resolve_named_project_db_path(name)
                    .expect("canonical identity resolves after restart");
                assert_eq!(
                    std::fs::canonicalize(resolved).unwrap(),
                    std::fs::canonicalize(repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME))
                        .unwrap()
                );
            }
            drop(restarted);
        });
    }
}
