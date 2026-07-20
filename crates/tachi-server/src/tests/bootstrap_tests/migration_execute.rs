use super::*;

#[test]
fn tidy_execute_migrates_rows_and_archives_source() {
    let root = std::env::temp_dir().join(format!("tachi-tidy-exec-{}", uuid::Uuid::new_v4()));
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
    };
    let dry_summary =
        crate::bootstrap::execute_tidy_migrations(&plan, &dry_cfg).expect("dry-run summary");
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
    };
    let summary =
        crate::bootstrap::execute_tidy_migrations(&plan, &exec_cfg).expect("execute summary");
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
    let root = std::env::temp_dir().join(format!("tachi-tidy-owned-{}", uuid::Uuid::new_v4()));
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
    };

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(crate::db_ownership::DbOwnership::Owned));

    let summary = crate::bootstrap::execute_tidy_migrations(std::slice::from_ref(&migration), &cfg)
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
    let root = std::env::temp_dir().join(format!("tachi-tidy-unknown-{}", uuid::Uuid::new_v4()));
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
    };

    let _clear = ClearOwnershipInject;
    crate::db_ownership::set_ownership_inject_for_test(Some(
        crate::db_ownership::DbOwnership::Unknown("lsof unavailable: test".to_string()),
    ));

    let summary = crate::bootstrap::execute_tidy_migrations(std::slice::from_ref(&migration), &cfg)
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
