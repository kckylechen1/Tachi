use super::*;

#[tokio::test]
async fn board_marks_abandoned_kanban_card_as_failed_without_live_run() {
    let server = make_server();
    let dispatch_id = format!(
        "20260626T000002Z-stale-kanban-card-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let stale_updated_at = (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    server
        .with_global_store(|store| {
            let mut entry = make_entry("stale-kanban-card");
            entry.path = format!("/kanban/tasks/{dispatch_id}");
            entry.summary = "Arena persisted stale kanban noise".to_string();
            entry.text =
                "Dispatch Task\nAgent: arena\nTask: Arena persisted stale kanban noise".to_string();
            entry.timestamp = stale_updated_at.clone();
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

    let active_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("active".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("active board should render");
    let active: serde_json::Value = serde_json::from_str(&active_raw).expect("active board JSON");
    assert!(
        !active["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["dispatch_id"].as_str() == Some(dispatch_id.as_str())),
        "stale kanban-only card should not appear on active board: {active:#}"
    );

    let failed_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("failed".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("failed board should render");
    let failed: serde_json::Value = serde_json::from_str(&failed_raw).expect("failed board JSON");
    assert!(
        failed["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task["state"].as_str() == Some("TASK_STATE_FAILED")
                && task["stale"].as_bool() == Some(true)
                && task["state_source"].as_str() == Some("kanban_stale_timeout")
        }),
        "failed board should surface stale kanban-only card for diagnostics: {failed:#}"
    );
}
