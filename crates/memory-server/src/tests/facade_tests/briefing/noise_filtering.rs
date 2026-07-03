use super::*;

#[tokio::test]
async fn tachi_memory_briefing_excludes_stale_run_board_noise() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260626T000000Z-arena-stale-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create stale run fixture");
    let stale_updated_at = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&json!({
            "dispatch_id": dispatch_id,
            "agent": "arena",
            "task": "Arena mission stale briefing noise",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "result_written": false,
            "timeout_secs": 5,
        }))
        .expect("serialize stale status fixture"),
    )
    .expect("write status fixture");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("current task".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");
    let tasks = parsed["kanban"]["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    assert!(
        !tasks
            .iter()
            .any(|task| task["summary"] == json!("Arena mission stale briefing noise")),
        "briefing should not surface stale run board noise: {parsed}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}

#[tokio::test]
async fn tachi_memory_briefing_excludes_stale_kanban_card_noise() {
    let server = make_server();
    let dispatch_id = format!(
        "20260626T000003Z-arena-stale-kanban-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let stale_updated_at = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    server
        .with_global_store(|store| {
            let mut entry = make_entry("briefing-stale-kanban-card");
            entry.path = format!("/kanban/tasks/{dispatch_id}");
            entry.summary = "Arena persisted kanban briefing noise".to_string();
            entry.text = "Dispatch Task\nAgent: arena\nTask: Arena persisted kanban briefing noise"
                .to_string();
            entry.timestamp = stale_updated_at;
            entry.topic = "kanban".to_string();
            entry.keywords = vec!["kanban".to_string(), "dispatch".to_string()];
            entry.metadata = json!({
                "type": "a2a_task",
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
                "agent": "arena",
                "timeout_secs": 5,
            });
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed stale kanban card");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("current task".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");
    let tasks = parsed["kanban"]["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    assert!(
        !tasks
            .iter()
            .any(|task| task["summary"] == json!("Arena persisted kanban briefing noise")),
        "briefing should not surface stale persisted kanban noise: {parsed}"
    );
}
