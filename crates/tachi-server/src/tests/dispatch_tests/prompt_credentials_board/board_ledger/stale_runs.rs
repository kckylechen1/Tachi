use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_marks_abandoned_working_run_as_failed() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260608T000000Z-stale-run-ledger-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create stale run fixture");
    let stale_updated_at = (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "stale run should not clog working board",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "result_written": false,
            "timeout_secs": 5,
        }))
        .expect("serialize stale status fixture"),
    )
    .expect("write stale status fixture");

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
                && task["stale_reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("WORKING"))
        }),
        "failed board should include stale derived run: {failed:#}"
    );

    let working_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("working".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("working board should render");
    let working: serde_json::Value =
        serde_json::from_str(&working_raw).expect("working board JSON");
    assert!(
        !working["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["dispatch_id"].as_str() == Some(dispatch_id.as_str())),
        "stale run should not remain on working board: {working:#}"
    );

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
        "stale failed run should not appear on active board: {active:#}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_marks_working_run_with_result_as_stale_after_timeout() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260626T000004Z-result-still-working-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create result-stale run fixture");
    let stale_updated_at = (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "arena",
            "task": "result exists but status stayed working",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "timeout_secs": 5,
        }))
        .expect("serialize stale status fixture"),
    )
    .expect("write stale status fixture");
    std::fs::write(run_dir.join("result.md"), "done").expect("write result fixture");

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
        "run with result.md but stale WORKING status should not appear on active board: {active:#}"
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
                && task["result_written"].as_bool() == Some(true)
        }),
        "failed board should show stale result-written run for diagnostics: {failed:#}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}
