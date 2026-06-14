use super::*;

#[test]
fn setup_report_detects_readiness_from_local_state() {
    let home = std::env::temp_dir().join(format!("tachi-setup-report-{}", uuid::Uuid::new_v4()));
    let app_home = home.join(".tachi");
    let global_db = app_home.join("global").join("memory.db");
    let project_db = home.join("repo").join(".tachi").join("memory.db");
    let git_root = home.join("repo");

    std::fs::create_dir_all(app_home.join("skills").join("review-skill"))
        .expect("create tachi skill");
    std::fs::write(
        app_home
            .join("skills")
            .join("review-skill")
            .join("SKILL.md"),
        "# review",
    )
    .expect("write tachi skill");
    std::fs::create_dir_all(home.join(".claude")).expect("create claude dir");
    std::fs::write(home.join(".claude").join("mcp.json"), "{}").expect("write claude mcp");
    std::fs::create_dir_all(global_db.parent().unwrap()).expect("create global db dir");

    let env = HashMap::from([
        ("VOYAGE_API_KEY".to_string(), "voyage-test".to_string()),
        (
            "SILICONFLOW_API_KEY".to_string(),
            "siliconflow-test".to_string(),
        ),
        ("ENABLE_PIPELINE".to_string(), "true".to_string()),
    ]);

    let report = crate::bootstrap::build_setup_report(
        &home,
        &app_home,
        &global_db,
        Some(&project_db),
        Some(&git_root),
        &env,
    )
    .expect("setup report should build");

    assert_eq!(report.items.len(), 5);
    assert_eq!(report.items[0].id, "api_keys");
    assert_eq!(report.items[0].status, "ready");
    assert_eq!(report.items[1].id, "skills");
    assert_eq!(report.items[1].status, "ready");
    assert_eq!(report.items[2].id, "agents");
    assert_eq!(report.items[2].status, "ready");
    assert_eq!(report.items[3].id, "pipeline");
    assert_eq!(report.items[3].status, "ready");
    assert!(
        report.items[4]
            .details
            .iter()
            .any(|detail| detail.contains("vault: not initialized")),
        "expected vault to be reported as not initialized"
    );
    assert!(
        report
            .next_steps
            .iter()
            .any(|step| step.contains("vault_init")),
        "expected vault next step"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn tidy_report_scans_memory_dbs_and_suggests_scope() {
    let root = std::env::temp_dir().join(format!("tachi-tidy-report-{}", uuid::Uuid::new_v4()));
    let git_root = root.join("repo");
    let global_db = root.join(".tachi").join("global").join("memory.db");
    let project_db = git_root.join(".tachi").join("memory.db");
    let openclaw_plugin_db = root
        .join(".openclaw")
        .join("extensions")
        .join("tachi")
        .join("data")
        .join("agents")
        .join("main")
        .join("memory.db");
    let openclaw_backup_db = root
        .join(".openclaw")
        .join("backups")
        .join("snapshot-1")
        .join("local-plugin-memory-hybrid-bridge")
        .join("data")
        .join("agents")
        .join("ops")
        .join("memory.db");

    for db in [
        &global_db,
        &project_db,
        &openclaw_plugin_db,
        &openclaw_backup_db,
    ] {
        std::fs::create_dir_all(db.parent().unwrap()).expect("create db parent");
        let mut store =
            MemoryStore::open(db.to_str().expect("db path must be valid utf8")).expect("open db");
        let id = format!("entry-{}", db.display());
        let mut entry = make_entry(&id);
        entry.path = if db == &project_db {
            "/repo".to_string()
        } else {
            "/global".to_string()
        };
        store.upsert(&entry).expect("seed db");
    }

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), Some(&git_root))
        .expect("tidy report should build");

    assert_eq!(report.total_databases, 4);
    assert_eq!(report.total_memories, 4);
    assert!(report
        .databases
        .iter()
        .any(|db| db.path == project_db.display().to_string() && db.scope_suggestion == "project"));
    assert!(report
        .databases
        .iter()
        .any(|db| db.path == global_db.display().to_string() && db.scope_suggestion == "global"));
    assert!(report
        .databases
        .iter()
        .any(|db| db.path == openclaw_plugin_db.display().to_string()
            && db.scope_suggestion == "openclaw-plugin-agent:main"));
    assert!(report
        .databases
        .iter()
        .any(|db| db.path == openclaw_plugin_db.display().to_string()
            && db.recommended_action == "keep_separate_agent_db"));
    assert!(report
        .databases
        .iter()
        .any(|db| db.path == openclaw_backup_db.display().to_string()
            && db.scope_suggestion == "openclaw-backup-agent:ops"));
    assert!(report
        .databases
        .iter()
        .any(|db| db.path == openclaw_backup_db.display().to_string()
            && db.recommended_action == "archive_or_delete_after_review"));
    assert!(report.groups.iter().any(|group| group.group == "global"
        && group.database_count == 1
        && group.memory_count == 1));
    assert!(report.groups.iter().any(|group| group.group == "project"
        && group.database_count == 1
        && group.memory_count == 1));
    assert!(report
        .groups
        .iter()
        .any(|group| group.group == "openclaw-plugin-agent"
            && group.database_count == 1
            && group.memory_count == 1));
    assert!(report
        .groups
        .iter()
        .any(|group| group.group == "openclaw-backup-agent"
            && group.database_count == 1
            && group.memory_count == 1));
    assert_eq!(
        report.groups.first().map(|group| group.group.as_str()),
        Some("openclaw-plugin-agent")
    );
    assert_eq!(
        report
            .databases
            .first()
            .map(|db| db.scope_suggestion.as_str()),
        Some("openclaw-plugin-agent:main")
    );
    assert_eq!(report.dry_run_plan.len(), 4);
    assert_eq!(report.dry_run_plan[0].order, 1);
    assert_eq!(report.dry_run_plan[0].scope, "openclaw-plugin-agent:main");
    assert_eq!(report.dry_run_plan[0].action, "keep_separate_agent_db");
    assert_eq!(
        report.dry_run_plan[0].target_label,
        "openclaw-plugin-agent:main"
    );
    assert!(report.dry_run_plan[0]
        .rationale
        .contains("should stay separate"));
    assert_eq!(report.dry_run_plan[3].scope, "openclaw-backup-agent:ops");
    assert_eq!(
        report.dry_run_plan[3].action,
        "archive_or_delete_after_review"
    );
    assert_eq!(report.dry_run_plan[3].target_label, "archive");

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_apply_removes_broken_memory_db_symlink() {
    let root = std::env::temp_dir().join(format!(
        "tachi-tidy-broken-symlink-{}",
        uuid::Uuid::new_v4()
    ));
    let app_home = root.join(".tachi-home");
    let link = root
        .join(".tachi")
        .join("projects")
        .join("stale")
        .join("memory.db");
    let missing_target = root.join("missing").join("memory.db");
    std::fs::create_dir_all(link.parent().unwrap()).expect("create symlink parent");
    std::os::unix::fs::symlink(&missing_target, &link).expect("create broken symlink");

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), None)
        .expect("tidy report should build");
    let finding = report
        .databases
        .iter()
        .find(|db| db.path == link.display().to_string())
        .expect("broken symlink should be included");
    assert_eq!(finding.status, "broken_symlink");
    assert_eq!(finding.recommended_action, "remove_broken_symlink");
    assert_eq!(finding.target_exists, Some(false));
    assert!(finding.is_symlink);

    let summary = crate::bootstrap::execute_tidy_apply(&app_home, &report)
        .expect("apply summary should build");
    assert!(summary
        .applied_steps
        .iter()
        .any(|step| step.action == "remove_broken_symlink" && step.outcome == "cleaned"));
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "broken symlink should be removed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Phase 5: fragment-DB consolidation (`tachi tidy --execute`)
// ---------------------------------------------------------------------------

fn build_legacy_openclaw_fixture(
    root: &std::path::Path,
    rows_per_db: usize,
) -> (std::path::PathBuf, std::path::PathBuf) {
    // One legacy OpenClaw "core-extensions" agent DB that should migrate.
    let legacy_db = root
        .join(".openclaw")
        .join("core")
        .join("extensions")
        .join("tachi")
        .join("data")
        .join("agents")
        .join("legacy-one")
        .join("memory.db");
    let legacy_db2 = root
        .join(".openclaw")
        .join("core")
        .join("extensions")
        .join("memory-hybrid-bridge")
        .join("data")
        .join("agents")
        .join("legacy-two")
        .join("memory.db");

    for db in [&legacy_db, &legacy_db2] {
        std::fs::create_dir_all(db.parent().unwrap()).expect("create db parent");
        let mut store =
            MemoryStore::open(db.to_str().expect("db path utf8")).expect("open legacy db");
        // Use the parent directory name to keep ids unique across DBs (the
        // file name is just "memory.db" for both).
        let tag = db
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        for i in 0..rows_per_db {
            let id = format!("{tag}-row-{i}");
            store.upsert(&make_entry(&id)).expect("seed legacy db");
        }
    }
    (legacy_db, legacy_db2)
}

#[test]
fn migration_plan_targets_only_legacy_dbs_and_skips_keep_actions() {
    let root = std::env::temp_dir().join(format!("tachi-tidy-plan-{}", uuid::Uuid::new_v4()));
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
    let root = std::env::temp_dir().join(format!("tachi-tidy-manifest-{}", uuid::Uuid::new_v4()));
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

#[test]
fn tidy_apply_writes_report_and_only_confirms_safe_actions() {
    let root = std::env::temp_dir().join(format!("tachi-tidy-apply-{}", uuid::Uuid::new_v4()));
    let git_root = root.join("repo");
    let app_home = root.join(".tachi-home");
    let global_db = root.join(".tachi").join("global").join("memory.db");
    let project_db = git_root.join(".tachi").join("memory.db");
    let openclaw_backup_db = root
        .join(".openclaw")
        .join("backups")
        .join("snapshot-1")
        .join("local-plugin-memory-hybrid-bridge")
        .join("data")
        .join("agents")
        .join("ops")
        .join("memory.db");

    for db in [&global_db, &project_db, &openclaw_backup_db] {
        std::fs::create_dir_all(db.parent().unwrap()).expect("create db parent");
        let mut store =
            MemoryStore::open(db.to_str().expect("db path must be valid utf8")).expect("open db");
        let id = format!("entry-{}", db.display());
        store.upsert(&make_entry(&id)).expect("seed db");
    }

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), Some(&git_root))
        .expect("tidy report should build");
    let summary = crate::bootstrap::execute_tidy_apply(&app_home, &report)
        .expect("apply summary should build");

    assert_eq!(summary.applied_count, 2);
    assert_eq!(summary.skipped_count, 1);
    assert!(summary
        .applied_steps
        .iter()
        .any(|step| step.scope == "project" && step.outcome == "confirmed"));
    assert!(summary
        .applied_steps
        .iter()
        .any(|step| step.scope == "global" && step.outcome == "confirmed"));
    assert!(summary
        .applied_steps
        .iter()
        .any(|step| step.scope == "openclaw-backup-agent:ops" && step.outcome == "skipped"));
    assert!(
        std::path::Path::new(&summary.report_path).exists(),
        "expected apply report artifact"
    );

    let _ = std::fs::remove_dir_all(&root);
}
