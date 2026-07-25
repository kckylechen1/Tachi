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
    /// depend on that slot at all — `with_path_store` below is enough to force
    /// the DB file (and its schema) into existence.
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
        // Registration may continue only when identity lookup proves genuine
        // absence. A successful lookup must resolve to this exact repo-local
        // DB; ambiguity, manifest failure, or a same-name standalone store is
        // an error, never a reason to create/open another DB.
        if preflight_project_identity(&db_path, &git_root, &project_name)? {
            return Ok(project_name);
        }
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "create project db parent directory at {}: {e}",
                    parent.display()
                )
            })?;
        }
        // Force the DB file (and its schema) into existence via the same
        // per-path attach cache every named-project call goes through — see
        // the doc comment above for why this, and not `activate_project_db`.
        self.with_path_store(&db_path, |_store| Ok(()))
            .map_err(|e| format!("initialize project db at {}: {e}", db_path.display()))?;

        // Primary registration: write a manifest entry so
        // `resolve_named_project_db_path` can find this DB by name
        // independent of the Plan C symlink below — the manifest-recorded
        // repo-local path is the addressing scheme's documented preferred
        // path (`server_methods/db.rs::resolve_named_project_db_path`'s doc
        // comment), and unlike the symlink it works on every platform (review
        // finding [2], #1207: `ensure_plan_c_symlink` is a no-op `Skipped` on
        // non-Unix hosts, so a project registered only via the symlink could
        // never be reopened there).
        register_repo_local_manifest_entry(&db_path, &project_name)?;

        // Secondary/legacy addressing: the `~/.tachi/projects/<name>/`
        // symlink alias. `ensure_plan_c_symlink` is a no-op `Skipped` on
        // non-Unix hosts (see its own cfg-gated definitions in
        // `path_utils/symlink.rs`) — safe to call unconditionally here,
        // unlike `handle_tachi_init_project_db` below, which surfaces the
        // platform split in its caller-facing note.
        match crate::path_utils::ensure_plan_c_symlink(&db_path, &git_root) {
            crate::path_utils::PlanCLinkOutcome::Failed { path, error } => {
                tracing::warn!(
                    target: "tachi::project_db::auto_register",
                    path = %path.display(),
                    error = %error,
                    project = %project_name,
                    "workspace-root auto-registration created the project DB but the Plan C \
                     alias symlink failed; the project remains reachable by its repo-local path"
                );
            }
            crate::path_utils::PlanCLinkOutcome::SplitBrain(issue) => {
                tracing::warn!(
                    target: "tachi::project_db::auto_register",
                    project = %project_name,
                    "{}",
                    issue.warning_message()
                );
            }
            // Exhaustive on purpose (no `_` catch-all): a future new
            // `PlanCLinkOutcome` variant must force a deliberate decision
            // here about whether it needs its own warning, not silently fall
            // into "nothing to log" the way a wildcard arm would.
            crate::path_utils::PlanCLinkOutcome::AlreadyLinked
            | crate::path_utils::PlanCLinkOutcome::Created(_)
            | crate::path_utils::PlanCLinkOutcome::Skipped(_) => {}
        }

        // The whole point of auto-registration is a project name the caller
        // can immediately reopen (review finding [2], #1207: "initialization
        // returns a project name that cannot be reopened" is a bug). Verify
        // reachability through the exact resolver every subsequent
        // named-project call uses, and fail loudly instead of returning a
        // name that silently cannot be reopened (e.g. the manifest write
        // above also failed for some reason on top of a non-Unix/no-symlink
        // host).
        Self::resolve_named_project_db_path(&project_name).map_err(|err| {
            format!(
                "project db was created at {} but is not resolvable by its derived name \
                 '{project_name}': {err}",
                db_path.display()
            )
        })?;

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
pub(crate) fn register_repo_local_manifest_entry(
    db_path: &std::path::Path,
    project_name: &str,
) -> Result<(), String> {
    let manifest_path = crate::path_utils::tachi_home().join("manifest.json");
    let _process_guard = manifest_registration_mutex()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    with_manifest_registration_file_lock(&manifest_path, || {
        register_repo_local_manifest_entry_locked(db_path, project_name, &manifest_path)
    })
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

    let canonical = std::fs::canonicalize(db_path).unwrap_or_else(|_| db_path.to_path_buf());
    let canon_str = canonical.display().to_string();
    if let Some(entry) = manifest.dbs.iter_mut().find(|e| e.path == canon_str) {
        // A physical canonical DB path is unambiguous authority. Refresh only
        // its derived identity label; never move or auto-claim an alias path.
        let canonical_scope = format!("project:{project_name}");
        if entry.scope_hint == canonical_scope {
            return Ok(());
        }
        entry.scope_hint = canonical_scope;
        manifest.generated_at = chrono::Utc::now().to_rfc3339();
        return manifest
            .save(manifest_path)
            .map_err(|e| format!("save manifest {}: {e}", manifest_path.display()));
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

/// Prove that every existing identity source for `project_name` points to the
/// intended repo-local DB before any store is opened. Returns `true` when the
/// intended DB is already registered and usable, `false` only for genuine
/// absence. Manifest errors, broken/divergent aliases, and standalone-store
/// collisions are all fatal.
fn preflight_project_identity(
    db_path: &std::path::Path,
    project_root: &std::path::Path,
    project_name: &str,
) -> Result<bool, String> {
    let resolved = MemoryServer::resolve_existing_named_project_db_path(project_name)?;
    let already_resolved = resolved.is_some();
    let alias = crate::path_utils::plan_c_alias_db_for_root(project_root)?;
    let alias_exists = std::fs::symlink_metadata(&alias).is_ok();
    let has_existing_evidence = resolved.is_some() || alias_exists;
    if !has_existing_evidence {
        return Ok(false);
    }
    if !db_path.exists() {
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
    if alias_exists {
        let alias_identity = std::fs::canonicalize(&alias).map_err(|err| {
            format!(
                "project alias cannot be canonicalized at {}: {err}",
                alias.display()
            )
        })?;
        if alias_identity != expected {
            return Err(format!(
                "project identity '{project_name}' has divergent alias {}; expected {}",
                alias.display(),
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
    let existed = db_path.exists();
    preflight_project_identity(&db_path, &project_root, &project_name)?;
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create project db dir: {e}"))?;
    }

    // Hot-activate the project DB on the running server (no restart needed)
    let was_new_activation = server.activate_project_db(db_path.clone())?;

    register_repo_local_manifest_entry(&db_path, &project_name)?;

    let mut plan_c_note: Option<String> = None;
    if let Some(safe_name) = crate::path_utils::plan_c_dir_name_from_root(&project_root) {
        let global_link = crate::path_utils::plan_c_global_db_path(&safe_name);
        #[cfg(unix)]
        {
            match crate::path_utils::ensure_plan_c_symlink(&db_path, &project_root) {
                crate::path_utils::PlanCLinkOutcome::SplitBrain(issue) => {
                    plan_c_note = Some(issue.warning_message());
                }
                crate::path_utils::PlanCLinkOutcome::Failed { path, error } => {
                    plan_c_note = Some(format!(
                        "Plan C global symlink failed at {}: {}",
                        path.display(),
                        error
                    ));
                }
                _ => {
                    plan_c_note = Some(format!(
                        "Global symlink: {} -> {}",
                        global_link.display(),
                        db_path.display()
                    ));
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = safe_name;
            plan_c_note = Some(
                "Plan C global symlink skipped on non-Unix hosts; use db_path directly."
                    .to_string(),
            );
        }
    }

    let activation_note = if was_new_activation {
        "Project DB is now active on this server instance. No restart needed."
    } else {
        "Project DB was already active; re-opened with latest state."
    };
    let note = match plan_c_note {
        Some(plan_c) => format!("{activation_note} {plan_c}"),
        None => activation_note.to_string(),
    };

    serde_json::to_string(&json!({
        "initialized": true,
        "created": !existed,
        "active": true,
        "hot_activated": was_new_activation,
        "project_root": project_root.display().to_string(),
        "project": project_name,
        "db_path": db_path.display().to_string(),
        "db_relpath": rel.display().to_string(),
        "plan_c_split_brain": crate::path_utils::plan_c_split_brain(&db_path, &project_root),
        "note": note,
    }))
    .map_err(|e| format!("serialize: {e}"))
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
            let handles = [(alpha, "Alpha"), (beta, "Beta")]
                .into_iter()
                .map(|(db, project)| {
                    let barrier = std::sync::Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        register_repo_local_manifest_entry(&db, project)
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

            let manifest = crate::manifest::Manifest::load(
                &crate::path_utils::tachi_home().join("manifest.json"),
            )
            .expect("manifest");
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

    /// Review finding [2] (#1207): auto-registration must not return a
    /// project name that later becomes unreachable once the Plan C symlink
    /// (Unix-only, best-effort) is gone — e.g. it was never created at all on
    /// a non-Unix host. The manifest entry `register_repo_local_manifest_entry`
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
            let name = server
                .resolve_or_register_workspace_root(&repo.display().to_string())
                .expect("auto-register fresh named project");
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
