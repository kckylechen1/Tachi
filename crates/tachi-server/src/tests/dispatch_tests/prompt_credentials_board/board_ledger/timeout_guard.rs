use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_caps_corrupt_huge_timeout_before_duration_math() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260608T000001Z-huge-timeout-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create huge-timeout run fixture");
    let stale_updated_at = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "corrupt huge timeout must not panic board",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "result_written": false,
            "timeout_secs": i64::MAX,
        }))
        .expect("serialize huge-timeout status fixture"),
    )
    .expect("write huge-timeout status fixture");

    let failed_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("failed".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
        },
    )
    .await
    .expect("board should not panic on corrupt huge timeout");
    let failed: serde_json::Value = serde_json::from_str(&failed_raw).expect("failed board JSON");
    assert!(
        failed["tasks"].as_array().unwrap().iter().any(|task| {
            task["dispatch_id"].as_str() == Some(dispatch_id.as_str())
                && task["state"].as_str() == Some("TASK_STATE_FAILED")
                && task["stale"].as_bool() == Some(true)
        }),
        "capped huge timeout should still allow stale classification: {failed:#}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}
