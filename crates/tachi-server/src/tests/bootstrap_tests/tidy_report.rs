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

#[cfg(unix)]
#[test]
fn tidy_report_counts_physical_db_once_across_path_symlink_and_hardlink() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-identity-{}", uuid::Uuid::new_v4()));
    let agents = root.join(".openclaw/core/extensions/tachi/data/agents");
    let real_db = agents.join("real/memory.db");
    let symlink_db = agents.join("symlink/memory.db");
    let hardlink_db = agents.join("hardlink/memory.db");
    for path in [&real_db, &symlink_db, &hardlink_db] {
        std::fs::create_dir_all(path.parent().unwrap()).expect("create alias parent");
    }

    let mut store = MemoryStore::open(real_db.to_str().unwrap()).expect("open physical DB");
    store
        .upsert(&make_entry("one-physical-memory"))
        .expect("seed physical DB");
    drop(store);
    std::os::unix::fs::symlink(&real_db, &symlink_db).expect("create DB symlink");
    std::fs::hard_link(&real_db, &hardlink_db).expect("create DB hard link");

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), None)
        .expect("identity-aware tidy report");
    assert_eq!(report.total_databases, 1, "physical DBs, not path aliases");
    assert_eq!(report.total_aliases, 3);
    assert_eq!(report.resolved_aliases, 3);
    assert_eq!(report.unresolved_paths, 0);
    assert_eq!(report.path_appearances, 3);
    assert_eq!(
        report.total_memories, 1,
        "physical rows must be counted once"
    );
    assert_eq!(report.physical_stores.len(), 1);
    assert_eq!(report.physical_stores[0].aliases.len(), 3);

    let doctor = crate::doctor::scan(
        std::slice::from_ref(&root),
        &root.join("quarantine"),
        crate::doctor::ScanOptions::default(),
    );
    assert_eq!(doctor.summary.total_databases, report.total_databases);
    assert_eq!(doctor.summary.total_memories, report.total_memories);
    assert_eq!(doctor.summary.total_aliases, report.total_aliases);
    assert_eq!(doctor.summary.resolved_aliases, report.resolved_aliases);
    assert_eq!(doctor.summary.unresolved_paths, report.unresolved_paths);
    assert_eq!(doctor.summary.path_appearances, report.path_appearances);

    let target = root.join(".tachi/global/memory.db");
    let archive = root.join(".tachi/archive/test");
    let plan = crate::bootstrap::build_migration_plan(&report, &target, &archive, &root);
    assert_eq!(
        plan.len(),
        1,
        "aliases must not duplicate migration sources"
    );
    let self_plan = crate::bootstrap::build_migration_plan(&report, &symlink_db, &archive, &root);
    assert!(
        self_plan.is_empty(),
        "an alias of the target must never be planned as a migration source"
    );

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["total_databases"], 1);
    assert_eq!(json["total_aliases"], 3);
    assert_eq!(json["resolved_aliases"], 3);
    assert_eq!(json["unresolved_paths"], 0);
    assert_eq!(json["path_appearances"], 3);
    assert_eq!(
        json["physical_stores"][0]["aliases"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_report_does_not_count_unresolved_paths_as_aliases() {
    let root = crate::utils::test_fixture_path(format!(
        "tachi-tidy-unresolved-count-{}",
        uuid::Uuid::new_v4()
    ));
    let broken = root.join(".tachi/global/memory.db");
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.join("missing.sqlite"), &broken).unwrap();

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), None).unwrap();
    assert_eq!(report.total_databases, 0);
    assert_eq!(report.total_aliases, 0);
    assert_eq!(report.resolved_aliases, 0);
    assert_eq!(report.unresolved_paths, 1);
    assert_eq!(report.path_appearances, 1);
    assert_eq!(report.databases.len(), 1);

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["total_aliases"], 0);
    assert_eq!(json["resolved_aliases"], 0);
    assert_eq!(json["unresolved_paths"], 1);
    assert_eq!(json["path_appearances"], 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_report_reads_committed_wal_while_writer_owns_database() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-live-wal-{}", uuid::Uuid::new_v4()));
    let db = root.join(".tachi/global/memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).expect("create DB parent");
    let mut store = MemoryStore::open(db.to_str().unwrap()).expect("open live DB");
    store.upsert(&make_entry("checkpointed")).expect("seed row");
    drop(store);

    let writer = rusqlite::Connection::open(&db).expect("open writer");
    writer
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .expect("enable retained WAL");
    writer
        .execute(
            "UPDATE memories SET summary = 'committed in WAL' WHERE id = 'checkpointed'",
            [],
        )
        .expect("commit update into WAL");
    writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold writable ownership");

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), None)
        .expect("read-only inventory must coexist with writer");
    assert_eq!(report.total_databases, 1);
    assert_eq!(report.total_memories, 1);
    assert_eq!(report.databases[0].status, "ok");

    writer.execute_batch("ROLLBACK").expect("release writer");
    drop(writer);
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_report_prefers_hardlink_alias_with_active_wal_sidecars() {
    let root = crate::utils::test_fixture_path(format!(
        "tachi-tidy-hardlink-wal-{}",
        uuid::Uuid::new_v4()
    ));
    let agents = root.join(".openclaw/core/extensions/tachi/data/agents");
    let hardlink = agents.join("a-hardlink/memory.db");
    let live = agents.join("z-live/memory.db");
    std::fs::create_dir_all(hardlink.parent().unwrap()).unwrap();
    std::fs::create_dir_all(live.parent().unwrap()).unwrap();

    let mut seed = MemoryStore::open(live.to_str().unwrap()).unwrap();
    seed.upsert(&make_entry("checkpointed")).unwrap();
    drop(seed);
    std::fs::hard_link(&live, &hardlink).unwrap();

    let mut live_owner = MemoryStore::open(live.to_str().unwrap()).unwrap();
    live_owner
        .upsert(&make_entry("committed-in-live-wal"))
        .unwrap();
    assert!(live.with_file_name("memory.db-wal").exists());

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), None).unwrap();
    assert_eq!(report.total_databases, 1);
    assert_eq!(
        report.total_memories, 2,
        "must read the alias owning live WAL"
    );
    assert_eq!(
        report.physical_stores[0].primary_path,
        live.display().to_string()
    );
    assert_eq!(
        report.physical_stores[0].open_path_basis,
        crate::physical_db_identity::OpenPathBasis::WalAndShmVisible
    );
    assert!(report.physical_stores[0]
        .sidecar_paths
        .contains(&live.display().to_string()));
    assert!(!report.physical_stores[0]
        .sidecar_paths
        .contains(&hardlink.display().to_string()));

    drop(live_owner);
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn tidy_report_refuses_mutation_authority_for_multiple_live_hardlink_sidecars() {
    let root = crate::utils::test_fixture_path(format!(
        "tachi-tidy-ambiguous-sidecars-{}",
        uuid::Uuid::new_v4()
    ));
    let agents = root.join(".openclaw/core/extensions/tachi/data/agents");
    let first = agents.join("a-first/memory.db");
    let second = agents.join("b-second/memory.db");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();

    let mut seed = MemoryStore::open(first.to_str().unwrap()).unwrap();
    seed.upsert(&make_entry("checkpointed")).unwrap();
    drop(seed);
    std::fs::hard_link(&first, &second).unwrap();

    let mut owner = MemoryStore::open(first.to_str().unwrap()).unwrap();
    owner.upsert(&make_entry("committed-in-first-wal")).unwrap();
    let first_wal = first.with_file_name("memory.db-wal");
    let first_shm = first.with_file_name("memory.db-shm");
    assert!(first_wal.exists());
    assert!(first_shm.exists());
    std::fs::copy(&first_wal, second.with_file_name("memory.db-wal")).unwrap();
    std::fs::copy(&first_shm, second.with_file_name("memory.db-shm")).unwrap();

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), None).unwrap();
    assert_eq!(report.total_databases, 1);
    assert_eq!(report.physical_stores[0].sidecar_paths.len(), 2);
    assert_eq!(
        report.physical_stores[0].mutation_state,
        crate::physical_db_identity::PhysicalStoreMutationState::AmbiguousPhysicalStore
    );
    assert!(
        crate::bootstrap::authorized_migration_sources(&report).is_empty(),
        "multiple live sidecar owners must not mint a migration authority"
    );
    let plan = crate::bootstrap::build_migration_plan(
        &report,
        &root.join(".tachi/global/memory.db"),
        &root.join(".tachi/archive/ambiguous"),
        &root,
    );
    assert!(
        plan.is_empty(),
        "ambiguous store must have no mutation plan"
    );

    drop(owner);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tidy_report_preserves_open_error_and_adds_typed_failure() {
    let root =
        crate::utils::test_fixture_path(format!("tachi-tidy-corrupt-{}", uuid::Uuid::new_v4()));
    let db = root.join(".tachi/global/memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"not a sqlite database").unwrap();

    let report = crate::bootstrap::build_tidy_report(std::slice::from_ref(&root), None).unwrap();
    assert_eq!(report.total_databases, 1);
    assert_eq!(report.databases[0].status, "open_error");
    assert_eq!(
        report.databases[0].open_failure_kind,
        Some(crate::physical_db_identity::InventoryFailureKind::NotDatabase)
    );

    let _ = std::fs::remove_dir_all(&root);
}
