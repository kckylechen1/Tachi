use super::*;
use crate::test_support::EnvRestore;

#[cfg(unix)]
struct ClearOwnershipInject;
#[cfg(unix)]
impl Drop for ClearOwnershipInject {
    fn drop(&mut self) {
        crate::db_ownership::set_ownership_inject_for_test(None);
    }
}

fn startup_test_cli() -> Cli {
    Cli {
        daemon: false,
        port: 6919,
        global_db: None,
        project_db: None,
        no_project_db: false,
        allow_schema_migration: false,
        profile: None,
        gc_enabled: None,
        gc_initial_delay_secs: None,
        gc_interval_secs: None,
        command: Some(Commands::Serve),
    }
}

#[cfg(unix)]
#[test]
fn duplicate_daemon_refuses_before_creating_database() {
    let _environment = crate::utils::global_test_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _cwd = crate::test_support::CwdRestore::set(root.path());
    let app_home = root.path().join("tachi");
    let db = root.path().join("database/tachi-memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let _home = EnvRestore::set_path("HOME", root.path());
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _memory_db = EnvRestore::remove("MEMORY_DB_PATH");
    let _daemon = EnvRestore::remove("TACHI_DAEMON");
    let _profile = EnvRestore::remove(crate::host_profile::HOST_PROFILE_ENV);
    let _owner = crate::daemon_lock::DaemonLock::acquire(
        crate::daemon_lock::scoped_daemon_lock_path(&app_home, &db),
    )
    .unwrap();
    let mut cli = startup_test_cli();
    cli.daemon = true;
    cli.no_project_db = true;
    cli.global_db = Some(db.clone());
    let error = tokio_main(cli).unwrap_err();
    assert!(
        error.to_string().contains("singleton lock unavailable"),
        "{error}"
    );
    assert!(
        !db.exists(),
        "a losing daemon must not initialize the shared database"
    );
}

#[cfg(unix)]
#[test]
fn duplicate_daemon_refuses_before_copying_a_legacy_global_database() {
    let _environment = crate::utils::global_test_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _cwd = crate::test_support::CwdRestore::set(root.path());
    let app_home = root.path().join("tachi");
    let db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let legacy = app_home.join(memcore::LEGACY_MEMORY_DB_FILENAME);
    std::fs::write(&legacy, b"legacy database").unwrap();
    std::fs::write(sidecar_path(&legacy, "-wal"), b"legacy WAL").unwrap();
    let _home = EnvRestore::set_path("HOME", root.path());
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _memory_db = EnvRestore::remove("MEMORY_DB_PATH");
    let _daemon = EnvRestore::remove("TACHI_DAEMON");
    let _profile = EnvRestore::remove(crate::host_profile::HOST_PROFILE_ENV);
    let _owner = crate::daemon_lock::DaemonLock::acquire(
        crate::daemon_lock::scoped_daemon_lock_path(&app_home, &db),
    )
    .unwrap();
    let mut cli = startup_test_cli();
    cli.daemon = true;
    cli.no_project_db = true;
    let error = tokio_main(cli).unwrap_err();
    assert!(
        error.to_string().contains("singleton lock unavailable"),
        "{error}"
    );
    assert!(
        !db.exists(),
        "the losing daemon must not copy the legacy DB"
    );
    assert!(!sidecar_path(&db, "-wal").exists());
    assert_eq!(std::fs::read(legacy).unwrap(), b"legacy database");
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)] // The shared process environment stays isolated across startup.
async fn legacy_global_copy_requires_destination_ownership_and_preserves_existing_data() {
    let _environment = crate::utils::global_test_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let app_home = root.path().join("tachi");
    let _memory_db = EnvRestore::remove("MEMORY_DB_PATH");
    let ctx = StartupContext {
        home: root.path().to_path_buf(),
        app_home: app_home.clone(),
        command: Commands::Serve,
        defer_manifest_startup: true,
        git_root: None,
        schema_migration: memcore::MigrationAuthority::Deny,
    };
    let cli = startup_test_cli();
    let db = resolve_global_db(&cli, &ctx).await.unwrap();
    let legacy = app_home.join(memcore::LEGACY_MEMORY_DB_FILENAME);
    std::fs::write(&legacy, b"legacy database").unwrap();
    std::fs::write(sidecar_path(&legacy, "-wal"), b"legacy WAL").unwrap();
    let owner = daemon::acquire_startup_daemon_lock(&app_home, &db).unwrap();
    let error = migrate_legacy_global_db_if_needed(&cli, &ctx, &db, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("singleton lock unavailable"));
    assert!(!db.exists());
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));
    let _clear = ClearOwnershipInject;
    migrate_legacy_global_db_if_needed(&cli, &ctx, &db, Some(&owner))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&db).unwrap(), b"legacy database");
    assert_eq!(
        std::fs::read(sidecar_path(&db, "-wal")).unwrap(),
        b"legacy WAL"
    );
    std::fs::write(&db, b"new owner's data").unwrap();
    migrate_legacy_global_db_if_needed(&cli, &ctx, &db, Some(&owner))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&db).unwrap(), b"new owner's data");
}

#[cfg(not(unix))]
#[test]
fn platform_refusal_startup_write_modes_preserve_absent_and_existing_state() {
    let _env_lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for wiki in [false, true] {
        for existing in [false, true] {
            let root = tempfile::tempdir().expect("temporary root");
            let home = root.path().join("absent-home");
            let _tachi_home = EnvRestore::set_path("TACHI_HOME", &home);
            let db = root.path().join("global/memory.db");
            let project_db = root.path().join("project/memory.db");
            let output = root.path().join("output");
            if existing {
                std::fs::create_dir(db.parent().expect("DB parent")).expect("DB directory");
                std::fs::write(&db, b"existing database sentinel").expect("DB sentinel");
                std::fs::create_dir(&output).expect("output directory");
                std::fs::write(output.join("sentinel.md"), b"existing output")
                    .expect("output sentinel");
            }
            let mut cli = startup_test_cli();
            cli.daemon = !wiki;
            cli.global_db = Some(db.clone());
            cli.project_db = Some(project_db.clone());
            cli.allow_schema_migration = true;
            if wiki {
                cli.command = Some(Commands::Wiki {
                    action: tachi_bootstrap::cli::WikiAction::Export {
                        format: "obsidian".into(),
                        output: output.clone(),
                        project: "wiki".into(),
                    },
                });
            }
            let error = tokio_main(cli).expect_err("unsupported startup mode");
            assert!(error.to_string().contains("unsupported on this platform"));
            assert!(
                !home.exists(),
                "unsupported startup must not initialize its home"
            );
            assert!(!project_db.parent().expect("project parent").exists());
            if existing {
                assert_eq!(
                    std::fs::read(&db).expect("DB after"),
                    b"existing database sentinel"
                );
                assert_eq!(
                    std::fs::read(output.join("sentinel.md")).expect("output after"),
                    b"existing output"
                );
                assert_eq!(
                    std::fs::read_dir(&output).expect("output entries").count(),
                    1
                );
                assert_eq!(
                    std::fs::read_dir(db.parent().expect("DB parent"))
                        .expect("DB entries")
                        .count(),
                    1
                );
            } else {
                assert!(!db.parent().expect("DB parent").exists());
                assert!(!output.exists());
            }
        }
    }
}

#[test]
fn recall_coverage_cli_short_circuits_default_db_startup() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let coverage_db = dir.path().join("coverage.db");
    drop(
        MemoryStore::open(coverage_db.to_str().expect("utf8 coverage path"))
            .expect("create coverage DB"),
    );
    let default_db = dir.path().join("must-not-open-default.db");
    let mut cli = startup_test_cli();
    cli.global_db = Some(default_db.clone());
    cli.command = Some(Commands::RecallCoverage(Box::new(
        tachi_bootstrap::cli::RecallCoverageArgs {
            db: coverage_db,
            top_k: None,
            candidates_per_channel: None,
            limit: None,
            equivalence_file: None,
            human: false,
        },
    )));

    tokio_main(cli).expect("recall coverage must run through the early read-only route");
    assert!(
        !default_db.exists(),
        "recall coverage must not resolve or create a default database"
    );
}

fn startup_test_context(root: &Path, app_home: &Path) -> StartupContext {
    StartupContext {
        home: root.to_path_buf(),
        app_home: app_home.to_path_buf(),
        command: Commands::Serve,
        defer_manifest_startup: false,
        git_root: Some(root.to_path_buf()),
        schema_migration: memcore::MigrationAuthority::Deny,
    }
}

fn startup_distill_cli_and_context(
    root: &Path,
    app_home: &Path,
    db: PathBuf,
) -> (Cli, StartupContext) {
    use tachi_bootstrap::cli::DistillAction;

    let mut cli = startup_test_cli();
    cli.command = Some(Commands::Distill {
        action: DistillAction::Run {
            db: Some(db.clone()),
            no_consolidate: false,
        },
    });
    let mut ctx = startup_test_context(root, app_home);
    ctx.command = Commands::Distill {
        action: DistillAction::Run {
            db: Some(db),
            no_consolidate: false,
        },
    };
    (cli, ctx)
}

#[cfg(unix)]
fn startup_file_identity(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path).expect("path metadata");
    (metadata.dev(), metadata.ino())
}

#[cfg(unix)]
fn assert_startup_canonical_symlink_refusal_side_effects(
    root: &Path,
    app_home: &Path,
    canonical_db: &Path,
    expected_target: &Path,
    canonical_identity: (u64, u64),
    legacy_db: &Path,
    manifest_before: &[u8],
) {
    assert_eq!(startup_file_identity(canonical_db), canonical_identity);
    assert_eq!(
        std::fs::read_link(canonical_db).expect("canonical symlink preserved"),
        expected_target
    );
    assert_eq!(
        std::fs::read(legacy_db).expect("legacy DB preserved"),
        b"legacy-source"
    );
    assert_eq!(
        std::fs::read(app_home.join("manifest.json")).expect("manifest preserved"),
        manifest_before
    );
    let project = crate::path_utils::plan_c_dir_name_from_root(root).expect("project name");
    let alias = crate::path_utils::plan_c_global_db_path(&project);
    assert!(
        std::fs::symlink_metadata(alias).is_err(),
        "canonical refusal must precede Plan C alias creation"
    );
}

#[cfg(unix)]
fn assert_distill_override_refusal_side_effects(
    root: &Path,
    app_home: &Path,
    global_db: &Path,
    manifest_before: &[u8],
) {
    let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    assert!(
        std::fs::symlink_metadata(&canonical_db).is_err(),
        "override refusal must not create the auto project DB"
    );
    let project = crate::path_utils::plan_c_dir_name_from_root(root).expect("project name");
    let alias = crate::path_utils::plan_c_global_db_path(&project);
    assert!(
        std::fs::symlink_metadata(alias).is_err(),
        "override refusal must precede Plan C alias creation"
    );
    assert!(
        std::fs::symlink_metadata(global_db).is_err(),
        "override refusal must precede global DB open"
    );
    assert_eq!(
        std::fs::read(app_home.join("manifest.json")).expect("manifest preserved"),
        manifest_before
    );
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn distill_db_override_dangling_symlink_refuses_before_side_effects() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("distill-db-override-");
    let root = fixture.path().join("Dangling-Override-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    std::fs::create_dir_all(&app_home).expect("app home");
    let manifest_before = b"distill-manifest-preimage";
    std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
    let external_target = fixture.path().join("external/dangling.db");
    let override_db = fixture.path().join("override.db");
    std::os::unix::fs::symlink(&external_target, &override_db).expect("dangling override");
    let override_identity = startup_file_identity(&override_db);
    let (cli, ctx) = startup_distill_cli_and_context(&root, &app_home, override_db.clone());
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

    let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
        Err(error) => error,
        Ok(_) => panic!("dangling distill DB override must refuse startup"),
    };

    assert!(
        error.to_string().contains("canonical repo DB path")
            && error.to_string().contains("must not be a symlink"),
        "expected canonical leaf refusal, got: {error}"
    );
    assert_eq!(startup_file_identity(&override_db), override_identity);
    assert_eq!(std::fs::read_link(&override_db).unwrap(), external_target);
    assert!(std::fs::symlink_metadata(&external_target).is_err());
    assert_distill_override_refusal_side_effects(&root, &app_home, &global_db, manifest_before);
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn distill_db_override_wrong_symlink_refuses_without_external_mutation() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("distill-db-override-");
    let root = fixture.path().join("Wrong-Override-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    std::fs::create_dir_all(&app_home).expect("app home");
    let manifest_before = b"distill-manifest-preimage";
    std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
    let external_target = fixture.path().join("external/foreign.db");
    std::fs::create_dir_all(external_target.parent().unwrap()).expect("external parent");
    let store = memcore::MemoryStore::open(external_target.to_str().expect("UTF-8 DB"))
        .expect("seed foreign DB");
    drop(store);
    let external_identity = startup_file_identity(&external_target);
    let external_before = std::fs::read(&external_target).expect("foreign DB bytes");
    let override_db = fixture.path().join("override.db");
    std::os::unix::fs::symlink(&external_target, &override_db).expect("wrong override");
    let override_identity = startup_file_identity(&override_db);
    let (cli, ctx) = startup_distill_cli_and_context(&root, &app_home, override_db.clone());
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

    let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
        Err(error) => error,
        Ok(_) => panic!("wrong-target distill DB override must refuse startup"),
    };

    assert!(
        error.to_string().contains("canonical repo DB path")
            && error.to_string().contains("must not be a symlink"),
        "expected canonical leaf refusal, got: {error}"
    );
    assert_eq!(startup_file_identity(&override_db), override_identity);
    assert_eq!(std::fs::read_link(&override_db).unwrap(), external_target);
    assert_eq!(startup_file_identity(&external_target), external_identity);
    assert_eq!(std::fs::read(&external_target).unwrap(), external_before);
    assert_distill_override_refusal_side_effects(&root, &app_home, &global_db, manifest_before);
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn distill_db_override_symlink_loop_refuses_before_side_effects() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("distill-db-override-");
    let root = fixture.path().join("Loop-Override-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    std::fs::create_dir_all(&app_home).expect("app home");
    let manifest_before = b"distill-manifest-preimage";
    std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
    let override_db = fixture.path().join("override.db");
    std::os::unix::fs::symlink(&override_db, &override_db).expect("looped override");
    let override_identity = startup_file_identity(&override_db);
    let (cli, ctx) = startup_distill_cli_and_context(&root, &app_home, override_db.clone());
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

    let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
        Err(error) => error,
        Ok(_) => panic!("looped distill DB override must refuse startup"),
    };

    assert!(
        error.to_string().contains("canonical repo DB path")
            && error.to_string().contains("must not be a symlink"),
        "expected canonical leaf refusal, got: {error}"
    );
    assert_eq!(startup_file_identity(&override_db), override_identity);
    assert_eq!(std::fs::read_link(&override_db).unwrap(), override_db);
    assert_distill_override_refusal_side_effects(&root, &app_home, &global_db, manifest_before);
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn distill_db_override_regular_file_opens_normally() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("distill-db-override-");
    let root = fixture.path().join("Regular-Override-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    let override_db = fixture.path().join("override.db");
    let store = memcore::MemoryStore::open(override_db.to_str().expect("UTF-8 DB"))
        .expect("seed regular override DB");
    drop(store);
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(global_db.parent().unwrap()).expect("global parent");
    let global = memcore::MemoryStore::open(global_db.to_str().expect("UTF-8 global DB"))
        .expect("seed global DB");
    drop(global);
    let (cli, ctx) = startup_distill_cli_and_context(&root, &app_home, override_db.clone());

    let result = run_startup_hygiene(&cli, &ctx, &global_db)
        .await
        .expect("regular distill DB override works");

    assert!(result.is_none(), "distill command completes during hygiene");
    let metadata = std::fs::symlink_metadata(override_db).expect("override metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn startup_canonical_db_dangling_symlink_refuses_before_side_effects() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
    let root = fixture.path().join("Dangling-Canonical-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(canonical_db.parent().unwrap()).expect("canonical parent");
    let external_target = fixture.path().join("external/dangling.db");
    std::os::unix::fs::symlink(&external_target, &canonical_db)
        .expect("dangling canonical symlink");
    let canonical_identity = startup_file_identity(&canonical_db);
    let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
    std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
    std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
    std::fs::create_dir_all(&app_home).expect("app home");
    let manifest_before = b"startup-manifest-preimage";
    std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
    let cli = startup_test_cli();
    let ctx = startup_test_context(&root, &app_home);
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
        Err(error) => error,
        Ok(_) => panic!("dangling canonical DB symlink must refuse startup"),
    };

    assert!(
        error.to_string().contains("canonical repo DB path")
            && error.to_string().contains("must not be a symlink"),
        "expected canonical leaf refusal, got: {error}"
    );
    assert!(
        std::fs::symlink_metadata(&external_target).is_err(),
        "startup must not create the dangling external target"
    );
    assert_startup_canonical_symlink_refusal_side_effects(
        &root,
        &app_home,
        &canonical_db,
        &external_target,
        canonical_identity,
        &legacy_db,
        manifest_before,
    );
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn startup_canonical_db_wrong_target_symlink_refuses_before_side_effects() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
    let root = fixture.path().join("Wrong-Canonical-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(canonical_db.parent().unwrap()).expect("canonical parent");
    let external_target = fixture.path().join("external/wrong.db");
    std::fs::create_dir_all(external_target.parent().unwrap()).expect("external parent");
    std::fs::write(&external_target, b"foreign-external-db").expect("external DB");
    let external_identity = startup_file_identity(&external_target);
    std::os::unix::fs::symlink(&external_target, &canonical_db)
        .expect("wrong-target canonical symlink");
    let canonical_identity = startup_file_identity(&canonical_db);
    let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
    std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
    std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
    std::fs::create_dir_all(&app_home).expect("app home");
    let manifest_before = b"startup-manifest-preimage";
    std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
    let cli = startup_test_cli();
    let ctx = startup_test_context(&root, &app_home);
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

    let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
        Err(error) => error,
        Ok(_) => panic!("wrong-target canonical DB symlink must refuse startup"),
    };

    assert!(
        error.to_string().contains("canonical repo DB path")
            && error.to_string().contains("must not be a symlink"),
        "expected canonical leaf refusal, got: {error}"
    );
    assert_eq!(startup_file_identity(&external_target), external_identity);
    assert_eq!(
        std::fs::read(&external_target).expect("external DB preserved"),
        b"foreign-external-db"
    );
    assert_startup_canonical_symlink_refusal_side_effects(
        &root,
        &app_home,
        &canonical_db,
        &external_target,
        canonical_identity,
        &legacy_db,
        manifest_before,
    );
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn startup_canonical_db_symlink_loop_refuses_before_side_effects() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
    let root = fixture.path().join("Loop-Canonical-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(canonical_db.parent().unwrap()).expect("canonical parent");
    std::os::unix::fs::symlink(&canonical_db, &canonical_db).expect("looped canonical symlink");
    let canonical_identity = startup_file_identity(&canonical_db);
    let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
    std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
    std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
    std::fs::create_dir_all(&app_home).expect("app home");
    let manifest_before = b"startup-manifest-preimage";
    std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
    let cli = startup_test_cli();
    let ctx = startup_test_context(&root, &app_home);
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
        Err(error) => error,
        Ok(_) => panic!("canonical DB symlink loop must refuse startup"),
    };

    assert!(
        error.to_string().contains("canonical repo DB path")
            && error.to_string().contains("must not be a symlink"),
        "expected canonical leaf refusal, got: {error}"
    );
    assert_startup_canonical_symlink_refusal_side_effects(
        &root,
        &app_home,
        &canonical_db,
        &canonical_db,
        canonical_identity,
        &legacy_db,
        manifest_before,
    );
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn startup_canonical_db_normal_absence_keeps_plan_c_alias_and_opens_regular_db() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
    let root = fixture.path().join("Absent-Canonical-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    assert!(std::fs::symlink_metadata(&canonical_db).is_err());
    let cli = startup_test_cli();
    let ctx = startup_test_context(&root, &app_home);
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

    let hygiene = run_startup_hygiene(&cli, &ctx, &global_db)
        .await
        .expect("normal absent startup hygiene")
        .expect("serve continues");
    let server = MemoryServer::new(global_db, hygiene.project_db_path.clone())
        .expect("startup opens absent canonical DB as a regular file");
    drop(server);

    let metadata = std::fs::symlink_metadata(&canonical_db).expect("canonical DB created");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
    let project = crate::path_utils::plan_c_dir_name_from_root(&root).expect("project name");
    let alias = crate::path_utils::plan_c_global_db_path(&project);
    assert_eq!(
        std::fs::canonicalize(alias).expect("Plan C alias resolves"),
        std::fs::canonicalize(canonical_db).expect("canonical DB resolves")
    );
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn startup_canonical_db_regular_file_keeps_plan_c_alias_and_opens() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
    let root = fixture.path().join("Regular-Canonical-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(canonical_db.parent().unwrap()).expect("canonical parent");
    let store = memcore::MemoryStore::open(canonical_db.to_str().expect("UTF-8 DB"))
        .expect("seed regular canonical DB");
    drop(store);
    let canonical_identity = startup_file_identity(&canonical_db);
    let cli = startup_test_cli();
    let ctx = startup_test_context(&root, &app_home);
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

    let hygiene = run_startup_hygiene(&cli, &ctx, &global_db)
        .await
        .expect("regular canonical startup hygiene")
        .expect("serve continues");
    let server = MemoryServer::new(global_db, hygiene.project_db_path.clone())
        .expect("startup opens regular canonical DB");
    drop(server);

    assert_eq!(startup_file_identity(&canonical_db), canonical_identity);
    let project = crate::path_utils::plan_c_dir_name_from_root(&root).expect("project name");
    let alias = crate::path_utils::plan_c_global_db_path(&project);
    assert_eq!(
        std::fs::canonicalize(alias).expect("Plan C alias resolves"),
        std::fs::canonicalize(canonical_db).expect("canonical DB resolves")
    );
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn startup_plan_c_alias_refusal_precedes_legacy_copy_for_regular_file() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-alias-");
    let root = fixture.path().join("Regular-Alias-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
    std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
    std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
    let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    let alias_name = crate::path_utils::plan_c_dir_name_from_root(&root).expect("alias name");
    let alias_db = crate::path_utils::plan_c_global_db_path(&alias_name);
    std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
    std::fs::write(&alias_db, b"divergent-alias").expect("alias DB");
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    let cli = startup_test_cli();
    let ctx = startup_test_context(&root, &app_home);
    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
        Err(error) => error,
        Ok(_) => panic!("divergent alias must refuse startup"),
    };

    assert!(error.to_string().contains("split-brain"), "{error}");
    assert!(!canonical_db.exists(), "canonical DB must remain absent");
    assert!(
        !canonical_db.parent().unwrap().exists(),
        "canonical parent must remain absent"
    );
    assert_eq!(std::fs::read(&legacy_db).unwrap(), b"legacy-source");
    assert_eq!(std::fs::read(&alias_db).unwrap(), b"divergent-alias");
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn startup_plan_c_alias_refusal_precedes_legacy_copy_for_wrong_symlink() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-alias-");
    let root = fixture.path().join("Wrong-Symlink-Repo");
    let app_home = fixture.path().join("home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    std::fs::create_dir_all(root.join(".git")).expect("git root");
    let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
    std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
    std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
    let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    let alias_name = crate::path_utils::plan_c_dir_name_from_root(&root).expect("alias name");
    let alias_db = crate::path_utils::plan_c_global_db_path(&alias_name);
    let wrong_db = fixture.path().join("wrong-target.db");
    std::fs::write(&wrong_db, b"wrong-target").expect("wrong target");
    std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
    std::os::unix::fs::symlink(&wrong_db, &alias_db).expect("wrong symlink");
    let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    let cli = startup_test_cli();
    let ctx = startup_test_context(&root, &app_home);
    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
        Err(error) => error,
        Ok(_) => panic!("wrong-target alias must refuse startup"),
    };

    assert!(
        error.to_string().contains("instead of canonical DB"),
        "{error}"
    );
    assert!(!canonical_db.exists(), "canonical DB must remain absent");
    assert!(
        !canonical_db.parent().unwrap().exists(),
        "canonical parent must remain absent"
    );
    assert_eq!(std::fs::read(&legacy_db).unwrap(), b"legacy-source");
    assert_eq!(std::fs::read(&wrong_db).unwrap(), b"wrong-target");
    assert_eq!(std::fs::read_link(&alias_db).unwrap(), wrong_db);
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_refuses_when_owned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::Owned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(
        result.is_err(),
        "must refuse to copy a DB a live daemon holds open"
    );
    assert!(
        !dest.exists(),
        "refused copy must leave zero bytes at the destination"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_refuses_when_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::Unknown("lsof unavailable: test".to_string()),
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    let err = result.expect_err("undetermined ownership must refuse");
    assert!(
        err.to_string().contains("cannot determine"),
        "error must say ownership was undetermined, got: {err}"
    );
    assert!(
        !dest.exists(),
        "refused copy must leave zero bytes at the destination"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_proceeds_when_not_owned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(result.is_ok(), "not-owned must proceed: {result:?}");
    assert_eq!(
        std::fs::read(&dest).expect("dest must exist"),
        b"legacy-bytes"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_copies_wal_and_shm_sidecars_when_present() {
    // Crashed-WAL fixture: committed rows can live only in `-wal` until
    // checkpointed. Copying just the main file would silently drop them.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-main").expect("seed legacy main");
    std::fs::write(sidecar_path(&src, "-wal"), b"legacy-wal-rows").expect("seed legacy wal");
    std::fs::write(sidecar_path(&src, "-shm"), b"legacy-shm").expect("seed legacy shm");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(result.is_ok(), "not-owned must proceed: {result:?}");
    assert_eq!(std::fs::read(&dest).expect("dest main"), b"legacy-main");
    assert_eq!(
        std::fs::read(sidecar_path(&dest, "-wal")).expect("dest wal must exist"),
        b"legacy-wal-rows",
        "committed-only-in-WAL rows must not be silently dropped"
    );
    assert_eq!(
        std::fs::read(sidecar_path(&dest, "-shm")).expect("dest shm must exist"),
        b"legacy-shm"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_skips_absent_sidecars() {
    // No -wal/-shm on disk (already checkpointed / non-WAL source): the
    // copy must still succeed and must not fabricate sidecar files.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-main").expect("seed legacy main");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(result.is_ok(), "not-owned must proceed: {result:?}");
    assert!(!sidecar_path(&dest, "-wal").exists());
    assert!(!sidecar_path(&dest, "-shm").exists());
}

#[tokio::test]
#[cfg(unix)]
async fn copy_legacy_db_guarded_discards_partial_copy_when_sidecar_copy_fails() {
    // Force the -wal sidecar copy to fail (source is a directory, not a
    // regular file) after the main file has already been copied. The
    // whole destination — main + any sidecar already copied — must be
    // discarded; a main-file-only leftover would silently drop the rows
    // that only exist in -wal.
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("legacy.db");
    let dest = dir.path().join("global").join("memory.db");
    std::fs::write(&src, b"legacy-main").expect("seed legacy main");
    std::fs::create_dir(sidecar_path(&src, "-wal")).expect("wal as directory");

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::NotOwned,
    ));

    let result = copy_legacy_db_guarded(&src, &dest).await;

    assert!(
        result.is_err(),
        "sidecar copy failure must fail the whole operation"
    );
    assert!(
        !dest.exists(),
        "partial main-file-only copy must be discarded on sidecar failure"
    );
    assert!(!sidecar_path(&dest, "-wal").exists());
    assert!(!sidecar_path(&dest, "-shm").exists());
}

// No test exercises the "cleanup itself fails" branch (the
// `partial copy could not be fully discarded: ...` message) — there is
// no cheap injection available for it. `unlink(2)`/`remove_file` needs
// WRITE permission on the file's *parent directory*, not the file
// itself; that is the exact same permission `tokio::fs::copy` needs to
// *create* `dest` a few lines earlier in this same call. Chmod'ing
// `dest`'s parent read-only before the call blocks the initial copy
// (a different, earlier failure) rather than isolating a delete-only
// failure; chmod'ing it read-only *between* the main-file copy and the
// sidecar-failure trigger would require instrumenting the function
// itself (a test seam this function does not otherwise need), and a
// platform-specific immutable-file flag (e.g. macOS `chflags uchg`) is
// not portable enough to call "cheap". The message-formatting logic
// itself is straight-line and covered by review; if a cheap injection
// seam is added to this function later, add the test alongside it.

fn remember_cli(global_db: std::path::PathBuf, allow_schema_migration: bool) -> Cli {
    Cli {
        daemon: false,
        port: 6919,
        global_db: Some(global_db),
        project_db: None,
        no_project_db: true,
        allow_schema_migration,
        profile: None,
        gc_enabled: None,
        gc_initial_delay_secs: None,
        gc_interval_secs: None,
        command: Some(Commands::Remember {
            text: Some("#1131 cli migration authority regression".to_string()),
            text_flag: None,
            tags: vec![],
            scope: None,
            project: None,
            path: None,
            importance: None,
            category: None,
            topic: None,
            domain: None,
            retention_policy: None,
            summary: None,
            force: true,
            supersedes: vec![],
        }),
    }
}

#[test]
fn remember_cli_requires_flag_to_migrate_stamped_older_db_in_process() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("cli-remember-schema-");
    let app_home = fixture.path().join("home");
    let global_db = app_home.join("global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global DB parent"))
        .expect("create global DB parent");
    let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = crate::test_support::EnvRestore::remove("SIGIL_HOME");
    let _app_home = crate::test_support::EnvRestore::remove("TACHI_APP_HOME");

    let server = MemoryServer::new(global_db.clone(), None).expect("seed current DB");
    drop(server);
    let conn = memcore::db::open_raw(&global_db).expect("open seeded DB");
    conn.execute_batch(&format!(
        "PRAGMA user_version = {}",
        memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
    ))
    .expect("stamp older schema version");
    drop(conn);

    let err = tokio_main(remember_cli(global_db.clone(), false))
        .expect_err("remember without the flag must preserve OpenExisting + Deny");
    assert!(
        err.to_string().contains("refusing to migrate db schema"),
        "unexpected deny error: {err}"
    );

    let conn = memcore::db::open_raw(&global_db).expect("reopen denied DB");
    assert_eq!(
        memcore::db::migrations::read_schema_version(&conn).expect("read denied version"),
        memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
        "deny must not mutate the old schema stamp"
    );
    drop(conn);

    tokio_main(remember_cli(global_db.clone(), true))
        .expect("remember with the flag must migrate the isolated DB");

    let conn = memcore::db::open_raw(&global_db).expect("reopen migrated DB");
    assert_eq!(
        memcore::db::migrations::read_schema_version(&conn).expect("read migrated version"),
        memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
        "allow must re-stamp the DB at the current schema version"
    );
}

fn backfill_fts_cli(global_db: std::path::PathBuf, allow_schema_migration: bool) -> Cli {
    Cli {
        daemon: false,
        port: 6919,
        global_db: Some(global_db),
        project_db: None,
        no_project_db: true,
        allow_schema_migration,
        profile: None,
        gc_enabled: None,
        gc_initial_delay_secs: None,
        gc_interval_secs: None,
        command: Some(Commands::BackfillFts {
            db: None,
            full: false,
            dry_run: false,
        }),
    }
}

/// Codex review (2026-07-17, checkpoint 3): the discrimination tests in
/// `bootstrap::backfill::tests` call the private `run_backfill_*`
/// functions directly with a hand-constructed `MigrationAuthority`,
/// bypassing the actual CLI flag resolution (`initialize_startup_context`)
/// and dispatch (`run_if_backfill_command`) #1181's contract is about.
/// This test closes that gap for `backfill-fts` by going through the
/// full `tokio_main` entry point, mirroring
/// `remember_cli_requires_flag_to_migrate_stamped_older_db_in_process`
/// immediately above.
#[test]
fn backfill_fts_cli_requires_flag_to_migrate_stamped_older_db_in_process() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = crate::test_support::non_skipped_fixture_tempdir("cli-backfill-fts-schema-");
    let app_home = fixture.path().join("home");
    let global_db = app_home.join("global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global DB parent"))
        .expect("create global DB parent");
    let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = crate::test_support::EnvRestore::remove("SIGIL_HOME");
    let _app_home = crate::test_support::EnvRestore::remove("TACHI_APP_HOME");

    let server = MemoryServer::new(global_db.clone(), None).expect("seed current DB");
    drop(server);
    let conn = memcore::db::open_raw(&global_db).expect("open seeded DB");
    conn.execute_batch(&format!(
        "PRAGMA user_version = {}",
        memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
    ))
    .expect("stamp older schema version");
    drop(conn);

    let err = tokio_main(backfill_fts_cli(global_db.clone(), false))
        .expect_err("backfill-fts without the flag must preserve OpenExisting + Deny");
    assert!(
        err.to_string().contains("refusing to migrate db schema"),
        "unexpected deny error: {err}"
    );

    let conn = memcore::db::open_raw(&global_db).expect("reopen denied DB");
    assert_eq!(
        memcore::db::migrations::read_schema_version(&conn).expect("read denied version"),
        memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
        "deny must not mutate the old schema stamp"
    );
    drop(conn);

    tokio_main(backfill_fts_cli(global_db.clone(), true))
        .expect("backfill-fts with the flag must migrate the isolated DB, resolved through the real CLI flag path");

    let conn = memcore::db::open_raw(&global_db).expect("reopen migrated DB");
    assert_eq!(
        memcore::db::migrations::read_schema_version(&conn).expect("read migrated version"),
        memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
        "allow must re-stamp the DB at the current schema version"
    );
}

#[test]
fn primary_log_path_lives_under_app_home() {
    let app_home = std::path::Path::new("/tmp/tachi-app-home");
    assert_eq!(
        primary_log_path(app_home),
        app_home.join("logs").join("tachi.log")
    );
}

#[test]
fn workspace_home_notice_is_emitted_only_for_implicit_workspace_selection() {
    let path = Path::new("/tmp/repo/data/tachi");
    assert_eq!(
        workspace_home_selection_notice(crate::path_utils::TachiHomeSource::WorkspaceData, path)
            .as_deref(),
        Some(
            "[tachi-home] selected workspace home /tmp/repo/data/tachi from cwd ancestry; set TACHI_HOME to pin a different home"
        )
    );
    assert_eq!(
        workspace_home_selection_notice(
            crate::path_utils::TachiHomeSource::ExplicitEnv("TACHI_HOME"),
            path,
        ),
        None
    );
    assert_eq!(
        workspace_home_selection_notice(crate::path_utils::TachiHomeSource::UserDefault, path),
        None
    );
}

#[test]
fn daily_distill_marker_path_lives_under_app_home() {
    let app_home = std::path::Path::new("/tmp/tachi-custom-home");
    assert_eq!(
        daily_distill_marker_path(app_home),
        app_home.join("foundry-runs").join(".last_distill_run")
    );
}

#[test]
fn no_project_serve_detaches_launch_cwd_only_for_mcp_serve() {
    assert!(no_project_serve_detaches_launch_cwd(&Commands::Serve, true));
    assert!(!no_project_serve_detaches_launch_cwd(
        &Commands::Serve,
        false
    ));
    assert!(!no_project_serve_detaches_launch_cwd(
        &Commands::Stats,
        true
    ));
}

#[test]
fn project_local_env_loading_skips_no_project_and_vault_commands() {
    assert!(!should_load_project_local_env(&Commands::Serve, true));
    assert!(!should_load_project_local_env(
        &Commands::Vault {
            action: tachi_bootstrap::cli::VaultAction::Status,
        },
        false
    ));
    assert!(should_load_project_local_env(&Commands::Serve, false));
}

#[test]
#[cfg(unix)]
fn vault_status_does_not_create_project_alias_from_cwd() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = tempfile::tempdir().expect("vault status fixture");
    let app_home = fixture.path().join("home");
    let repo = fixture.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).expect("create synthetic git root");

    let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil_home = crate::test_support::EnvRestore::remove("SIGIL_HOME");
    let _app_home = crate::test_support::EnvRestore::remove("TACHI_APP_HOME");
    let _tachi_root = crate::test_support::EnvRestore::remove("TACHI_ROOT");
    let _memory_db = crate::test_support::EnvRestore::remove("MEMORY_DB_PATH");
    let _no_tty = crate::test_support::EnvRestore::set("TACHI_TEST_FORCE_NO_TTY", "1");
    let _cwd = crate::test_support::CwdRestore::set(&repo);
    std::fs::create_dir_all(app_home.join("global")).unwrap();
    drop(
        memcore::MemoryStore::open(app_home.join("global/tachi-memory.db").to_str().unwrap())
            .expect("seed existing global store"),
    );

    let cli = Cli {
        daemon: false,
        port: 6919,
        global_db: Some(app_home.join("global/tachi-memory.db")),
        project_db: None,
        no_project_db: false,
        allow_schema_migration: false,
        profile: None,
        gc_enabled: None,
        gc_initial_delay_secs: None,
        gc_interval_secs: None,
        command: Some(Commands::Vault {
            action: tachi_bootstrap::cli::VaultAction::Status,
        }),
    };

    tokio_main(cli).expect("vault status should run without project identity");

    assert!(
        !app_home.join("projects").exists(),
        "vault status must not create a Plan C alias from its launch cwd"
    );
    assert!(
        !repo.join(".tachi").exists(),
        "vault status must not initialize a repo-local project DB"
    );
}

#[test]
fn manifest_startup_is_deferred_for_no_project_daemon_serve() {
    assert!(should_defer_manifest_startup(&Commands::Serve, true, true));
}

#[test]
fn manifest_startup_stays_enabled_for_no_project_stdio_serve() {
    assert!(!should_defer_manifest_startup(
        &Commands::Serve,
        false,
        true
    ));
}

#[test]
fn manifest_startup_stays_enabled_for_project_daemon_serve() {
    assert!(!should_defer_manifest_startup(
        &Commands::Serve,
        true,
        false
    ));
}

#[test]
fn plan_c_symlink_refresh_skips_explicit_project_db() {
    let project_db = std::path::Path::new("/tmp/agent-workspace/memory.db");
    let git_root = std::path::Path::new("/tmp/repo");

    assert!(!should_refresh_plan_c_symlink(
        Some(project_db),
        Some(git_root),
        true
    ));
    assert!(should_refresh_plan_c_symlink(
        Some(project_db),
        Some(git_root),
        false
    ));
    assert!(!should_refresh_plan_c_symlink(None, Some(git_root), false));
    assert!(!should_refresh_plan_c_symlink(
        Some(project_db),
        None,
        false
    ));
}

#[cfg(unix)]
#[test]
fn restrict_file_permissions_sets_owner_only_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::NamedTempFile::new().expect("temp file");
    let path = temp.path();

    restrict_file_permissions(path).expect("set permissions");

    let meta = std::fs::metadata(path).expect("metadata");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "expected 0o600, got {mode:o}");
}

/// Hermetic global-only server bound to `home` as its Tachi home, so the
/// distill gate's named-project scan sees only what the test created.
fn server_with_home(home: &std::path::Path, project_db: Option<PathBuf>) -> crate::MemoryServer {
    crate::MemoryServer::new_with_home_for_test(
        home.join("global.db"),
        project_db,
        home.to_path_buf(),
    )
    .expect("server")
}

fn write_named_project_db(home: &std::path::Path, name: &str) {
    let dir = home.join("projects").join(name);
    std::fs::create_dir_all(&dir).expect("named project dir");
    std::fs::write(dir.join(memcore::MEMORY_DB_FILENAME), b"").expect("named project db file");
}

#[test]
fn daily_distill_scheduler_stays_enabled_when_pipeline_is_disabled() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = std::env::var("ENABLE_PIPELINE").ok();
    std::env::remove_var("ENABLE_PIPELINE");

    let temp = tempfile::tempdir().expect("tempdir");
    let project_db = temp.path().join("project").join("memory.db");
    std::fs::create_dir_all(project_db.parent().expect("project parent"))
        .expect("create project db parent");

    let server = server_with_home(temp.path(), Some(project_db));
    assert!(
        !server.pipeline_enabled,
        "test precondition: pipeline should default to disabled"
    );
    assert!(matches!(
        daily_distill_scheduler_gate(&server),
        DailyDistillGate::Enabled {
            bound_project: true,
            ..
        }
    ));

    match previous {
        Some(value) => std::env::set_var("ENABLE_PIPELINE", value),
        None => std::env::remove_var("ENABLE_PIPELINE"),
    }
}

/// #1605 headline: the global-only owner daemon has real distill work —
/// `run_daily_batch_distill` scans every manifest-attached named-project DB
/// with no bound project involved — so the gate must spawn, not refuse.
#[test]
fn daily_distill_scheduler_enabled_for_global_only_daemon_with_named_projects() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    write_named_project_db(temp.path(), "Sigil");
    let server = server_with_home(temp.path(), None);

    assert!(
        !server.has_project_db(),
        "test precondition: no bound project DB"
    );
    assert_eq!(
        daily_distill_scheduler_gate(&server),
        DailyDistillGate::Enabled {
            bound_project: false,
            named_projects: 1,
        }
    );
}

/// The runner skips `wiki` (it has its own curation path), so a home whose
/// only named project is `wiki` genuinely has nothing to distill — and the
/// refusal must name the gate and the remedy rather than go silent.
#[test]
fn daily_distill_scheduler_disabled_refusal_names_gate_and_remedy() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    write_named_project_db(temp.path(), "wiki");
    let server = server_with_home(temp.path(), None);

    match daily_distill_scheduler_gate(&server) {
        DailyDistillGate::Disabled { reason, remedy } => {
            assert!(
                reason.contains("daily_distill_scheduler_gate"),
                "refusal must name the gate, got: {reason}"
            );
            assert!(
                reason.contains("no bound project DB"),
                "refusal must state the condition, got: {reason}"
            );
            assert!(
                remedy.contains("--project-db") && remedy.contains("projects"),
                "refusal must state the remedy, got: {remedy}"
            );
        }
        other => panic!("expected DISABLED for a wiki-only home, got {other:?}"),
    }
}

struct HostProfileEnvFixture {
    _lock: std::sync::MutexGuard<'static, ()>,
    original_cwd: PathBuf,
    original_host_profile: Option<std::ffi::OsString>,
    original_fixture_key: Option<std::ffi::OsString>,
    home: tempfile::TempDir,
    app_home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl HostProfileEnvFixture {
    fn new() -> Self {
        let lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_cwd = std::env::current_dir().expect("cwd");
        let original_host_profile = std::env::var_os(crate::host_profile::HOST_PROFILE_ENV);
        let original_fixture_key = std::env::var_os("TACHI_TEST_HOST_PROFILE_FIXTURE");
        let home = tempfile::tempdir().expect("home");
        let app_home = tempfile::tempdir().expect("app_home");
        let repo = tempfile::tempdir().expect("repo");
        std::fs::create_dir_all(home.path().join(".secrets")).expect("secrets dir");
        std::fs::create_dir_all(home.path().join(".sigil")).expect("legacy home dir");
        std::fs::create_dir_all(repo.path().join(".tachi")).expect("repo tachi dir");
        std::fs::create_dir_all(repo.path().join(".sigil")).expect("repo sigil dir");
        std::env::set_current_dir(repo.path()).expect("set synthetic repo cwd");
        Self {
            _lock: lock,
            original_cwd,
            original_host_profile,
            original_fixture_key,
            home,
            app_home,
            repo,
        }
    }

    fn write_env(&self, path: PathBuf, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("env parent");
        }
        std::fs::write(path, body).expect("write env file");
    }

    fn seed_elevated_sources(&self) {
        self.write_env(
            self.home.path().join(".secrets/master.env"),
            "TACHI_HOST_PROFILE=home_data\n",
        );
        self.write_env(
            self.home.path().join(".sigil/config.env"),
            "TACHI_HOST_PROFILE=home_data\n",
        );
        self.write_env(
            self.repo.path().join(".tachi/config.env"),
            "TACHI_HOST_PROFILE=home_data\nTACHI_TEST_HOST_PROFILE_FIXTURE=repo_local_loaded\n",
        );
        self.write_env(
            self.repo.path().join(".sigil/config.env"),
            "TACHI_HOST_PROFILE=home_data\n",
        );
        self.write_env(
            self.repo.path().join(".env"),
            "TACHI_HOST_PROFILE=home_data\n",
        );
        std::env::set_var(crate::host_profile::HOST_PROFILE_ENV, "home_data");
    }

    fn load(&self) {
        load_env_files(
            self.home.path(),
            self.app_home.path(),
            true,
            Some(self.repo.path()),
        );
    }
}

impl Drop for HostProfileEnvFixture {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original_cwd);
        match self.original_host_profile.as_ref() {
            Some(value) => std::env::set_var(crate::host_profile::HOST_PROFILE_ENV, value),
            None => std::env::remove_var(crate::host_profile::HOST_PROFILE_ENV),
        }
        match self.original_fixture_key.as_ref() {
            Some(value) => std::env::set_var("TACHI_TEST_HOST_PROFILE_FIXTURE", value),
            None => std::env::remove_var("TACHI_TEST_HOST_PROFILE_FIXTURE"),
        }
    }
}

#[test]
fn app_home_is_only_host_profile_authority() {
    let fixture = HostProfileEnvFixture::new();
    fixture.seed_elevated_sources();
    fixture.write_env(
        fixture.app_home.path().join("config.env"),
        "TACHI_HOST_PROFILE=development\n",
    );

    fixture.load();

    assert_eq!(
        std::env::var(crate::host_profile::HOST_PROFILE_ENV).as_deref(),
        Ok("development")
    );
    assert_eq!(
        crate::host_profile::HostProfile::current().expect("profile"),
        crate::host_profile::HostProfile::Development
    );

    fixture.write_env(
        fixture.app_home.path().join("config.env"),
        "TACHI_HOST_PROFILE=home_data\n",
    );
    std::env::set_var(crate::host_profile::HOST_PROFILE_ENV, "development");
    fixture.write_env(
        fixture.repo.path().join(".tachi/config.env"),
        "TACHI_HOST_PROFILE=development\nTACHI_TEST_HOST_PROFILE_FIXTURE=repo_local_loaded\n",
    );
    fixture.load();
    assert_eq!(
        std::env::var(crate::host_profile::HOST_PROFILE_ENV).as_deref(),
        Ok("home_data")
    );
    assert_eq!(
        crate::host_profile::HostProfile::current().expect("profile"),
        crate::host_profile::HostProfile::HomeData
    );
}

#[test]
fn missing_app_home_profile_rejects_inherited_and_repo_elevation() {
    let fixture = HostProfileEnvFixture::new();
    fixture.seed_elevated_sources();
    fixture.write_env(
        fixture.app_home.path().join("config.env"),
        "# intentionally missing TACHI_HOST_PROFILE\n",
    );

    fixture.load();

    assert!(
        std::env::var_os(crate::host_profile::HOST_PROFILE_ENV).is_none(),
        "missing app-home profile must leave the raw key absent"
    );
    assert_eq!(
        crate::host_profile::HostProfile::current().expect("default profile"),
        crate::host_profile::HostProfile::Development
    );
}

#[test]
fn invalid_app_home_profile_is_not_repaired_by_repo_profile() {
    let fixture = HostProfileEnvFixture::new();
    fixture.seed_elevated_sources();
    fixture.write_env(
        fixture.app_home.path().join("config.env"),
        "TACHI_HOST_PROFILE=not-a-real-profile\n",
    );

    fixture.load();

    assert_eq!(
        std::env::var(crate::host_profile::HOST_PROFILE_ENV).as_deref(),
        Ok("not-a-real-profile")
    );
    let err = crate::host_profile::HostProfile::current().expect_err("invalid stays invalid");
    assert!(
        err.contains("invalid TACHI_HOST_PROFILE"),
        "expected invalid profile error, got {err}"
    );
}

#[test]
fn project_local_non_host_env_still_loads() {
    let fixture = HostProfileEnvFixture::new();
    std::env::remove_var(crate::host_profile::HOST_PROFILE_ENV);
    std::env::remove_var("TACHI_TEST_HOST_PROFILE_FIXTURE");
    fixture.write_env(
        fixture.app_home.path().join("config.env"),
        "TACHI_HOST_PROFILE=development\n",
    );
    fixture.write_env(
        fixture.repo.path().join(".tachi/config.env"),
        "TACHI_TEST_HOST_PROFILE_FIXTURE=repo_local_loaded\n",
    );

    fixture.load();

    assert_eq!(
        std::env::var("TACHI_TEST_HOST_PROFILE_FIXTURE").as_deref(),
        Ok("repo_local_loaded"),
        "project-local non-host keys must still load"
    );
    assert_eq!(
        std::env::var(crate::host_profile::HOST_PROFILE_ENV).as_deref(),
        Ok("development")
    );
}

/// #2111: app-home `secrets.env` must load for serve/daemon the same way
/// the `tachi-mcp` wrapper sources it, so provider keys reach embed.
#[test]
fn app_home_secrets_env_loads_provider_keys() {
    let fixture = HostProfileEnvFixture::new();
    std::env::remove_var(crate::host_profile::HOST_PROFILE_ENV);
    std::env::remove_var("TACHI_TEST_SECRETS_ENV_PROBE");
    fixture.write_env(
        fixture.app_home.path().join("config.env"),
        "TACHI_HOST_PROFILE=development\n",
    );
    fixture.write_env(
        fixture.app_home.path().join("secrets.env"),
        "TACHI_TEST_SECRETS_ENV_PROBE=from-secrets-env\n",
    );

    fixture.load();

    assert_eq!(
        std::env::var("TACHI_TEST_SECRETS_ENV_PROBE").as_deref(),
        Ok("from-secrets-env"),
        "app-home secrets.env must be loaded by serve bootstrap"
    );
    std::env::remove_var("TACHI_TEST_SECRETS_ENV_PROBE");
}

/// #2111: process-injected secrets (Cursor/cloud) must win over the file.
#[test]
fn app_home_secrets_env_does_not_override_process_env() {
    let fixture = HostProfileEnvFixture::new();
    std::env::remove_var(crate::host_profile::HOST_PROFILE_ENV);
    std::env::set_var("TACHI_TEST_SECRETS_ENV_PROBE", "from-process");
    fixture.write_env(
        fixture.app_home.path().join("config.env"),
        "TACHI_HOST_PROFILE=development\n",
    );
    fixture.write_env(
        fixture.app_home.path().join("secrets.env"),
        "TACHI_TEST_SECRETS_ENV_PROBE=from-secrets-env\n",
    );

    fixture.load();

    assert_eq!(
        std::env::var("TACHI_TEST_SECRETS_ENV_PROBE").as_deref(),
        Ok("from-process"),
        "already-set process env must win over secrets.env"
    );
    std::env::remove_var("TACHI_TEST_SECRETS_ENV_PROBE");
}

#[test]
fn retired_stdio_refuses_before_startup_or_store_side_effects() {
    let _guard = crate::utils::global_test_lock().lock().unwrap();
    for explicit_serve in [false, true] {
        for existing in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let global = root.path().join("database/memory.db");
            let project = root.path().join("project/memory.db");
            if existing {
                std::fs::create_dir_all(&home).unwrap();
                std::fs::write(home.join("config.env"), "TACHI_DISABLE_STDIO_PROXY=1\n").unwrap();
                std::fs::create_dir_all(global.parent().unwrap()).unwrap();
                std::fs::write(&global, b"existing DB sentinel").unwrap();
            }
            let _home = EnvRestore::set_path("TACHI_HOME", &home);
            let _proxy = EnvRestore::set("TACHI_DISABLE_STDIO_PROXY", "1");
            let mut cli = startup_test_cli();
            cli.global_db = Some(global.clone());
            cli.project_db = Some(project.clone());
            cli.allow_schema_migration = true;
            if !explicit_serve {
                cli.command = None;
            }
            let error = tokio_main(cli).unwrap_err().to_string();
            assert!(error.contains("stdio has been retired"), "{error}");
            assert!(
                error.contains("tachi --daemon --port 6919 serve"),
                "{error}"
            );
            assert!(!project.parent().unwrap().exists());
            if existing {
                assert_eq!(std::fs::read(&global).unwrap(), b"existing DB sentinel");
                assert_eq!(std::fs::read_dir(&home).unwrap().count(), 1);
                assert_eq!(
                    std::fs::read_dir(global.parent().unwrap()).unwrap().count(),
                    1
                );
            } else {
                assert!(!home.exists());
                assert!(!global.parent().unwrap().exists());
            }
        }
    }
}
