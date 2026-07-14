use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_dispatch_with_flow_id_records_dispatch_card() {
    let (server, _temp_home) = make_server_with_temp_home();
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let flow_id = "flow_20260608T000008Z_dispatch_card_test";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id,
            "dispatch_ids": [],
        }))
        .expect("serialize status"),
    )
    .expect("seed status");

    let mut params = dispatch_params(Some("custom"), "smoke flow dispatch marker");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('ok')".to_string(),
    ];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    params.profile = Some("glm_impl".to_string());
    params.flow_id = Some(flow_id.to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert!(
        status["dispatch_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(dispatch_id))),
        "flow status should include dispatch id {dispatch_id}: {status:#}"
    );
    assert!(
        status["artifacts"]["dispatches"][dispatch_id]
            .as_str()
            .is_some_and(|path| path.ends_with(".json")),
        "flow status should link compact dispatch card: {status:#}"
    );
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["suggested_complete"]["tool"], json!("tachi_task"));
    assert_eq!(
        card["suggested_complete"]["arguments"]["action"],
        json!("complete")
    );
    assert_eq!(
        card["suggested_complete"]["arguments"]["dispatch_id"],
        json!(dispatch_id)
    );
    assert_eq!(
        card["suggested_complete"]["arguments"]["flow_id"],
        json!(flow_id)
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(
        events.contains(dispatch_id) && events.contains("\"event\":\"dispatch_linked\""),
        "{events}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_board_filters_to_flow_dispatch_ids() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // #1096: the server freezes its home identity at construction, so the
    // TACHI_HOME override must be in place BEFORE make_server() runs.
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let flow_id = "flow_20260609T000005Z_board_flow_filter";
    let dispatch_id = "20260609T000005Z-codex-flow";
    let other_dispatch_id = "20260609T000006Z-codex-other";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "codex",
            "profile": "codex_53_fast",
            "task": "flow task",
        }),
    )
    .expect("mark dispatch");
    for (id, task) in [
        (dispatch_id, "flow task"),
        (other_dispatch_id, "other task"),
    ] {
        let run_dir = temp_home.path().join("runs").join(id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        if id == dispatch_id {
            std::fs::write(run_dir.join("result.md"), "worker completed").expect("result");
        }
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::to_string_pretty(&json!({
                "dispatch_id": id,
                "agent": "codex",
                "task": task,
                "state": "TASK_STATE_COMPLETED",
                "exit_code": 0,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .expect("status json"),
        )
        .expect("write status");
    }

    let mut params = task_params("board");
    params.flow_id = Some(flow_id.to_string());
    params.limit = Some(20);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("board should succeed");
    let board: Value = serde_json::from_str(&raw).expect("board JSON");
    assert_eq!(board["flow_id"], json!(flow_id), "{board:#}");
    assert_eq!(board["run_count"], json!(1), "{board:#}");
    let tasks = board["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 1, "{board:#}");
    assert_eq!(tasks[0]["dispatch_id"], json!(dispatch_id), "{board:#}");
    assert_eq!(
        tasks[0]["state"],
        json!("TASK_STATE_COMPLETED"),
        "{board:#}"
    );
    assert_eq!(tasks[0]["exit_code"], json!(0), "{board:#}");
    assert_eq!(tasks[0]["result_written"], json!(true), "{board:#}");
    assert_eq!(tasks[0]["state_source"], json!("run"), "{board:#}");
    assert!(
        tasks[0]["run_dir"]
            .as_str()
            .is_some_and(|path| path.ends_with(dispatch_id)),
        "{board:#}"
    );
}
