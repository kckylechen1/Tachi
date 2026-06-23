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
