use super::*;

#[test]
fn tidy_execute_migrates_rows_and_archives_source() {
    let root = crate::utils::test_fixture_path(format!("tachi-tidy-exec-{}", uuid::Uuid::new_v4()));
    let home = root.clone();
    let app_home = home.join(".tachi");
    std::fs::create_dir_all(&app_home).expect("create app_home");
    let target_db = app_home.join("global").join("memory.db");
    let archive_root = app_home.join("archive").join("ts-test");

    // Seed: 1 keep-able global + 2 legacy migration candidates.
    std::fs::create_dir_all(target_db.parent().unwrap()).expect("create target parent");
    let mut tgt = MemoryStore::open(target_db.to_str().unwrap()).expect("open target");
    tgt.upsert(&make_entry("preexisting-target"))
        .expect("seed target");
    drop(tgt);

    let (legacy_a, legacy_b) = build_legacy_openclaw_fixture(&home, 3);

    let report = crate::bootstrap::build_tidy_report(&[home.clone()], None).expect("report");
    let plan = crate::bootstrap::build_migration_plan(&report, &target_db, &archive_root, &home);
    assert_eq!(plan.len(), 2, "expected 2 legacy DBs in the plan");

    // First: dry-run via MigrationConfig — verify zero writes.
    let dry_cfg = crate::bootstrap::MigrationConfig {
        target_db: target_db.clone(),
        manifest_path: app_home.join("manifest.json"),
        dry_run: true,
        interactive: false,
        app_home: app_home.clone(),
    };
    let authorized_sources = authorized_plan_sources(&plan);
    let dry_summary =
        crate::bootstrap::execute_tidy_migrations(&plan, &dry_cfg, &authorized_sources)
            .expect("dry-run summary");
    assert!(dry_summary.dry_run);
    assert_eq!(dry_summary.migrated_count, 0);
    assert_eq!(dry_summary.outcomes.len(), 2);
    let statuses: Vec<&str> = dry_summary
        .outcomes
        .iter()
        .map(|o| o.status.as_str())
        .collect();
    let messages: Vec<&str> = dry_summary
        .outcomes
        .iter()
        .map(|o| o.message.as_str())
        .collect();
    assert!(
        dry_summary.outcomes.iter().all(|o| o.status == "dry_run"),
        "expected all dry_run; got statuses={statuses:?} messages={messages:?}"
    );
    // Sources still exist after dry-run.
    assert!(legacy_a.exists());
    assert!(legacy_b.exists());
    // Target row count unchanged.
    let tgt_check = MemoryStore::open_read_only(target_db.to_str().unwrap()).expect("open target");
    assert_eq!(tgt_check.stats(true).unwrap().total, 1);
    drop(tgt_check);

    // Second: real execute.
    let exec_cfg = crate::bootstrap::MigrationConfig {
        target_db: target_db.clone(),
        manifest_path: app_home.join("manifest.json"),
        dry_run: false,
        interactive: false,
        app_home: app_home.clone(),
    };
    let summary = crate::bootstrap::execute_tidy_migrations(&plan, &exec_cfg, &authorized_sources)
        .expect("execute summary");
    let exec_messages: Vec<&str> = summary
        .outcomes
        .iter()
        .map(|o| o.message.as_str())
        .collect();
    let exec_statuses: Vec<&str> = summary.outcomes.iter().map(|o| o.status.as_str()).collect();
    assert_eq!(
        summary.migrated_count, 2,
        "expected 2 migrated; statuses={exec_statuses:?} messages={exec_messages:?}"
    );
    assert_eq!(summary.failed_count, 0);
    // Sources moved into archive.
    assert!(!legacy_a.exists(), "source A should be archived");
    assert!(!legacy_b.exists(), "source B should be archived");
    // Archive files exist.
    for outcome in &summary.outcomes {
        let arch = outcome.archive_path.as_ref().expect("archive path");
        assert!(
            std::path::Path::new(arch).exists(),
            "archive {arch} should exist"
        );
    }
    // Target now has preexisting + 2*3 migrated = 7 rows.
    let tgt_after =
        MemoryStore::open_read_only(target_db.to_str().unwrap()).expect("open target after");
    assert_eq!(tgt_after.stats(true).unwrap().total, 7);
    drop(tgt_after);
    // Manifest contains target entry.
    let manifest =
        crate::manifest::Manifest::load(&app_home.join("manifest.json")).expect("load manifest");
    assert!(manifest
        .dbs
        .iter()
        .any(|e| e.path.contains("global") && e.path.ends_with("memory.db")));

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_execute_never_archives_symlink_target_selected_as_inventory_open_path() {
    let root = crate::utils::test_fixture_path(format!(
        "tachi-tidy-symlink-auth-{}",
        uuid::Uuid::new_v4()
    ));
    let home = root.join("home");
    let app_home = home.join(".tachi");
    let target_db = app_home.join("global/memory.db");
    let real_db = root.join("outside/real-store.sqlite");
    let alias = home.join(".openclaw/core/extensions/tachi/data/agents/legacy-link/memory.db");
    std::fs::create_dir_all(real_db.parent().unwrap()).expect("create real DB parent");
    std::fs::create_dir_all(alias.parent().unwrap()).expect("create alias parent");

    let mut real_store = MemoryStore::open(real_db.to_str().unwrap()).expect("open real DB");
    real_store
        .upsert(&make_entry("symlink-authority-row"))
        .expect("seed real DB");
    drop(real_store);
    std::os::unix::fs::symlink(&real_db, &alias).expect("create discovered alias");

    let report = crate::bootstrap::build_tidy_report(&[home.clone()], None).expect("report");
    let archive_root = app_home.join("archive/ts-symlink-auth");
    let plan = crate::bootstrap::build_migration_plan(&report, &target_db, &archive_root, &home);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].source_path, alias.display().to_string());
    let canonical_real_db = std::fs::canonicalize(&real_db).expect("canonical real DB");
    assert_eq!(
        report.databases[0].inventory_open_path.as_deref(),
        canonical_real_db.to_str(),
        "fixture must force the read probe through the real target"
    );

    let authorized_sources = crate::bootstrap::authorized_migration_sources(&report);
    let cfg = crate::bootstrap::MigrationConfig {
        target_db: target_db.clone(),
        manifest_path: app_home.join("manifest.json"),
        dry_run: false,
        interactive: false,
        app_home: app_home.clone(),
    };

    let mut probe_path_plan = plan.clone();
    probe_path_plan[0].source_path = real_db.display().to_string();
    let refused =
        crate::bootstrap::execute_tidy_migrations(&probe_path_plan, &cfg, &authorized_sources)
            .expect("unauthorized probe path must produce a refusal outcome");
    assert_eq!(refused.failed_count, 1);
    assert!(refused.outcomes[0]
        .message
        .contains("scan-captured physical authority"));
    assert!(real_db.exists(), "refusal must not archive the real target");
    assert!(alias.is_symlink(), "refusal must not move the alias either");

    let executed = crate::bootstrap::execute_tidy_migrations(&plan, &cfg, &authorized_sources)
        .expect("authorized alias migration");
    assert_eq!(executed.migrated_count, 1, "{executed:?}");
    assert!(
        real_db.exists(),
        "authorized alias must not move its target"
    );
    assert!(
        std::fs::symlink_metadata(&plan[0].archive_path)
            .expect("archived alias")
            .file_type()
            .is_symlink(),
        "archive operation must move the discovered symlink itself"
    );
    assert!(std::fs::symlink_metadata(&alias).is_err());
    let target = MemoryStore::open_read_only(target_db.to_str().unwrap()).expect("open target");
    assert_eq!(target.stats(true).unwrap().total, 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
fn planned_mutation_authority_fixture(
    label: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    Vec<crate::bootstrap::TidyMigration>,
    std::collections::BTreeMap<String, crate::physical_db_identity::PhysicalMutationAuthority>,
    crate::bootstrap::MigrationConfig,
) {
    let root = crate::utils::test_fixture_path(format!(
        "tachi-tidy-physical-authority-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    let app_home = root.join(".tachi");
    let target = app_home.join("global/memory.db");
    let source = root.join(".openclaw/core/extensions/tachi/data/agents/legacy/memory.db");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();

    let mut target_store = MemoryStore::open(target.to_str().unwrap()).unwrap();
    target_store
        .upsert(&make_entry("target-before-authority-check"))
        .unwrap();
    drop(target_store);
    let mut source_store = MemoryStore::open(source.to_str().unwrap()).unwrap();
    source_store
        .upsert(&make_entry("scanned-source-row"))
        .unwrap();
    drop(source_store);

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), None).unwrap();
    let archive_root = app_home.join("archive/physical-authority");
    let plan = crate::bootstrap::build_migration_plan(&report, &target, &archive_root, &root);
    assert_eq!(plan.len(), 1, "fixture must produce one migration plan");
    let authorities = crate::bootstrap::authorized_migration_sources(&report);
    assert_eq!(
        authorities.len(),
        1,
        "fixture must bind one physical source"
    );
    let cfg = crate::bootstrap::MigrationConfig {
        target_db: target.clone(),
        manifest_path: app_home.join("manifest.json"),
        dry_run: false,
        interactive: false,
        app_home,
    };

    (root, source, target, plan, authorities, cfg)
}

#[cfg(unix)]
#[test]
fn tidy_execute_refuses_symlink_substitution_after_scan_before_source_open() {
    let (root, source, target, plan, authorities, cfg) =
        planned_mutation_authority_fixture("symlink-substitution");
    let replacement = root.join("replacement/other.db");
    std::fs::create_dir_all(replacement.parent().unwrap()).unwrap();
    let mut replacement_store = MemoryStore::open(replacement.to_str().unwrap()).unwrap();
    replacement_store
        .upsert(&make_entry("replacement-must-not-open"))
        .unwrap();
    drop(replacement_store);

    std::fs::rename(&source, source.with_extension("scanned.db")).unwrap();
    std::os::unix::fs::symlink(&replacement, &source).unwrap();

    let summary = crate::bootstrap::execute_tidy_migrations(&plan, &cfg, &authorities).unwrap();
    assert_eq!(summary.failed_count, 1);
    assert_eq!(summary.migrated_count, 0);
    assert!(summary.outcomes[0].message.contains("canonical identity"));
    assert!(
        source.is_symlink(),
        "substituted source must remain untouched"
    );
    assert!(
        replacement.exists(),
        "replacement object must never be opened or archived"
    );
    assert!(
        !std::path::Path::new(&plan[0].archive_path).exists(),
        "refusal before source open must not create an archive object"
    );
    let target_store = MemoryStore::open_read_only(target.to_str().unwrap()).unwrap();
    assert_eq!(target_store.stats(true).unwrap().total, 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_execute_refuses_replaced_source_inode_after_plan_before_source_open() {
    let (root, source, target, plan, authorities, cfg) =
        planned_mutation_authority_fixture("inode-replacement");
    let replacement = root.join("replacement/other.db");
    std::fs::create_dir_all(replacement.parent().unwrap()).unwrap();
    let mut replacement_store = MemoryStore::open(replacement.to_str().unwrap()).unwrap();
    replacement_store
        .upsert(&make_entry("replacement-must-not-open"))
        .unwrap();
    drop(replacement_store);

    std::fs::rename(&source, source.with_extension("scanned.db")).unwrap();
    std::fs::rename(&replacement, &source).unwrap();

    let summary = crate::bootstrap::execute_tidy_migrations(&plan, &cfg, &authorities).unwrap();
    assert_eq!(summary.failed_count, 1);
    assert_eq!(summary.migrated_count, 0);
    assert!(summary.outcomes[0].message.contains("physical identity"));
    assert!(source.exists(), "replacement source must remain untouched");
    assert!(
        !std::path::Path::new(&plan[0].archive_path).exists(),
        "replacement source must refuse before archive mutation"
    );
    let target_store = MemoryStore::open_read_only(target.to_str().unwrap()).unwrap();
    assert_eq!(target_store.stats(true).unwrap().total, 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_execute_refuses_source_becoming_target_after_plan() {
    let (root, source, target, plan, authorities, cfg) =
        planned_mutation_authority_fixture("source-equals-target");

    std::fs::rename(&source, source.with_extension("scanned.db")).unwrap();
    std::fs::hard_link(&target, &source).unwrap();

    let summary = crate::bootstrap::execute_tidy_migrations(&plan, &cfg, &authorities).unwrap();
    assert_eq!(summary.failed_count, 1);
    assert_eq!(summary.migrated_count, 0);
    assert!(summary.outcomes[0]
        .message
        .contains("physically equal to target"));
    assert!(source.exists(), "source-now-target must never be archived");
    assert!(
        !std::path::Path::new(&plan[0].archive_path).exists(),
        "source-now-target must refuse before archive mutation"
    );
    let target_store = MemoryStore::open_read_only(target.to_str().unwrap()).unwrap();
    assert_eq!(target_store.stats(true).unwrap().total, 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_execute_archive_failure_rolls_back_overwritten_target_rows_atomically() {
    let (root, source, target, plan, authorities, cfg) =
        planned_mutation_authority_fixture("archive-failure-overlap");

    let mut target_original = make_entry("overlap-row");
    target_original.text = "target original must survive".to_string();
    let mut target_store = MemoryStore::open(target.to_str().unwrap()).unwrap();
    target_store.upsert(&target_original).unwrap();
    drop(target_store);

    let mut source_replacement = make_entry("overlap-row");
    source_replacement.text = "source replacement must roll back".to_string();
    let mut source_store = MemoryStore::open(source.to_str().unwrap()).unwrap();
    source_store.upsert(&source_replacement).unwrap();
    drop(source_store);

    let archive_path = std::path::PathBuf::from(&plan[0].archive_path);
    std::fs::create_dir_all(&archive_path)
        .expect("an existing directory forces the post-copy archive operation to fail");

    let summary = crate::bootstrap::execute_tidy_migrations(&plan, &cfg, &authorities)
        .expect("archive failure is represented by a failed migration outcome");
    assert_eq!(summary.failed_count, 1);
    let error = &summary.outcomes[0].message;
    assert!(
        error.contains("File exists"),
        "unexpected archive failure: {error}"
    );

    let target_after = MemoryStore::open_read_only(target.to_str().unwrap()).unwrap();
    let overlap = target_after
        .get("overlap-row")
        .unwrap()
        .expect("pre-existing overlap row survives");
    assert_eq!(overlap.text, "target original must survive");
    assert!(
        target_after.get("scanned-source-row").unwrap().is_none(),
        "new source rows must also roll back with the overwritten row"
    );
    assert!(source.exists(), "failed archive leaves source untouched");

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_execute_boundary_failure_keeps_live_source_after_archive_staging() {
    let (root, source, target, plan, authorities, cfg) =
        planned_mutation_authority_fixture("boundary-failure-source-retained");

    crate::bootstrap::force_boundary_failure_after_archive_stage(true);
    let summary = crate::bootstrap::execute_tidy_migrations(&plan, &cfg, &authorities)
        .expect("boundary failure is represented by a failed migration outcome");
    crate::bootstrap::force_boundary_failure_after_archive_stage(false);

    assert_eq!(summary.failed_count, 1, "{summary:?}");
    assert!(
        summary.outcomes[0]
            .message
            .contains("injected boundary failure after archive staging"),
        "the discriminator must fail after archive staging: {:?}",
        summary.outcomes[0]
    );
    assert!(
        source.exists(),
        "a post-staging boundary failure must leave the live source in place"
    );
    assert!(
        std::path::Path::new(&plan[0].archive_path).exists(),
        "the pre-commit archive copy may remain as redundant recovery evidence"
    );
    let target_after = MemoryStore::open_read_only(target.to_str().unwrap()).unwrap();
    assert!(
        target_after.get("scanned-source-row").unwrap().is_none(),
        "target transaction must roll back when the boundary fails"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// #1341-follow-up: the archive step moves main/-wal/-shm as three sequential,
/// non-atomic filesystem operations. If a live daemon still holds the
/// source DB open (or ownership can't be determined), migrating anyway
/// risks a torn archived copy. Injection drives `daemon_ownership`
/// deterministically instead of depending on a real lsof/daemon fixture.
#[cfg(unix)]
struct ClearOwnershipInject;
#[cfg(unix)]
impl Drop for ClearOwnershipInject {
    fn drop(&mut self) {
        crate::db_ownership::set_ownership_inject_for_test(None);
    }
}

#[cfg(unix)]
#[test]
fn tidy_execute_rolls_back_when_source_db_is_owned() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-owned-{}", uuid::Uuid::new_v4()));
    let home = root.clone();
    let app_home = home.join(".tachi");
    std::fs::create_dir_all(&app_home).expect("create app_home");
    let target_db = app_home.join("global").join("memory.db");

    std::fs::create_dir_all(target_db.parent().unwrap()).expect("create target parent");
    let mut tgt = MemoryStore::open(target_db.to_str().unwrap()).expect("open target");
    tgt.upsert(&make_entry("preexisting-target"))
        .expect("seed target");
    drop(tgt);

    let source_db = home.join("legacy-owned").join("memory.db");
    std::fs::create_dir_all(source_db.parent().unwrap()).expect("create source parent");
    let mut src = MemoryStore::open(source_db.to_str().unwrap()).expect("open source");
    src.upsert(&make_entry("owned-source-row"))
        .expect("seed source");
    drop(src);

    let archive_path = app_home
        .join("archive")
        .join("ts-owned-test")
        .join("legacy-owned-memory.db");
    let migration = crate::bootstrap::TidyMigration {
        source_path: source_db.display().to_string(),
        target_path: target_db.display().to_string(),
        archive_path: archive_path.display().to_string(),
        scope_suggestion: "test".to_string(),
        action: "review_for_legacy_migration".to_string(),
        source_row_count: 1,
        reason: "test fixture".to_string(),
    };
    let cfg = crate::bootstrap::MigrationConfig {
        target_db: target_db.clone(),
        manifest_path: app_home.join("manifest.json"),
        dry_run: false,
        interactive: false,
        app_home: app_home.clone(),
    };

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::Owned,
    ));

    let authorized_sources = authorized_plan_sources(std::slice::from_ref(&migration));
    let summary = crate::bootstrap::execute_tidy_migrations(
        std::slice::from_ref(&migration),
        &cfg,
        &authorized_sources,
    )
    .expect("execute summary");

    assert_eq!(summary.migrated_count, 0, "must not report migrated");
    assert_eq!(summary.failed_count, 1, "must report failed (refused)");
    assert_eq!(summary.outcomes.len(), 1);
    let outcome = &summary.outcomes[0];
    assert_eq!(outcome.status, "failed");
    assert!(
        outcome.message.contains("rolled back"),
        "message must say rows were rolled back, got: {}",
        outcome.message
    );
    assert!(
        outcome.message.contains("live daemon holds this DB open"),
        "message must name the live-daemon refusal reason, got: {}",
        outcome.message
    );
    assert!(outcome.archive_path.is_none());

    // Source must survive untouched — never archived while owned.
    assert!(
        source_db.exists(),
        "source DB must remain on disk when archiving was refused"
    );
    assert!(
        !archive_path.exists(),
        "no archive copy may exist when the migration was refused"
    );
    // Target must be rolled back to its pre-migration row count.
    let tgt_after =
        MemoryStore::open_read_only(target_db.to_str().unwrap()).expect("open target after");
    assert_eq!(
        tgt_after.stats(true).unwrap().total,
        1,
        "target rows must be rolled back to the preexisting count"
    );
    drop(tgt_after);

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_execute_rolls_back_when_source_db_ownership_is_unknown() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-unknown-{}", uuid::Uuid::new_v4()));
    let home = root.clone();
    let app_home = home.join(".tachi");
    std::fs::create_dir_all(&app_home).expect("create app_home");
    let target_db = app_home.join("global").join("memory.db");

    std::fs::create_dir_all(target_db.parent().unwrap()).expect("create target parent");
    let mut tgt = MemoryStore::open(target_db.to_str().unwrap()).expect("open target");
    tgt.upsert(&make_entry("preexisting-target"))
        .expect("seed target");
    drop(tgt);

    let source_db = home.join("legacy-unknown").join("memory.db");
    std::fs::create_dir_all(source_db.parent().unwrap()).expect("create source parent");
    let mut src = MemoryStore::open(source_db.to_str().unwrap()).expect("open source");
    src.upsert(&make_entry("unknown-source-row"))
        .expect("seed source");
    drop(src);

    let archive_path = app_home
        .join("archive")
        .join("ts-unknown-test")
        .join("legacy-unknown-memory.db");
    let migration = crate::bootstrap::TidyMigration {
        source_path: source_db.display().to_string(),
        target_path: target_db.display().to_string(),
        archive_path: archive_path.display().to_string(),
        scope_suggestion: "test".to_string(),
        action: "review_for_legacy_migration".to_string(),
        source_row_count: 1,
        reason: "test fixture".to_string(),
    };
    let cfg = crate::bootstrap::MigrationConfig {
        target_db: target_db.clone(),
        manifest_path: app_home.join("manifest.json"),
        dry_run: false,
        interactive: false,
        app_home: app_home.clone(),
    };

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::Unknown("lsof unavailable: test".to_string()),
    ));

    let authorized_sources = authorized_plan_sources(std::slice::from_ref(&migration));
    let summary = crate::bootstrap::execute_tidy_migrations(
        std::slice::from_ref(&migration),
        &cfg,
        &authorized_sources,
    )
    .expect("execute summary");

    assert_eq!(summary.failed_count, 1);
    let outcome = &summary.outcomes[0];
    assert!(
        outcome.message.contains("ownership undetermined"),
        "message must say ownership was undetermined, got: {}",
        outcome.message
    );
    assert!(source_db.exists(), "source DB must remain on disk");
    let tgt_after =
        MemoryStore::open_read_only(target_db.to_str().unwrap()).expect("open target after");
    assert_eq!(tgt_after.stats(true).unwrap().total, 1);
    drop(tgt_after);

    let _ = std::fs::remove_dir_all(&root);
}

/// Codex-review follow-up: `tidy`'s outer `DualDaemonLock` (held by
/// `run_tidy_command` around the whole `--execute` run) only covers
/// `target_db`'s scope. A migration `source_path` belonging to a DIFFERENT
/// scope has no lock protection from that outer hold — `migrate_single_db`
/// must acquire its own `DualDaemonLock` for `source_path` and treat a busy
/// source-scope lock the same as a live-daemon `Owned` probe result: roll
/// back and fail, source untouched. No ownership injection needed here —
/// the lock conflict is detected and the migration fails before the
/// lsof-based ownership probe ever runs.
#[test]
fn tidy_execute_rolls_back_when_source_scope_lock_is_held() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-srclock-{}", uuid::Uuid::new_v4()));
    let home = root.clone();
    let app_home = home.join(".tachi");
    std::fs::create_dir_all(&app_home).expect("create app_home");
    let target_db = app_home.join("global").join("memory.db");

    std::fs::create_dir_all(target_db.parent().unwrap()).expect("create target parent");
    let mut tgt = MemoryStore::open(target_db.to_str().unwrap()).expect("open target");
    tgt.upsert(&make_entry("preexisting-target"))
        .expect("seed target");
    drop(tgt);

    let source_db = home.join("legacy-src-locked").join("memory.db");
    std::fs::create_dir_all(source_db.parent().unwrap()).expect("create source parent");
    let mut src = MemoryStore::open(source_db.to_str().unwrap()).expect("open source");
    src.upsert(&make_entry("src-locked-row"))
        .expect("seed source");
    drop(src);

    // Simulate "source DB's own daemon is running": pre-acquire the scoped
    // lock for `source_db` (a different scope than `target_db`).
    let source_scoped_lock_path =
        crate::daemon_lock::scoped_daemon_lock_path(&app_home, &source_db);
    let target_scoped_lock_path =
        crate::daemon_lock::scoped_daemon_lock_path(&app_home, &target_db);
    assert_ne!(
        source_scoped_lock_path, target_scoped_lock_path,
        "fixture must exercise a genuinely different scope than the target"
    );
    let _source_daemon = crate::daemon_lock::DaemonLock::acquire(&source_scoped_lock_path)
        .expect("pre-acquire source-scope lock to simulate its own live daemon");

    let archive_path = app_home
        .join("archive")
        .join("ts-srclock-test")
        .join("legacy-src-locked-memory.db");
    let migration = crate::bootstrap::TidyMigration {
        source_path: source_db.display().to_string(),
        target_path: target_db.display().to_string(),
        archive_path: archive_path.display().to_string(),
        scope_suggestion: "test".to_string(),
        action: "review_for_legacy_migration".to_string(),
        source_row_count: 1,
        reason: "test fixture".to_string(),
    };
    let cfg = crate::bootstrap::MigrationConfig {
        target_db: target_db.clone(),
        manifest_path: app_home.join("manifest.json"),
        dry_run: false,
        interactive: false,
        app_home: app_home.clone(),
    };

    let authorized_sources = authorized_plan_sources(std::slice::from_ref(&migration));
    let summary = crate::bootstrap::execute_tidy_migrations(
        std::slice::from_ref(&migration),
        &cfg,
        &authorized_sources,
    )
    .expect("execute summary");

    assert_eq!(summary.migrated_count, 0, "must not report migrated");
    assert_eq!(summary.failed_count, 1, "must report failed (refused)");
    let outcome = &summary.outcomes[0];
    assert_eq!(outcome.status, "failed");
    assert!(
        outcome.message.contains("rolled back"),
        "message must say rows were rolled back, got: {}",
        outcome.message
    );
    assert!(
        outcome
            .message
            .contains("source DB's own daemon is running"),
        "message must name the source-scope lock conflict, got: {}",
        outcome.message
    );
    assert!(
        outcome.message.contains("scoped lock"),
        "message must name which lock kind conflicted, got: {}",
        outcome.message
    );
    assert!(outcome.archive_path.is_none());

    // Source must survive untouched — never archived while its own scope's
    // lock is held by someone else.
    assert!(
        source_db.exists(),
        "source DB must remain on disk when its scope's lock was busy"
    );
    assert!(
        !archive_path.exists(),
        "no archive copy may exist when the migration was refused"
    );
    let tgt_after =
        MemoryStore::open_read_only(target_db.to_str().unwrap()).expect("open target after");
    assert_eq!(
        tgt_after.stats(true).unwrap().total,
        1,
        "target rows must be rolled back to the preexisting count"
    );
    drop(tgt_after);

    drop(_source_daemon);
    let _ = std::fs::remove_dir_all(&root);
}

/// The regression this fix targets: `run_tidy_command` holds an outer
/// `DualDaemonLock` for `target_db`'s scope — which also holds the single,
/// non-scoped legacy lock file — for the ENTIRE `--execute` run (see
/// `crates/tachi-server/src/bootstrap/tidy/command.rs:39`, held via `_lock`
/// until after `execute_tidy_migrations` returns). Before this fix,
/// `migrate_single_db` re-acquired a full `DualDaemonLock` (scoped + legacy)
/// for `source_path` too; when `source_path` belongs to a DIFFERENT scope
/// than `target_db`, that second legacy attempt opens a fresh fd on the
/// SAME legacy lock file the outer lock already holds via another fd —
/// `flock(2)` is per open file description, not per process, so it
/// collides with itself and misreports `LegacyRunning { pid: self }`,
/// rolling back every cross-scope migration unconditionally.
///
/// This test reproduces the outer lock's real lifetime around the call
/// (acquire before, hold across `execute_tidy_migrations`, drop after) so it
/// actually exercises that collision: on the pre-fix code this test is red
/// (spurious rollback); after switching the source-side acquisition to
/// `ScopedDaemonLock` (scoped-only, no legacy re-attempt) it is green.
#[test]
fn tidy_execute_migrates_cross_scope_source_while_outer_target_lock_is_held() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-crossscope-{}", uuid::Uuid::new_v4()));
    let home = root.clone();
    let app_home = home.join(".tachi");
    std::fs::create_dir_all(&app_home).expect("create app_home");
    let target_db = app_home.join("global").join("memory.db");

    std::fs::create_dir_all(target_db.parent().unwrap()).expect("create target parent");
    let mut tgt = MemoryStore::open(target_db.to_str().unwrap()).expect("open target");
    tgt.upsert(&make_entry("preexisting-target"))
        .expect("seed target");
    drop(tgt);

    let source_db = home.join("legacy-cross-scope").join("memory.db");
    std::fs::create_dir_all(source_db.parent().unwrap()).expect("create source parent");
    let mut src = MemoryStore::open(source_db.to_str().unwrap()).expect("open source");
    src.upsert(&make_entry("cross-scope-row-1"))
        .expect("seed source 1");
    src.upsert(&make_entry("cross-scope-row-2"))
        .expect("seed source 2");
    drop(src);

    let source_scoped_lock_path =
        crate::daemon_lock::scoped_daemon_lock_path(&app_home, &source_db);
    let target_scoped_lock_path =
        crate::daemon_lock::scoped_daemon_lock_path(&app_home, &target_db);
    assert_ne!(
        source_scoped_lock_path, target_scoped_lock_path,
        "fixture must exercise a genuinely different scope than the target"
    );

    let archive_path = app_home
        .join("archive")
        .join("ts-crossscope-test")
        .join("legacy-cross-scope-memory.db");
    let migration = crate::bootstrap::TidyMigration {
        source_path: source_db.display().to_string(),
        target_path: target_db.display().to_string(),
        archive_path: archive_path.display().to_string(),
        scope_suggestion: "test".to_string(),
        action: "review_for_legacy_migration".to_string(),
        source_row_count: 2,
        reason: "test fixture".to_string(),
    };
    let cfg = crate::bootstrap::MigrationConfig {
        target_db: target_db.clone(),
        manifest_path: app_home.join("manifest.json"),
        dry_run: false,
        interactive: false,
        app_home: app_home.clone(),
    };

    // Simulate `run_tidy_command`'s real outer-lock lifetime: acquired
    // before the migration run, held across the whole
    // `execute_tidy_migrations` call, dropped after — exactly the window in
    // which the pre-fix source-side `DualDaemonLock::acquire` would have
    // self-collided on the shared legacy fd.
    let _outer_lock = crate::daemon_lock::DualDaemonLock::acquire(&app_home, &target_db)
        .expect("outer lock on target scope must succeed (nothing else holds it)");

    let authorized_sources = authorized_plan_sources(std::slice::from_ref(&migration));
    let summary = crate::bootstrap::execute_tidy_migrations(
        std::slice::from_ref(&migration),
        &cfg,
        &authorized_sources,
    )
    .expect("execute summary");

    drop(_outer_lock);

    let outcome = &summary.outcomes[0];
    assert_eq!(
        summary.migrated_count, 1,
        "cross-scope source with no holder must migrate successfully; outcome: {outcome:?}"
    );
    assert_eq!(summary.failed_count, 0, "outcome: {outcome:?}");
    assert_eq!(outcome.status, "migrated");
    assert!(
        !outcome.message.contains("rolled back"),
        "must not roll back a genuinely free cross-scope source, got: {}",
        outcome.message
    );
    assert!(
        !source_db.exists(),
        "source DB must be archived (moved) on a successful cross-scope migration"
    );
    assert!(archive_path.exists(), "archive copy must exist");
    let tgt_after =
        MemoryStore::open_read_only(target_db.to_str().unwrap()).expect("open target after");
    assert_eq!(
        tgt_after.stats(true).unwrap().total,
        3,
        "target must have preexisting (1) + migrated (2) rows"
    );
    drop(tgt_after);

    let _ = std::fs::remove_dir_all(&root);
}
