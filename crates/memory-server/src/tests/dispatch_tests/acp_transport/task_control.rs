use super::*;

#[tokio::test]
async fn tachi_task_facade_defaults_to_json_and_keeps_markdown_escape_hatch() {
    let server = make_server();

    let mut json_params = task_params("profiles");
    json_params.format = None;
    let json_body = server
        .tachi_task(Parameters(json_params))
        .await
        .expect("default profiles should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("default profiles JSON");
    assert!(parsed["dispatch_profiles"].as_array().is_some());

    let mut markdown_params = task_params("profiles");
    markdown_params.format = Some("markdown".to_string());
    let markdown = server
        .tachi_task(Parameters(markdown_params))
        .await
        .expect("markdown profiles should succeed");
    assert!(markdown.starts_with("## Tachi task profiles"), "{markdown}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_wait_returns_terminal_dispatch_status() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let dispatch_id = "dispatch-wait-complete";
    let run_dir = temp_home.path().join("runs").join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "wait for completed dispatch",
            "state": "TASK_STATE_COMPLETED",
            "exit_code": 0,
            "updated_at": Utc::now().to_rfc3339(),
        }))
        .expect("status json"),
    )
    .expect("write status");
    std::fs::write(run_dir.join("result.md"), "done").expect("result");

    let mut params = task_params("wait");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(0);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("wait should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("wait JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["terminal"], json!(true));
    assert_eq!(parsed["state"], json!("TASK_STATE_COMPLETED"));
    assert_eq!(parsed["task"]["result_written"], json!(true));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_status_runs_acpx_status_control() {
    let (server, temp_home) = make_server_with_temp_home();
    let temp_python = tempfile::tempdir().expect("temp python module");
    write_fake_acpx_control_module(temp_python.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let dispatch_id = "99991231T235957Z-acpx-status-control";
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(dispatch_id);
    write_acpx_control_fixture(&run_dir, dispatch_id);

    let mut params = task_params("status");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(5);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("status should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("status JSON");
    assert_eq!(parsed["status"], json!("ok"));
    assert_eq!(
        parsed["acpx_status"]["stdout_json"]["action"],
        json!("status")
    );
    assert_eq!(
        parsed["acpx_status"]["stdout_json"]["state"],
        json!("running")
    );
    assert!(run_dir.join("acpx_status.json").exists());
    let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory");
    assert!(trajectory.contains("acpx_control_invoked"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_cancel_invokes_acpx_cancel_and_records_request() {
    let (server, temp_home) = make_server_with_temp_home();
    let temp_python = tempfile::tempdir().expect("temp python module");
    write_fake_acpx_control_module(temp_python.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let dispatch_id = "99991231T235956Z-acpx-cancel-control";
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(dispatch_id);
    write_acpx_control_fixture(&run_dir, dispatch_id);

    let mut params = task_params("cancel");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(5);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("cancel should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("cancel JSON");
    assert_eq!(parsed["status"], json!("cancel_requested"));
    assert_eq!(
        parsed["acpx_cancel"]["stdout_json"]["action"],
        json!("cancel")
    );
    assert!(run_dir.join("acpx_cancel.json").exists());

    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("updated status JSON");
    assert_eq!(status["cancel_requested"], json!(true));
    assert_eq!(
        status["acpx_cancel"]["stdout_json"]["state"],
        json!("cancelled")
    );
}
