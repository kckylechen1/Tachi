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

        let error =
            register_repo_local_manifest_entry_then_in_home(&db_path, "ManifestRace", root, || {
                foreign_identity = Some(replace_file_atomically(
                    &manifest,
                    b"foreign manifest replacement",
                ));
                Err::<(), _>("injected post-registration failure".to_string())
            })
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
        let global = manifest
            .global()
            .expect("root global store must be registered");
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
        let nested_global = root.join("global").join(memcore::MEMORY_DB_FILENAME);
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
            aliases: Vec::new(),
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
        std::fs::write(&existing_global, b"existing global DB").expect("existing global DB");

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
            aliases: Vec::new(),
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
        let m1 = crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
        assert_eq!(m1.dbs.len(), 1);
        assert!(m1.global().is_none());

        // Create global store on disk
        let global_db = root.join("global").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("global parent");
        std::fs::write(&global_db, b"global_db").expect("global DB");

        // Re-register the same project DB
        register_repo_local_manifest_entry_in_home(&project_db, "Alpha", root)
            .expect("re-register project db");

        let m2 = crate::manifest::Manifest::load(&root.join("manifest.json")).expect("manifest");
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
            aliases: Vec::new(),
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
            aliases: Vec::new(),
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
fn registers_plain_directory_without_a_git_repository() {
    with_test_home(|root| {
        let server = make_server(root);
        let workspace = root.join("Research-Notes");
        std::fs::create_dir_all(&workspace).unwrap();
        assert!(find_git_root_from(&workspace).is_none());
        let project = server
            .resolve_or_register_workspace_root(&workspace.display().to_string())
            .expect("ordinary folders can own memory");
        let db = workspace.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        assert_eq!(
            server
                .resolve_server_named_project_db_path(&project)
                .unwrap(),
            db.canonicalize().unwrap()
        );
        assert!(db.is_file());
        assert!(server.project_db_path_buf().is_none());
    });
}

#[test]
fn ordinary_workspace_subdirectory_reuses_registered_database() {
    with_test_home(|root| {
        let server = make_server(root);
        let workspace = root.join("Research-Notes");
        let nested = workspace.join("2026/november");
        std::fs::create_dir_all(&nested).unwrap();
        let project = server
            .resolve_or_register_workspace_root(&workspace.display().to_string())
            .unwrap();
        let db = workspace.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        let before = std::fs::read(&db).unwrap();
        let child_project = server
            .resolve_or_register_workspace_root(&nested.display().to_string())
            .unwrap();
        assert_eq!(child_project, project);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert!(!nested.join(".tachi").exists());
        assert!(server.project_db_path_buf().is_none());
    });
}

#[test]
fn config_only_parent_is_not_a_workspace_boundary() {
    with_test_home(|root| {
        let server = make_server(root);
        let parent = root.join("Documents");
        let workspace = parent.join("Research");
        std::fs::create_dir_all(parent.join(".tachi")).unwrap();
        std::fs::write(parent.join(".tachi/config.env"), "EXAMPLE=value").unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        server
            .resolve_or_register_workspace_root(&workspace.display().to_string())
            .unwrap();
        assert!(workspace
            .join(".tachi")
            .join(memcore::MEMORY_DB_FILENAME)
            .is_file());
        assert!(!parent
            .join(".tachi")
            .join(memcore::MEMORY_DB_FILENAME)
            .exists());
    });
}

#[test]
fn missing_registered_parent_database_does_not_create_a_child_workspace() {
    with_test_home(|root| {
        let server = make_server(root);
        let workspace = root.join("Research");
        let child = workspace.join("notes");
        std::fs::create_dir_all(&child).unwrap();
        server
            .resolve_or_register_workspace_root(&workspace.display().to_string())
            .unwrap();
        let db = workspace.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        std::fs::remove_file(&db).unwrap();
        let error = server
            .resolve_or_register_workspace_root(&child.display().to_string())
            .expect_err("missing authoritative database must not become absence");
        assert!(
            error.contains("missing") || error.contains("unreadable"),
            "{error}"
        );
        assert!(!db.exists());
        assert!(!child.join(".tachi").exists());
    });
}

#[test]
fn file_workspace_root_is_rejected_without_side_effects() {
    with_test_home(|root| {
        let server = make_server(root);
        let file = root.join("notes.txt");
        std::fs::write(&file, "notes").unwrap();
        let error = server
            .resolve_or_register_workspace_root(&file.display().to_string())
            .expect_err("workspace boundaries are directories");
        assert!(error.contains("must be a directory"), "{error}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "notes");
        assert!(!root.join(".tachi").exists());
    });
}

#[test]
fn concurrent_first_bindings_share_one_complete_database() {
    with_test_home(|root| {
        let server = make_server(root);
        let workspace = root.join("Concurrent-Research");
        std::fs::create_dir_all(&workspace).unwrap();
        let barrier = std::sync::Barrier::new(8);
        let projects = std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        server.resolve_or_register_workspace_root(&workspace.display().to_string())
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(projects.iter().all(|project| project == &projects[0]));
        let db = workspace.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        let store = memcore::MemoryStore::open(db.to_str().unwrap()).expect("complete database");
        drop(store);
        let manifest =
            crate::manifest::Manifest::load(&server.tachi_home_dir().join("manifest.json"))
                .unwrap();
        assert_eq!(
            manifest
                .dbs
                .iter()
                .filter(|entry| entry.scope_hint == format!("project:{}", projects[0]))
                .count(),
            1
        );
        assert!(server.project_db_path_buf().is_none());
    });
}

#[test]
fn plain_workspace_does_not_inherit_the_cargo_target_repository() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let sandbox = tempfile::tempdir().unwrap();
    let repo = sandbox.path().join("fixture-repo");
    let in_repo_target = repo.join("target");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(&in_repo_target).unwrap();
    let _cargo_target = EnvRestore::set_path("CARGO_TARGET_DIR", &in_repo_target);
    let fixture = crate::test_support::non_skipped_fixture_tempdir("workspace-root-");
    let tachi_home = fixture.path().join("home");
    std::fs::create_dir_all(&tachi_home).unwrap();
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    let server = make_server(fixture.path());
    let plain_dir = fixture.path().join("not-a-git-repo");
    std::fs::create_dir_all(&plain_dir).unwrap();
    server
        .resolve_or_register_workspace_root(&plain_dir.display().to_string())
        .unwrap();
    assert!(plain_dir
        .join(".tachi")
        .join(memcore::MEMORY_DB_FILENAME)
        .is_file());
    assert!(!repo.join(".tachi").exists());
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
        std::os::unix::fs::symlink(&db_path, &db_path).expect("plant looped canonical DB symlink");
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
        let manifest =
            crate::manifest::Manifest::load(&crate::path_utils::tachi_home().join("manifest.json"))
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
        let alias_name = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("alias name");
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

#[test]
fn fallback_workspace_resolution_is_read_only_and_reuses_subdirectory_boundary() {
    with_test_home(|root| {
        let server = make_server(root);
        let workspace = root.join("Read-Only-Research");
        let child = workspace.join("notes");
        std::fs::create_dir_all(&child).unwrap();
        let manifest = server.tachi_home_dir().join("manifest.json");
        let before = std::fs::read(&manifest).ok();
        server
            .resolve_existing_workspace_project(&workspace.display().to_string())
            .expect_err("fallback must not register an unregistered directory");
        assert!(!workspace.join(".tachi").exists());
        assert_eq!(std::fs::read(&manifest).ok(), before);
        let project = server
            .resolve_or_register_workspace_root(&workspace.display().to_string())
            .unwrap();
        let db = workspace.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        let db_before = std::fs::read(&db).unwrap();
        let manifest_before = std::fs::read(&manifest).unwrap();
        assert_eq!(
            server
                .resolve_existing_workspace_project(&child.display().to_string())
                .unwrap(),
            project
        );
        assert_eq!(std::fs::read(&db).unwrap(), db_before);
        assert_eq!(std::fs::read(&manifest).unwrap(), manifest_before);
        assert!(!child.join(".tachi").exists());
    });
}
