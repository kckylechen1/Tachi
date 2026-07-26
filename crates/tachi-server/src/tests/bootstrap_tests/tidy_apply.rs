use super::*;

#[cfg(unix)]
#[test]
fn tidy_apply_removes_broken_memory_db_symlink() {
    let root = crate::utils::test_fixture_path(format!(
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

#[test]
fn tidy_apply_writes_report_and_only_confirms_safe_actions() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-apply-{}", uuid::Uuid::new_v4()));
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
