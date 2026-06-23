use super::*;

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
