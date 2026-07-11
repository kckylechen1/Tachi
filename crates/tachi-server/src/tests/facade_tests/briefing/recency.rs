use super::*;

/// tachi#925 defect 1: `recent_checkpoints` must be recency-first (newest
/// `updated_at` wins), not path-lexical-first. Regression receipt: a
/// checkpoint saved at `/agent/checkpoints/2026-07-09` ~45 minutes earlier
/// did not appear in the briefing at all — two `/agent/checkpoints/2026-05-30`
/// entries were returned instead, because the underlying store query ordered
/// `path ASC, timestamp DESC` and the `LIMIT` truncated before the
/// lexicographically-later (but chronologically newer) path was ever reached.
#[tokio::test]
async fn tachi_memory_briefing_recent_checkpoints_orders_newest_first_global() {
    // Briefing may resolve a named project from the active workspace for
    // read-only calls. Keep this global-store regression fixture under a
    // temporary Tachi home so a developer's real project database cannot
    // replace the checkpoints seeded below.
    let (server, _temp_home) = make_server_with_temp_home();

    server
        .with_global_store(|store| {
            let mut old_a = make_entry("global-checkpoint-old-a");
            old_a.path = "/agent/checkpoints/2026-05-30".to_string();
            old_a.summary = "Stale checkpoint A".to_string();
            old_a.text = "Old checkpoint content A".to_string();
            old_a.timestamp = "2026-05-30T10:00:00Z".to_string();
            store.upsert(&old_a).map_err(|e| e.to_string())?;

            let mut old_b = make_entry("global-checkpoint-old-b");
            old_b.path = "/agent/checkpoints/2026-05-30".to_string();
            old_b.summary = "Stale checkpoint B".to_string();
            old_b.text = "Old checkpoint content B".to_string();
            old_b.timestamp = "2026-05-30T11:00:00Z".to_string();
            store.upsert(&old_b).map_err(|e| e.to_string())?;

            let mut newest = make_entry("global-checkpoint-newest");
            newest.path = "/agent/checkpoints/2026-07-09".to_string();
            newest.summary = "Fresh checkpoint saved 45 minutes ago".to_string();
            newest.text = "Newest checkpoint content".to_string();
            newest.timestamp = "2026-07-09T15:53:00Z".to_string();
            store.upsert(&newest).map_err(|e| e.to_string())
        })
        .expect("seed global checkpoints");

    // compact briefing caps recent_checkpoints at 2 rows — with only 2 slots
    // and 3 candidate checkpoints, the old path-first bug would drop the
    // newest checkpoint entirely (both /2026-05-30 rows would win the LIMIT).
    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("checkpoint".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    let checkpoints = parsed["recent_checkpoints"]
        .as_array()
        .cloned()
        .expect("recent_checkpoints array");
    let checkpoint_ids = checkpoints
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();

    assert!(
        checkpoint_ids.contains(&"global-checkpoint-newest"),
        "expected the newest checkpoint to survive the recency cutoff: {parsed}"
    );
    assert_eq!(
        checkpoint_ids.first().copied(),
        Some("global-checkpoint-newest"),
        "expected the newest checkpoint first (recency-ordered), got: {parsed}"
    );
}

/// Same regression, but through the project-scoped checkpoint listing path
/// (`list_recent_checkpoint_entries_for_project`), which shares the same
/// underlying store query as the global path.
#[tokio::test]
async fn tachi_memory_briefing_recent_checkpoints_orders_newest_first_project() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home
        .temp_home
        .join("Recency Project Repo")
        .canonicalize()
        .unwrap_or_else(|_| temp_home.temp_home.join("Recency Project Repo"));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("project DB init should succeed");

    server
        .with_project_store(|store| {
            let mut old_a = make_entry("project-checkpoint-old-a");
            old_a.path = "/agent/checkpoints/2026-05-30".to_string();
            old_a.summary = "Stale project checkpoint A".to_string();
            old_a.text = "Old project checkpoint content A".to_string();
            old_a.timestamp = "2026-05-30T10:00:00Z".to_string();
            store.upsert(&old_a).map_err(|e| e.to_string())?;

            let mut old_b = make_entry("project-checkpoint-old-b");
            old_b.path = "/agent/checkpoints/2026-05-30".to_string();
            old_b.summary = "Stale project checkpoint B".to_string();
            old_b.text = "Old project checkpoint content B".to_string();
            old_b.timestamp = "2026-05-30T11:00:00Z".to_string();
            store.upsert(&old_b).map_err(|e| e.to_string())?;

            let mut newest = make_entry("project-checkpoint-newest");
            newest.path = "/agent/checkpoints/2026-07-09".to_string();
            newest.summary = "Fresh project checkpoint saved 45 minutes ago".to_string();
            newest.text = "Newest project checkpoint content".to_string();
            newest.timestamp = "2026-07-09T15:53:00Z".to_string();
            store.upsert(&newest).map_err(|e| e.to_string())
        })
        .expect("seed project checkpoints");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("checkpoint".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    let checkpoints = parsed["recent_checkpoints"]
        .as_array()
        .cloned()
        .expect("recent_checkpoints array");
    let checkpoint_ids = checkpoints
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();

    assert!(
        checkpoint_ids.contains(&"project-checkpoint-newest"),
        "expected the newest project checkpoint to survive the recency cutoff: {parsed}"
    );
    assert_eq!(
        checkpoint_ids.first().copied(),
        Some("project-checkpoint-newest"),
        "expected the newest project checkpoint first (recency-ordered), got: {parsed}"
    );
}
