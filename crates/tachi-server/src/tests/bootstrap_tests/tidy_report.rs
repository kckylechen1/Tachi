use super::*;

#[test]
fn tidy_report_scans_memory_dbs_and_suggests_scope() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-report-{}", uuid::Uuid::new_v4()));
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
