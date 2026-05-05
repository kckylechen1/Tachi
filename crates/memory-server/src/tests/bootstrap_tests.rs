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
