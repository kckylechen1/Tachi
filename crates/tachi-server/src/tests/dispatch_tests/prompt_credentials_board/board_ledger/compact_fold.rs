use super::*;

/// tachi#1173 item 3 discriminator: the default board view (no `verbose`,
/// `state_filter` omitted/"all") must not carry the heavy per-row session
/// payload, and must fold terminal (e.g. completed) rows into a count row
/// instead of listing them individually. `verbose=true` restores both.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn default_board_hides_heavy_fields_and_folds_terminal_rows() {
    let (server, temp_home) = make_server_with_temp_home();
    let runs_dir = temp_home.temp_home.join(".tachi/runs");

    let active_id = format!(
        "99991231T235958Z-compact-active-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let active_dir = runs_dir.join(&active_id);
    std::fs::create_dir_all(&active_dir).expect("create active run fixture");
    std::fs::write(
        active_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": active_id,
            "agent": "codex",
            "task": "still working",
            "state": "TASK_STATE_WORKING",
            "updated_at": "9999-12-31T23:59:58Z",
            "exit_code": null,
            "harness_transport": "acpx",
            "execution_backend": "acpx",
            "acpx": {"agent": "codex", "mode": "session"},
            "acpx_events": {"mapped_events": 1},
        }))
        .expect("serialize active fixture"),
    )
    .expect("write active fixture");

    let completed_id = format!(
        "20260101T000000Z-compact-completed-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let completed_dir = runs_dir.join(&completed_id);
    std::fs::create_dir_all(&completed_dir).expect("create completed run fixture");
    std::fs::write(
        completed_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": completed_id,
            "agent": "codex",
            "task": "done long ago",
            "state": "TASK_STATE_COMPLETED",
            "updated_at": "2026-01-01T00:00:00Z",
            "exit_code": 0,
        }))
        .expect("serialize completed fixture"),
    )
    .expect("write completed fixture");

    let default_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: None,
            limit: Some(20),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("default board should render");
    let default_board: serde_json::Value =
        serde_json::from_str(&default_raw).expect("default board JSON");
    let tasks = default_board["tasks"].as_array().expect("tasks array");

    assert!(
        tasks
            .iter()
            .any(|task| task["dispatch_id"].as_str() == Some(active_id.as_str())),
        "active row should still be listed individually: {default_board:#}"
    );
    assert!(
        !tasks
            .iter()
            .any(|task| task["dispatch_id"].as_str() == Some(completed_id.as_str())),
        "completed (terminal) row should be folded away by default: {default_board:#}"
    );
    assert!(
        tasks.iter().all(|task| task.get("identity_receipt").is_none()
            && task.get("acpx").is_none()
            && task.get("acpx_events").is_none()),
        "default board rows must not carry identity_receipt/acpx/acpx_events: {default_board:#}"
    );
    assert!(
        tasks.iter().any(|task| task.get("folded").and_then(Value::as_bool) == Some(true)
            && task["state"].as_str() == Some("TASK_STATE_COMPLETED")
            && task["count"].as_u64().unwrap_or(0) >= 1),
        "terminal rows should be folded into a count row: {default_board:#}"
    );
    assert!(
        default_board["folded_total"].as_u64().unwrap_or(0) >= 1,
        "folded_total should report at least the one folded completed row: {default_board:#}"
    );

    let verbose_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: None,
            limit: Some(20),
            project: None,
            flow_id: None,
            verbose: Some(true),
        },
    )
    .await
    .expect("verbose board should render");
    let verbose_board: serde_json::Value =
        serde_json::from_str(&verbose_raw).expect("verbose board JSON");
    let verbose_tasks = verbose_board["tasks"].as_array().expect("tasks array");

    assert!(
        verbose_tasks
            .iter()
            .any(|task| task["dispatch_id"].as_str() == Some(completed_id.as_str())),
        "verbose=true should restore the folded completed row individually: {verbose_board:#}"
    );
    assert!(
        verbose_tasks.iter().any(|task| {
            task["dispatch_id"].as_str() == Some(active_id.as_str())
                && task.get("acpx_events").is_some()
        }),
        "verbose=true should restore acpx_events on the active row: {verbose_board:#}"
    );

    let _ = std::fs::remove_dir_all(&active_dir);
    let _ = std::fs::remove_dir_all(&completed_dir);
}

/// tachi#1173 item 7 discriminator: an individually-visible (non-folded)
/// failed board row must carry an ANSI-free `failure_tail`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn failed_board_row_includes_ansi_free_failure_tail() {
    let (server, temp_home) = make_server_with_temp_home();
    let runs_dir = temp_home.temp_home.join(".tachi/runs");
    let dispatch_id = format!(
        "20260102T000000Z-compact-failed-tail-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = runs_dir.join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create failed run fixture");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "board autopsy failure",
            "state": "TASK_STATE_FAILED",
            "updated_at": "2026-01-02T00:00:00Z",
            "exit_code": 1,
        }))
        .expect("serialize failed fixture"),
    )
    .expect("write failed fixture");
    std::fs::write(
        run_dir.join("progress.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "event": "subprocess_finished",
                "output_tail": "\u{1b}[31mcompile error\u{1b}[0m",
            })
        ),
    )
    .expect("write progress.jsonl fixture");

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
    let failed_board: serde_json::Value =
        serde_json::from_str(&failed_raw).expect("failed board JSON");
    let task = failed_board["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .find(|task| task["dispatch_id"].as_str() == Some(dispatch_id.as_str()))
        .unwrap_or_else(|| {
            panic!("failed board should include dispatch {dispatch_id}: {failed_board:#}")
        });
    let tail = task["failure_tail"]
        .as_str()
        .unwrap_or_else(|| panic!("failure_tail should be present: {failed_board:#}"));
    assert!(
        tail.contains("compile error"),
        "failure_tail should carry the underlying message, got: {tail:?}"
    );
    assert!(
        !tail.contains('\u{1b}'),
        "failure_tail must not contain raw ANSI escape bytes: {tail:?}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}
