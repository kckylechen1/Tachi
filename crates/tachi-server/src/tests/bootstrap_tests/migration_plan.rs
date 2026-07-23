use super::*;

#[test]
fn migration_plan_targets_only_legacy_dbs_and_skips_keep_actions() {
    let root = crate::utils::test_fixture_path(format!("tachi-tidy-plan-{}", uuid::Uuid::new_v4()));
    let home = root.clone();
    let target_db = home.join(".tachi").join("global").join("memory.db");
    let archive_root = home.join(".tachi").join("archive").join("ts-fake");

    // Seed: 1 global (keep), 1 project (keep), 2 legacy (migrate).
    let global_db = home.join(".tachi").join("global").join("memory.db");
    let project_db = home.join("repo").join(".tachi").join("memory.db");
    for db in [&global_db, &project_db] {
        std::fs::create_dir_all(db.parent().unwrap()).expect("create dir");
        let mut s = MemoryStore::open(db.to_str().unwrap()).expect("open");
        s.upsert(&make_entry(&format!("seed-{}", db.display())))
            .expect("seed");
    }
    let (legacy_a, legacy_b) = build_legacy_openclaw_fixture(&home, 2);

    let report = crate::bootstrap::build_tidy_report(&[home.clone()], Some(&home.join("repo")))
        .expect("build report");
    let plan = crate::bootstrap::build_migration_plan(&report, &target_db, &archive_root, &home);

    let plan_sources: std::collections::HashSet<String> =
        plan.iter().map(|m| m.source_path.clone()).collect();
    assert!(plan_sources.contains(&legacy_a.display().to_string()));
    assert!(plan_sources.contains(&legacy_b.display().to_string()));
    assert!(!plan_sources.contains(&global_db.display().to_string()));
    assert!(!plan_sources.contains(&project_db.display().to_string()));
    for m in &plan {
        assert_eq!(m.action, "review_for_legacy_migration");
        assert_eq!(m.target_path, target_db.display().to_string());
        assert!(m
            .archive_path
            .starts_with(&archive_root.display().to_string()));
        assert_eq!(m.source_row_count, 2);
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn manifest_update_drops_migrated_sources_and_inserts_target() {
    let root = crate::utils::test_fixture_path(format!("tachi-tidy-manifest-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join(".tachi")).expect("create .tachi");
    let manifest_path = root.join(".tachi").join("manifest.json");

    // Seed manifest with a "legacy" entry that should be removed and a stray
    // entry that should be preserved.
    let initial = serde_json::json!({
        "schema_version": 1,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "dbs": [
            {
                "path": "/tmp/legacy/memory.db",
                "role": "agent",
                "owner": "openclaw",
                "schema_kind": "tachi-memory-v1",
                "vec_enabled": false,
                "allow_write": false,
                "last_doctor_at": "",
                "last_classification": "legacy",
                "scope_hint": "openclaw-legacy-agent:foo"
            },
            {
                "path": "/tmp/keep/memory.db",
                "role": "project",
                "owner": "tachi",
                "schema_kind": "tachi-memory-v1",
                "vec_enabled": true,
                "allow_write": true,
                "last_doctor_at": "",
                "last_classification": "healthy",
                "scope_hint": "project"
            }
        ]
    });
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&initial).unwrap())
        .expect("write manifest");

    let cfg = crate::bootstrap::MigrationConfig {
        target_db: root.join(".tachi").join("global").join("memory.db"),
        manifest_path: manifest_path.clone(),
        dry_run: false,
        interactive: false,
        app_home: root.join(".tachi"),
    };
    let outcomes = vec![crate::bootstrap::TidyMigrationOutcome {
        source_path: "/tmp/legacy/memory.db".to_string(),
        target_path: cfg.target_db.display().to_string(),
        archive_path: Some("/tmp/archive/legacy/memory.db".to_string()),
        status: "migrated".to_string(),
        rows_before_target: 0,
        rows_after_target: 2,
        rows_copied: 2,
        message: "ok".to_string(),
    }];

    crate::bootstrap::update_manifest_after_migration(&cfg, &outcomes)
        .expect("manifest update should succeed");

    let updated = crate::manifest::Manifest::load(&manifest_path).expect("load updated manifest");
    let paths: Vec<&str> = updated.dbs.iter().map(|e| e.path.as_str()).collect();
    assert!(!paths.contains(&"/tmp/legacy/memory.db"));
    assert!(paths.contains(&"/tmp/keep/memory.db"));
    // Target entry was inserted.
    assert!(updated
        .dbs
        .iter()
        .any(|e| e.path.ends_with("global/memory.db") && e.scope_hint == "global"));

    let _ = std::fs::remove_dir_all(&root);
}
