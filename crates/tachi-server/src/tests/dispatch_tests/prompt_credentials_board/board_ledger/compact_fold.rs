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
        tasks
            .iter()
            .all(|task| task.get("identity_receipt").is_none()
                && task.get("acpx").is_none()
                && task.get("acpx_events").is_none()),
        "default board rows must not carry identity_receipt/acpx/acpx_events: {default_board:#}"
    );
    assert!(
        tasks.iter().any(
            |task| task.get("folded").and_then(Value::as_bool) == Some(true)
                && task["state"].as_str() == Some("TASK_STATE_COMPLETED")
                && task["count"].as_u64().unwrap_or(0) >= 1
        ),
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

/// tachi#1173 board autopsy review discriminator: an explicitly-passed
/// `state_filter: "all"` must NOT be folded -- only the truly-default
/// (omitted) view folds terminal rows. Before this fix both collapsed to the
/// same "all" string and were indistinguishable, so an explicit "all" was
/// silently folded exactly like the default.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn explicit_all_state_filter_does_not_fold_terminal_rows() {
    let (server, temp_home) = make_server_with_temp_home();
    let runs_dir = temp_home.temp_home.join(".tachi/runs");

    let completed_id = format!(
        "20260103T000000Z-compact-explicit-all-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let completed_dir = runs_dir.join(&completed_id);
    std::fs::create_dir_all(&completed_dir).expect("create completed run fixture");
    std::fs::write(
        completed_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": completed_id,
            "agent": "codex",
            "task": "done, explicit all filter should still show this",
            "state": "TASK_STATE_COMPLETED",
            "updated_at": "2026-01-03T00:00:00Z",
            "exit_code": 0,
        }))
        .expect("serialize completed fixture"),
    )
    .expect("write completed fixture");

    let explicit_all_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(20),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("explicit-all board should render");
    let explicit_all_board: serde_json::Value =
        serde_json::from_str(&explicit_all_raw).expect("explicit-all board JSON");
    let tasks = explicit_all_board["tasks"].as_array().expect("tasks array");

    assert!(
        tasks
            .iter()
            .any(|task| task["dispatch_id"].as_str() == Some(completed_id.as_str())),
        "explicit state_filter=\"all\" must list the completed row individually, \
         not fold it away: {explicit_all_board:#}"
    );
    assert!(
        !tasks
            .iter()
            .any(|task| task.get("folded").and_then(Value::as_bool) == Some(true)),
        "explicit state_filter=\"all\" must not produce any folded summary row: \
         {explicit_all_board:#}"
    );

    let _ = std::fs::remove_dir_all(&completed_dir);
}

/// tachi#1173 board autopsy review discriminator: `failure_tail` must be a
/// stable key on every visible individual row -- present (and `null`) even
/// when the dispatch never failed, or failed but has no readable tail --
/// matching the shape `tachi_task(action='wait')` already commits to (see
/// `task_facade::wait_terminal_completion_has_null_failure_tail`). Before
/// this fix the key was only inserted when a tail was actually found on a
/// failed row, so "not failed" and "failed but no tail" were both simply
/// *absent* the key -- indistinguishable by key presence.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn board_row_failure_tail_is_a_stable_null_key_when_absent() {
    let (server, temp_home) = make_server_with_temp_home();
    let runs_dir = temp_home.temp_home.join(".tachi/runs");

    let active_id = format!(
        "99991231T235957Z-compact-tail-active-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let active_dir = runs_dir.join(&active_id);
    std::fs::create_dir_all(&active_dir).expect("create active run fixture");
    std::fs::write(
        active_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": active_id,
            "agent": "codex",
            "task": "still working, never failed",
            "state": "TASK_STATE_WORKING",
            "updated_at": "9999-12-31T23:59:57Z",
            "exit_code": null,
        }))
        .expect("serialize active fixture"),
    )
    .expect("write active fixture");

    let failed_no_tail_id = format!(
        "20260104T000000Z-compact-failed-no-tail-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let failed_no_tail_dir = runs_dir.join(&failed_no_tail_id);
    std::fs::create_dir_all(&failed_no_tail_dir).expect("create failed-no-tail run fixture");
    std::fs::write(
        failed_no_tail_dir.join("status.json"),
        serde_json::to_string(&serde_json::json!({
            "dispatch_id": failed_no_tail_id,
            "agent": "codex",
            "task": "failed with no progress.jsonl/result.md tail source",
            "state": "TASK_STATE_FAILED",
            "updated_at": "2026-01-04T00:00:00Z",
            "exit_code": 1,
        }))
        .expect("serialize failed-no-tail fixture"),
    )
    .expect("write failed-no-tail fixture");

    // verbose=true (rather than an explicit state_filter) keeps both rows
    // unfolded here regardless of the explicit-"all" fold-bypass fix above,
    // isolating this test to the failure_tail shape question alone.
    let board_raw = crate::dispatch_ops::handle_tachi_board(
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
    .expect("board should render");
    let board: serde_json::Value = serde_json::from_str(&board_raw).expect("board JSON");
    let tasks = board["tasks"].as_array().expect("tasks array");

    let active_task = tasks
        .iter()
        .find(|task| task["dispatch_id"].as_str() == Some(active_id.as_str()))
        .unwrap_or_else(|| panic!("active row should be present: {board:#}"));
    assert!(
        active_task.get("failure_tail").is_some_and(Value::is_null),
        "a non-failed row must carry a stable, present, null failure_tail key: {active_task:#}"
    );

    let failed_task = tasks
        .iter()
        .find(|task| task["dispatch_id"].as_str() == Some(failed_no_tail_id.as_str()))
        .unwrap_or_else(|| panic!("failed-no-tail row should be present: {board:#}"));
    assert!(
        failed_task.get("failure_tail").is_some_and(Value::is_null),
        "a failed row with no readable tail must still carry the key (null), \
         not omit it entirely: {failed_task:#}"
    );

    let _ = std::fs::remove_dir_all(&active_dir);
    let _ = std::fs::remove_dir_all(&failed_no_tail_dir);
}

/// tachi#1173 board autopsy review discriminator (folded_counts vs limit
/// CONCERN): per-state folded summary rows must not push the total `tasks`
/// array past the caller's requested `limit`, and the top-level `count`
/// field must match what's actually returned. Before this fix individual
/// rows were truncated to `limit` and folded summary rows were appended
/// afterward, unbounded -- `count` could silently exceed the requested
/// `limit`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn folded_summary_rows_count_toward_limit() {
    let (server, temp_home) = make_server_with_temp_home();
    let runs_dir = temp_home.temp_home.join(".tachi/runs");
    let mut cleanup = Vec::new();

    // 4 active (non-terminal) rows -- more than fit once 2 folded summary
    // rows (completed + failed) are reserved out of a limit of 5.
    for i in 0..4 {
        let id = format!(
            "99991231T23595{i}Z-compact-limit-active-{}",
            uuid::Uuid::new_v4().as_simple()
        );
        let dir = runs_dir.join(&id);
        std::fs::create_dir_all(&dir).expect("create active fixture");
        std::fs::write(
            dir.join("status.json"),
            serde_json::to_string(&serde_json::json!({
                "dispatch_id": id,
                "agent": "codex",
                "task": format!("active {i}"),
                "state": "TASK_STATE_WORKING",
                "updated_at": format!("9999-12-31T23:59:5{i}Z"),
                "exit_code": null,
            }))
            .expect("serialize active fixture"),
        )
        .expect("write active fixture");
        cleanup.push(dir);
    }

    // 1 completed + 1 failed -> 2 distinct folded states.
    for (idx, (state, tag)) in [
        ("TASK_STATE_COMPLETED", "completed"),
        ("TASK_STATE_FAILED", "failed"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!(
            "2026010{}T000000Z-compact-limit-{tag}-{}",
            5 + idx,
            uuid::Uuid::new_v4().as_simple()
        );
        let dir = runs_dir.join(&id);
        std::fs::create_dir_all(&dir).expect("create terminal fixture");
        std::fs::write(
            dir.join("status.json"),
            serde_json::to_string(&serde_json::json!({
                "dispatch_id": id,
                "agent": "codex",
                "task": format!("terminal {tag}"),
                "state": state,
                "updated_at": format!("2026-01-0{}T00:00:00Z", 5 + idx),
                "exit_code": if tag == "completed" { 0 } else { 1 },
            }))
            .expect("serialize terminal fixture"),
        )
        .expect("write terminal fixture");
        cleanup.push(dir);
    }

    let board_raw = crate::dispatch_ops::handle_tachi_board(
        &server,
        TachiBoardParams {
            state_filter: None,
            limit: Some(5),
            project: None,
            flow_id: None,
            verbose: None,
        },
    )
    .await
    .expect("board should render");
    let board: serde_json::Value = serde_json::from_str(&board_raw).expect("board JSON");
    let tasks = board["tasks"].as_array().expect("tasks array");

    assert!(
        tasks.len() <= 5,
        "tasks.len() ({}) must not exceed the requested limit (5), folded summary \
         rows included: {board:#}",
        tasks.len()
    );
    assert_eq!(
        board["count"].as_u64(),
        Some(tasks.len() as u64),
        "top-level count must match the actual number of rows returned: {board:#}"
    );
    assert!(
        tasks
            .iter()
            .any(|task| task.get("folded").and_then(Value::as_bool) == Some(true)),
        "at least one folded summary row should still be present: {board:#}"
    );

    for dir in cleanup {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
