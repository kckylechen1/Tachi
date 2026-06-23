use super::*;
use crate::manifest::DbRole;
use chrono::{DateTime, Utc};

fn entry(role: DbRole, scope_hint: &str) -> crate::manifest::DbEntry {
    crate::manifest::DbEntry {
        path: "/tmp/status/memory.db".to_string(),
        role,
        owner: "test".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: String::new(),
        last_classification: "healthy".to_string(),
        scope_hint: scope_hint.to_string(),
        notes: String::new(),
    }
}

#[test]
fn orphan_classification_matches_scheduler_routing() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("TACHI_HOME");
    std::env::set_var("TACHI_HOME", "/tmp/status-tachi-home");
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let project = PathBuf::from("/tmp/status/project/memory.db");
    let named = PathBuf::from("/tmp/status-tachi-home/projects/sigil/memory.db");
    let agent = PathBuf::from("/tmp/status-tachi-home/agents/main/memory.db");
    assert!(!is_orphan_entry(
        &entry(DbRole::Global, "global"),
        &global,
        &global,
        Some(&project)
    ));
    assert!(!is_orphan_entry(
        &entry(DbRole::Project, "project"),
        &project,
        &global,
        Some(&project)
    ));
    assert!(!is_orphan_entry(
        &entry(DbRole::Project, "project:sigil"),
        &named,
        &global,
        Some(&project)
    ));
    assert!(!is_orphan_entry(
        &entry(DbRole::Agent, "openclaw-agent:main"),
        &agent,
        &global,
        Some(&project)
    ));
    if let Some(v) = saved {
        std::env::set_var("TACHI_HOME", v);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn daemon_mismatch_detects_foreign_version() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(42),
        port: Some(6888),
        version: Some("1.3.0".to_string()),
        global_db: Some(global.display().to_string()),
    };
    let reason = daemon_mismatch_reason(42, Some(&info), &global)
        .expect("foreign version should be reported");
    assert!(reason.contains("1.3.0"));
    assert!(reason.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn daemon_mismatch_detects_pid_file_lock_pid_disagreement() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(7),
        port: Some(6919),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        global_db: Some(global.display().to_string()),
    };
    let reason = daemon_mismatch_reason(42, Some(&info), &global)
        .expect("pid disagreement should be reported");
    assert!(reason.contains("pid=7"));
    assert!(reason.contains("lock pid=42"));
}

#[test]
fn daemon_mismatch_accepts_matching_daemon_pid_file() {
    let global = PathBuf::from("/tmp/status/global/memory.db");
    let info = DaemonPidInfo {
        pid: Some(42),
        port: Some(6919),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        global_db: Some(global.display().to_string()),
    };
    assert!(daemon_mismatch_reason(42, Some(&info), &global).is_none());
}

#[test]
fn collect_daemon_status_prefers_scoped_match_over_legacy_foreign() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let global = app_home.path().join("global").join("memory.db");
    let foreign_global = app_home.path().join("openclaw").join("memory.db");
    let pid = std::process::id();

    let scoped_lock = crate::daemon_lock::scoped_daemon_lock_path(app_home.path(), &global);
    let scoped_pid = crate::daemon_lock::scoped_daemon_pid_path(app_home.path(), &global);
    std::fs::write(&scoped_lock, pid.to_string()).expect("scoped lock");
    std::fs::write(
        &scoped_pid,
        serde_json::json!({
            "pid": pid,
            "port": 7001,
            "version": env!("CARGO_PKG_VERSION"),
            "global_db": global.display().to_string(),
        })
        .to_string(),
    )
    .expect("scoped pid");
    std::fs::write(
        crate::daemon_lock::legacy_daemon_lock_path(app_home.path()),
        pid.to_string(),
    )
    .expect("legacy lock");
    std::fs::write(
        crate::daemon_lock::legacy_daemon_pid_path(app_home.path()),
        serde_json::json!({
            "pid": pid,
            "port": 6919,
            "version": env!("CARGO_PKG_VERSION"),
            "global_db": foreign_global.display().to_string(),
        })
        .to_string(),
    )
    .expect("legacy pid");

    match collect_daemon_status(app_home.path(), &global) {
        DaemonStatus::Running { lock_path, .. } => assert_eq!(lock_path, scoped_lock),
        other => panic!("expected scoped daemon to win, got {other:?}"),
    }
}

#[test]
fn collect_daemon_status_reports_legacy_foreign_without_scoped_match() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let global = app_home.path().join("global").join("memory.db");
    let foreign_global = app_home.path().join("openclaw").join("memory.db");
    let pid = std::process::id();

    std::fs::write(
        crate::daemon_lock::legacy_daemon_lock_path(app_home.path()),
        pid.to_string(),
    )
    .expect("legacy lock");
    std::fs::write(
        crate::daemon_lock::legacy_daemon_pid_path(app_home.path()),
        serde_json::json!({
            "pid": pid,
            "port": 6919,
            "version": env!("CARGO_PKG_VERSION"),
            "global_db": foreign_global.display().to_string(),
        })
        .to_string(),
    )
    .expect("legacy pid");

    match collect_daemon_status(app_home.path(), &global) {
        DaemonStatus::Foreign { reason, .. } => assert!(reason.contains("does not match")),
        other => panic!("expected legacy foreign daemon, got {other:?}"),
    }
}

#[test]
fn truncate_honors_max() {
    assert_eq!(truncate("abc", 5), "abc");
    assert_eq!(truncate("abcdefghij", 5), "ab...");
}

#[test]
fn read_distill_marker_parses_json_quality_summary() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let runs = app_home.path().join("foundry-runs");
    std::fs::create_dir_all(&runs).expect("runs dir");
    std::fs::write(
        runs.join(".last_distill_run"),
        serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "groups_distilled": 7,
            "groups_skipped": 2,
            "fallback_used": 1,
            "errors": 3,
        })
        .to_string(),
    )
    .expect("write marker");

    let marker = read_distill_marker(app_home.path()).expect("marker parsed");
    assert_eq!(marker.groups_distilled, Some(7));
    assert_eq!(marker.groups_skipped, Some(2));
    assert_eq!(marker.fallback_used, Some(1));
    assert_eq!(marker.errors, Some(3));
    assert!(!marker.is_stale, "fresh marker must not be stale");
}

#[test]
fn read_distill_marker_accepts_legacy_bare_timestamp() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let runs = app_home.path().join("foundry-runs");
    std::fs::create_dir_all(&runs).expect("runs dir");
    // Pre-JSON markers were a bare RFC3339 string; they must still parse with
    // every quality field left None (not be mistaken for a JSON document).
    std::fs::write(runs.join(".last_distill_run"), Utc::now().to_rfc3339())
        .expect("write legacy marker");

    let marker = read_distill_marker(app_home.path()).expect("legacy marker parsed");
    assert_eq!(marker.groups_distilled, None);
    assert_eq!(marker.errors, None);
    assert!(!marker.is_stale);
}

#[test]
fn distill_hard_errors_dock_health_but_fallbacks_do_not() {
    let base = DistillMarkerStatus {
        path: "/tmp/marker".to_string(),
        last_run_at: "2026-06-20T00:00:00Z".to_string(),
        age_seconds: 0,
        age: "0s ago".to_string(),
        is_stale: false,
        groups_distilled: Some(5),
        groups_skipped: Some(4),
        fallback_used: Some(9),
        errors: Some(0),
    };
    let with_errors = DistillMarkerStatus {
        errors: Some(2),
        ..base.clone()
    };
    let daemon = DaemonStatus::Running {
        pid: 1,
        lock_path: PathBuf::from("/tmp/tachi.lock"),
    };
    let healthy =
        status_health::calculate_health_score(&daemon, &[], Some(&base), &[], Some(&[]), Some(&[]));
    let errored = status_health::calculate_health_score(
        &daemon,
        &[],
        Some(&with_errors),
        &[],
        Some(&[]),
        Some(&[]),
    );
    // High fallback/skip counts alone keep a perfect score (graceful degradation).
    assert_eq!(
        healthy, 100,
        "fallback_used/groups_skipped must not be scored"
    );
    assert!(
        errored < healthy,
        "hard distill errors must dock the health score (got {errored} vs {healthy})"
    );
}

#[test]
fn infer_provider_from_auth_error_maps_real_failures() {
    assert_eq!(
        status_health::infer_provider_from_failed_job(
            "recall_rerank_cache",
            Some("rerank"),
            "SiliconFlow 403 forbidden"
        ),
        Some("SILICONFLOW".to_string())
    );
    assert_eq!(
        status_health::infer_provider_from_auth_error("Voyage API error: 403 Forbidden"),
        Some("VOYAGE".to_string())
    );
    assert_eq!(
        status_health::infer_provider_from_failed_job(
            "memory_distill",
            Some("distill"),
            "403 Forbidden"
        ),
        Some("SILICONFLOW".to_string())
    );
    assert_eq!(
        status_health::infer_provider_from_auth_error("network timeout"),
        None
    );
}

fn vector_health_entry(id: &str, source: &str, vector: Option<Vec<f32>>) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: format!("/scratch/status/{id}"),
        summary: "summary".to_string(),
        text: "status vector health test memory".to_string(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "status".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: source.to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({}),
        vector,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn vector_health_excludes_recall_cache_rows_from_coverage() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let db = dir.path().join("memory.db");
    let mut store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");

    store
        .upsert(&vector_health_entry(
            "normal-with-vector",
            "manual",
            Some(vec![0.1; EXPECTED_EMBEDDING_DIM]),
        ))
        .expect("insert vector row");
    store
        .upsert(&vector_health_entry("normal-missing", "manual", None))
        .expect("insert missing row");
    store
        .upsert(&vector_health_entry(
            "cache-missing",
            FOUNDRY_RECALL_CACHE_SOURCE,
            None,
        ))
        .expect("insert cache row");
    let mut cache_by_path = vector_health_entry("cache-by-path", "manual", None);
    cache_by_path.path = "/scratch/recall-cache/vector-health".to_string();
    cache_by_path.topic = "recall_rerank_cache".to_string();
    store.upsert(&cache_by_path).expect("insert path cache row");

    let health = vector_health(store.connection()).expect("vector health");
    assert_eq!(health.total, 2);
    assert_eq!(health.with_vec, 1);
    assert_eq!(health.missing, 1);
    assert_eq!(health.pending_enrichment, 1);
}

#[test]
fn collect_snapshot_surfaces_plan_c_split_brain_warning() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("TACHI_HOME");
    let dir = tempfile::tempdir().expect("temp db dir");
    let app_home = dir.path().join("home");
    std::env::set_var("TACHI_HOME", &app_home);

    let global_db = app_home.join("global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global parent"))
        .expect("create global parent");
    MemoryStore::open(global_db.to_str().expect("global path")).expect("open global");

    let repo = dir.path().join("Split Brain Repo");
    let local_db = repo.join(".tachi/memory.db");
    std::fs::create_dir_all(local_db.parent().expect("local parent")).expect("create local parent");
    MemoryStore::open(local_db.to_str().expect("local path")).expect("open local");

    let alias_db = crate::path_utils::plan_c_global_db_path("Split_Brain_Repo");
    std::fs::create_dir_all(alias_db.parent().expect("alias parent")).expect("create alias parent");
    MemoryStore::open(alias_db.to_str().expect("alias path")).expect("open alias");

    let snapshot = collect_snapshot(&app_home, &global_db, Some(&local_db));
    assert_eq!(snapshot.plan_c_split_brain.len(), 1);
    assert_eq!(
        snapshot.plan_c_split_brain[0].project_name,
        "Split_Brain_Repo"
    );

    let warnings = build_status_warnings(&snapshot, &daemon_running());
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("Plan C split-brain detected")),
        "expected Plan C warning in {warnings:?}"
    );

    if let Some(value) = saved {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn namespace_health_counts_cache_wiki_derived_and_graph_rows() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let db = dir.path().join("memory.db");
    let mut store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");

    store
        .upsert(&vector_health_entry("normal", "manual", None))
        .expect("insert normal row");

    let mut cache = vector_health_entry(
        "foundry:recall-cache:noise",
        FOUNDRY_RECALL_CACHE_SOURCE,
        None,
    );
    cache.path = "/scratch/recall-cache/noise".to_string();
    cache.topic = "recall_rerank_cache".to_string();
    store.upsert(&cache).expect("insert cache row");

    let mut wiki = vector_health_entry("wiki-legacy", "manual", None);
    wiki.path = "/wiki/engineering/legacy".to_string();
    wiki.category = "experience".to_string();
    wiki.domain = None;
    store.upsert(&wiki).expect("insert wiki row");

    store
        .connection()
        .execute(
            "INSERT INTO memory_edges
                 (source_id, target_id, relation, weight, metadata, created_at)
                 VALUES ('normal', 'missing-target', 'related_to', 1.0, '{}', ?1)",
            [Utc::now().to_rfc3339()],
        )
        .expect("insert orphan edge");

    let health = namespace_health(store.connection()).expect("namespace health");
    assert_eq!(health.recall_cache_rows, 1);
    assert_eq!(health.wiki_rows, 1);
    assert_eq!(health.wiki_non_source_rows, 1);
    assert_eq!(health.wiki_non_category_rows, 1);
    assert_eq!(health.derived_items, 0);
    assert_eq!(health.graph_edges, 1);
    assert_eq!(health.graph_orphan_edges, 1);
    assert_eq!(health.graph_relation_types[0].relation, "related_to");
    assert_eq!(health.graph_relation_types[0].count, 1);
}

#[test]
fn checkpoint_fixture_classifier_recognizes_only_real_fixtures() {
    let sep = std::path::MAIN_SEPARATOR;
    let cases = [
        (
            format!("{sep}Users{sep}u{sep}.tachi{sep}global{sep}memory.db"),
            false,
        ),
        (
            format!("{sep}home{sep}u{sep}.openclaw{sep}agents{sep}main{sep}memory.db"),
            false,
        ),
        (
            format!("{sep}srv{sep}checkpointed{sep}prod{sep}memory.db"),
            false,
        ),
        (
            format!("{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.sqlite"),
            false,
        ),
        (
            format!("{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.db"),
            true,
        ),
        (
            format!("{sep}tmp{sep}feature-daemon-project.db.checkpointed.20260430T015747Z.db"),
            true,
        ),
        (
            format!("{sep}tmp{sep}foo.db.CHECKPOINTED.20260430T015747Z.DB"),
            true,
        ),
    ];
    for (path, expected) in cases {
        assert_eq!(
            status_cli::is_checkpoint_fixture_path(&path),
            expected,
            "is_checkpoint_fixture_path({path:?}) misclassified"
        );
    }
}

fn db_status(label: &str, failed: usize, stuck: usize, coverage: f64) -> DbStatus {
    DbStatus {
        path: format!("/tmp/{label}.db"),
        label: label.to_string(),
        orphan: false,
        memory_total: 100,
        vector_count: (100.0 * coverage) as usize,
        vector_missing: 0,
        vector_orphans: 0,
        vector_coverage: coverage,
        vector_dimension: Some(EXPECTED_EMBEDDING_DIM),
        namespace: NamespaceHealth::default(),
        pending_enrichment: 0,
        enrichment_failed_recent: 0,
        enrichment_failures: Vec::new(),
        pending: 0,
        running: 0,
        active_jobs: 0,
        completed: 0,
        failed,
        dead_lettered: 0,
        skipped: 0,
        terminal_jobs: failed,
        gc_eligible: 0,
        stuck_in_progress: stuck,
        latest_active_job: None,
        latest_terminal_job: None,
        latest_job: None,
        latest_failed_job: None,
        error: None,
    }
}

fn empty_snapshot(dbs: Vec<DbStatus>) -> StatusSnapshot {
    StatusSnapshot {
        daemon: DaemonStatus::None,
        dbs,
        manifest_path: String::new(),
        dispatches: Vec::new(),
        recent_evals: Vec::new(),
        last_daily_report: None,
        distill_marker: None,
        api_keys: Vec::new(),
        provider_probe_cache: None,
        project_warnings: Vec::new(),
        plan_c_split_brain: Vec::new(),
        health_score: 95,
    }
}

fn daemon_running() -> serde_json::Value {
    serde_json::json!({ "running": true })
}

#[test]
fn build_status_warnings_names_foreign_daemon() {
    let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0)]);
    let daemon_state = serde_json::json!({
        "running": false,
        "foreign": true,
        "reason": "daemon global_db /tmp/openclaw.db does not match /tmp/tachi.db",
    });

    let warnings = build_status_warnings(&snapshot, &daemon_state);

    assert!(
        warnings
            .iter()
            .any(|w| w.contains("foreign daemon detected") && w.contains("openclaw.db")),
        "foreign daemon warning should name the mismatch, got: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .all(|w| !w.starts_with("daemon not running")),
        "foreign daemon should not be reported as a generic missing daemon: {warnings:?}"
    );
}

#[test]
fn normalize_dispatch_outcome_marks_old_working_rows_stale() {
    let now = DateTime::parse_from_rfc3339("2026-06-09T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let fresh = (now - chrono::Duration::minutes(30)).to_rfc3339();
    let stale = (now - chrono::Duration::hours(7)).to_rfc3339();

    assert_eq!(
        normalize_dispatch_outcome(Some("TASK_STATE_WORKING"), &fresh, now),
        "in_progress"
    );
    assert_eq!(
        normalize_dispatch_outcome(Some("TASK_STATE_WORKING"), &stale, now),
        "stale_working"
    );
    assert_eq!(
        normalize_dispatch_outcome(Some("TASK_STATE_COMPLETED"), &stale, now),
        "completed"
    );
}

#[test]
fn collect_dispatches_excludes_recall_cache_rows() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let db = dir.path().join("memory.db");
    let store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");
    let now = Utc::now().to_rfc3339();
    store
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, ?2, ?3, ?4, 0.8, ?5, 'kanban', 'kanban', '[]', '[]', ?6, 'general', 0, ?5, ?5, 0, 1, ?7)",
                rusqlite::params![
                    "real-dispatch",
                    "/kanban/tasks/20260609T000000Z-codex",
                    "Kanban: real task",
                    "Dispatch Task\nTask: real worker task",
                    now,
                    "manual",
                    json!({
                        "dispatch_id": "20260609T000000Z-codex",
                        "agent": "codex",
                        "task": "real worker task",
                        "a2a_state": "TASK_STATE_COMPLETED",
                        "reviewed": true,
                    })
                    .to_string(),
                ],
            )
            .expect("insert real dispatch");
    store
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, ?2, ?3, ?4, 0.8, ?5, 'kanban', 'kanban', '[]', '[]', ?6, 'general', 0, ?5, ?5, 0, 1, '{}')",
                rusqlite::params![
                    "foundry:recall-cache:noise",
                    "/kanban/tasks/recall-cache/review_tachi_mcp_facade",
                    "Recall rerank cache for query: review tachi mcp facade",
                    "Recall rerank cache for query: review tachi mcp facade",
                    Utc::now().to_rfc3339(),
                    FOUNDRY_RECALL_CACHE_SOURCE,
                ],
            )
            .expect("insert recall cache row");

    let dispatches = collect_dispatches(&db, None);
    assert_eq!(dispatches.len(), 1, "got: {dispatches:?}");
    assert_eq!(dispatches[0].dispatch_id, "20260609T000");
    assert_eq!(dispatches[0].agent, "codex");
    assert_eq!(dispatches[0].task, "real worker task");
}

#[test]
fn collect_dispatches_includes_project_db_rows() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let global_db = dir.path().join("global.db");
    let project_db = dir.path().join("project.db");
    MemoryStore::open(global_db.to_str().expect("global path")).expect("open global");
    let project =
        MemoryStore::open(project_db.to_str().expect("project path")).expect("open project");
    let now = Utc::now().to_rfc3339();
    project
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, ?2, ?3, ?4, 0.8, ?5, 'kanban', 'kanban', '[]', '[]', 'manual', 'project', 0, ?5, ?5, 0, 1, ?6)",
                rusqlite::params![
                    "project-dispatch",
                    "/kanban/tasks/20260609T000001Z-codex",
                    "Kanban: project task",
                    "Dispatch Task\nTask: project worker task",
                    now,
                    json!({
                        "dispatch_id": "20260609T000001Z-codex",
                        "agent": "codex",
                        "task": "project worker task",
                        "a2a_state": "TASK_STATE_COMPLETED",
                        "reviewed": true,
                    })
                    .to_string(),
                ],
            )
            .expect("insert project dispatch");

    let dispatches = collect_dispatches(&global_db, Some(&project_db));
    assert_eq!(dispatches.len(), 1, "got: {dispatches:?}");
    assert_eq!(dispatches[0].dispatch_id, "20260609T000");
    assert_eq!(dispatches[0].task, "project worker task");
}

#[test]
fn collect_recent_evals_reads_eval_category_rows() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let global_db = dir.path().join("global.db");
    let project_db = dir.path().join("project.db");
    MemoryStore::open(global_db.to_str().expect("global path")).expect("open global");
    let project =
        MemoryStore::open(project_db.to_str().expect("project path")).expect("open project");
    let now = Utc::now().to_rfc3339();
    project
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, ?2, ?3, ?4, 0.8, ?5, 'eval', 'eval', '[]', '[]', 'manual', 'project', 0, ?5, ?5, 0, 1, ?6)",
                rusqlite::params![
                    "eval-row",
                    "/eval/2026-06-09/20260609T000002Z-codex",
                    "[✓] codex / UX smoke",
                    "eval text",
                    now,
                    json!({
                        "agent": "codex",
                        "outcome": "success",
                        "quality_score": 0.82,
                    })
                    .to_string(),
                ],
            )
            .expect("insert eval");

    let evals = collect_recent_evals(&global_db, Some(&project_db));
    assert_eq!(evals.len(), 1, "got: {evals:?}");
    assert_eq!(evals[0].agent, "codex");
    assert_eq!(evals[0].outcome, "success");
    assert_eq!(evals[0].quality_score, Some(0.82));
}

#[test]
fn build_status_warnings_lists_db_names_for_low_coverage() {
    let snapshot = empty_snapshot(vec![
        db_status("global", 0, 0, 0.95),
        db_status("sigil", 0, 0, 0.42),
        db_status("hyperion", 0, 0, 0.81),
    ]);
    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let low_cov = warnings
        .iter()
        .find(|w| w.contains("vector coverage below 90%"))
        .expect("low-coverage warning present");
    assert!(
        low_cov.contains("sigil") && low_cov.contains("hyperion"),
        "low-coverage warning should name the affected dbs, got: {low_cov}"
    );
    assert!(
        !low_cov.contains("global"),
        "healthy dbs must not appear in low-coverage warning, got: {low_cov}"
    );
}

#[test]
fn build_status_warnings_includes_project_warnings() {
    let mut snapshot = empty_snapshot(vec![]);
    snapshot.project_warnings = vec![
        "/repo/.tachi/env.generated is tracked by git and may contain plaintext secrets"
            .to_string(),
    ];

    let warnings = build_status_warnings(&snapshot, &daemon_running());

    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains(".tachi/env.generated")),
        "project warning should be surfaced in status warnings, got: {warnings:?}"
    );
}

#[test]
fn build_status_warnings_lists_db_names_for_failed_jobs() {
    let mut sigil = db_status("sigil", 3, 0, 0.95);
    sigil.latest_failed_job = Some(LatestFailedJob {
        id: "job-1".to_string(),
        kind: "enrich".to_string(),
        lane: None,
        updated_at: None,
        reason: Some("401 invalid api key".to_string()),
        inferred_invalid_provider: Some("openai".to_string()),
    });
    let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 0.95), sigil]);
    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let failed = warnings
        .iter()
        .find(|w| w.contains("foundry job(s) failed"))
        .expect("failed-jobs warning present");
    assert!(
        failed.contains("sigil"),
        "failed-jobs warning should name the affected dbs, got: {failed}"
    );
    let auth = warnings
        .iter()
        .find(|w| w.contains("auth/API-key errors"))
        .expect("auth-failure warning present");
    assert!(
        auth.contains("sigil"),
        "auth-failure warning should name the affected dbs, got: {auth}"
    );
}

#[test]
fn build_status_warnings_lists_db_names_for_stuck_jobs() {
    let snapshot = empty_snapshot(vec![
        db_status("global", 0, 0, 0.95),
        db_status("sigil", 0, 2, 0.95),
    ]);
    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let stuck = warnings
        .iter()
        .find(|w| w.contains("stuck running"))
        .expect("stuck-jobs warning present");
    assert!(
        stuck.contains("sigil"),
        "stuck-jobs warning should name the affected dbs, got: {stuck}"
    );
}

#[test]
fn build_status_warnings_lists_enrichment_failures_and_vector_orphans() {
    let mut hyperion = db_status("hyperion", 0, 0, 1.0);
    hyperion.enrichment_failed_recent = 42;
    let mut sigil = db_status("sigil", 0, 0, 1.0);
    sigil.vector_orphans = 2;
    let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0), hyperion, sigil]);

    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let enrichment = warnings
        .iter()
        .find(|w| w.contains("memory enrichment failure"))
        .expect("enrichment-failure warning present");
    assert!(
        enrichment.contains("hyperion") && enrichment.contains("42"),
        "enrichment warning should name affected db and count, got: {enrichment}"
    );
    let orphans = warnings
        .iter()
        .find(|w| w.contains("orphan vector row"))
        .expect("vector-orphan warning present");
    assert!(
        orphans.contains("sigil") && orphans.contains("2"),
        "vector orphan warning should name affected db and count, got: {orphans}"
    );
}

#[test]
fn health_score_drops_for_background_enrichment_failures_and_orphans() {
    let mut hyperion = db_status("hyperion", 0, 0, 1.0);
    hyperion.enrichment_failed_recent = 42;
    let mut sigil = db_status("sigil", 0, 0, 1.0);
    sigil.vector_orphans = 1;
    let dbs = vec![hyperion, sigil];
    let score = status_health::calculate_health_score(
        &DaemonStatus::Running {
            pid: 1,
            lock_path: PathBuf::from("/tmp/tachi.lock"),
        },
        &dbs,
        Some(&DistillMarkerStatus {
            path: "/tmp/marker".to_string(),
            last_run_at: "2026-06-09T00:00:00Z".to_string(),
            age_seconds: 0,
            age: "0s ago".to_string(),
            is_stale: false,
            groups_distilled: Some(4),
            groups_skipped: Some(0),
            fallback_used: Some(0),
            errors: Some(0),
        }),
        &[],
        Some(&[]),
        Some(&[]),
    );

    assert!(
        score < 100,
        "background failures/orphans must prevent perfect health score"
    );
}
