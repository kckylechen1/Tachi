use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_surfaces_dispatch_run_ledger() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "99991231T235959Z-test-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create run ledger fixture");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "custom",
            "task": "smoke board run ledger",
            "state": "TASK_STATE_WORKING",
            "updated_at": "9999-12-31T23:59:59Z",
            "exit_code": null,
            "result_written": false,
            "harness_transport": "acpx",
            "execution_backend": "acpx",
            "acpx": {
                "agent": "codex",
                "mode": "session",
                "session": "raven",
                "permissions": "approve-reads"
            },
            "acpx_events": {
                "events_file": "/tmp/acpx_events.jsonl",
                "mapped_events": 2,
                "final_response_extracted": true
            },
        }))
        .expect("serialize status fixture"),
    )
    .expect("write status fixture");

    let board_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
            // tachi#1173 item 3: this test asserts the full acpx/acpx_events
            // payload is present on the row -- the new agent-facing default
            // (verbose omitted) strips those fields, so request the full
            // shape explicitly rather than weaken the assertion.
            verbose: Some(true),
        },
    )
    .await
    .expect("board should render");
    let board: serde_json::Value = serde_json::from_str(&board_raw).expect("board JSON");
    assert!(
        board["run_count"].as_u64().unwrap_or(0) >= 1,
        "board must include run-ledger rows even if kanban search misses: {board:#}"
    );
    assert!(
        board["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| { task["dispatch_id"].as_str() == Some(dispatch_id.as_str()) }),
        "board tasks should include dispatch {dispatch_id}: {board:#}"
    );
    assert!(
        board["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task.get("run_dir").and_then(|v| v.as_str()).is_some()
        }),
        "dispatch should carry run_dir from run ledger: {board:#}"
    );
    assert!(
        board["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task["execution_backend"].as_str() == Some("acpx")
                && task["acpx"]["session"].as_str() == Some("raven")
                && task["acpx_events"]["final_response_extracted"].as_bool() == Some(true)
        }),
        "dispatch should carry acpx metadata from run ledger: {board:#}"
    );
    let _ = std::fs::remove_dir_all(&run_dir);
}
