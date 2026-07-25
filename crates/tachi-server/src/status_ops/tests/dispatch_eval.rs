use super::*;

fn fixture_memory(
    id: &str,
    path: &str,
    summary: &str,
    text: &str,
    timestamp: &str,
    category: &str,
    topic: &str,
    source: &str,
    scope: &str,
    metadata: serde_json::Value,
) -> memcore::MemoryEntry {
    memcore::MemoryEntry {
        id: id.to_string(),
        path: path.to_string(),
        summary: summary.to_string(),
        text: text.to_string(),
        importance: 0.8,
        timestamp: timestamp.to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: category.to_string(),
        topic: topic.to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: source.to_string(),
        scope: scope.to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
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
    let mut store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");
    let now = Utc::now().to_rfc3339();
    store
        .upsert(&fixture_memory(
            "real-dispatch",
            "/kanban/tasks/20260609T000000Z-codex",
            "Kanban: real task",
            "Dispatch Task\nTask: real worker task",
            &now,
            "kanban",
            "kanban",
            "manual",
            "general",
            json!({
                "dispatch_id": "20260609T000000Z-codex",
                "agent": "codex",
                "task": "real worker task",
                "a2a_state": "TASK_STATE_COMPLETED",
                "reviewed": true,
            }),
        ))
        .expect("insert real dispatch");
    store
        .upsert(&fixture_memory(
            "foundry:recall-cache:noise",
            "/kanban/tasks/recall-cache/review_tachi_mcp_facade",
            "Recall rerank cache for query: review tachi mcp facade",
            "Recall rerank cache for query: review tachi mcp facade",
            &Utc::now().to_rfc3339(),
            "kanban",
            "kanban",
            FOUNDRY_RECALL_CACHE_SOURCE,
            "general",
            json!({}),
        ))
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
    let mut project =
        MemoryStore::open(project_db.to_str().expect("project path")).expect("open project");
    let now = Utc::now().to_rfc3339();
    project
        .upsert(&fixture_memory(
            "project-dispatch",
            "/kanban/tasks/20260609T000001Z-codex",
            "Kanban: project task",
            "Dispatch Task\nTask: project worker task",
            &now,
            "kanban",
            "kanban",
            "manual",
            "project",
            json!({
                "dispatch_id": "20260609T000001Z-codex",
                "agent": "codex",
                "task": "project worker task",
                "a2a_state": "TASK_STATE_COMPLETED",
                "reviewed": true,
            }),
        ))
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
    let mut project =
        MemoryStore::open(project_db.to_str().expect("project path")).expect("open project");
    let now = Utc::now().to_rfc3339();
    project
        .upsert(&fixture_memory(
            "eval-row",
            "/eval/2026-06-09/20260609T000002Z-codex",
            "[✓] codex / UX smoke",
            "eval text",
            &now,
            "eval",
            "eval",
            "manual",
            "project",
            json!({
                "agent": "codex",
                "outcome": "success",
                "quality_score": 0.82,
            }),
        ))
        .expect("insert eval");

    let evals = collect_recent_evals(&global_db, Some(&project_db));
    assert_eq!(evals.len(), 1, "got: {evals:?}");
    assert_eq!(evals[0].agent, "codex");
    assert_eq!(evals[0].outcome, "success");
    assert_eq!(evals[0].quality_score, Some(0.82));
}
