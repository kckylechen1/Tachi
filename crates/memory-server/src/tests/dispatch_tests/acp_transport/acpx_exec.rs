use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_acpx_transport_persists_events_and_final_result() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_python = tempfile::tempdir().expect("temp python module");
    let fake_acpx = temp_python.path().join("fake_acpx.py");
    std::fs::write(
        &fake_acpx,
        r#"import json
print(json.dumps({"type": "message", "message": "working"}))
print(json.dumps({"event": "tool_call_start", "tool_name": "noop"}))
print(json.dumps({"event": "end_turn", "final_response": "acpx done"}))
"#,
    )
    .expect("fake acpx module");

    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let _acpx_command = EnvVarGuard::set_value("TACHI_ACPX_COMMAND", "python3");
    let _acpx_args = EnvVarGuard::set_value("TACHI_ACPX_ARGS", "-m fake_acpx");
    let _acpx_agent = EnvVarGuard::set_value("TACHI_ACPX_AGENT", "codex");
    let _acpx_mode = EnvVarGuard::set_value("TACHI_ACPX_RUN_MODE", "exec");
    let _acpx_session_mode = EnvVarGuard::set_value("TACHI_ACPX_SESSION_MODE", "");
    let _acpx_session = EnvVarGuard::set_value("TACHI_ACPX_SESSION", "");

    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "Run through fake acpx");
    params.harness_transport = Some("acpx".to_string());
    params.cwd = Some(temp_home.path().to_string_lossy().to_string());
    params.timeout_secs = 5;

    let response = server
        .tachi_dispatch(Parameters(params))
        .await
        .expect("acpx dispatch should start");
    let parsed: Value = serde_json::from_str(&response).expect("dispatch JSON");
    assert_eq!(parsed["execution_backend"], json!("acpx"));
    assert_eq!(parsed["acpx"]["permissions"], json!("approve-reads"));
    assert_eq!(parsed["acpx"]["mode"], json!("exec"));
    assert_eq!(parsed["acpx"]["session"], json!(null));
    assert!(
        parsed["acpx"]["prompt_file"]
            .as_str()
            .is_some_and(|path| path.ends_with("/prompt.md")),
        "acpx should execute the canonical Tachi prompt.md: {parsed:#}"
    );
    assert_eq!(
        parsed["acpx"]["controls"]["status"]["supported"],
        json!(false)
    );
    let dispatch_id = parsed["dispatch_id"].as_str().expect("dispatch id");
    let run_dir = temp_home.path().join("runs").join(dispatch_id);
    assert!(run_dir.join("prompt.md").exists());
    assert!(!run_dir.join("acpx_prompt.md").exists());

    let result = wait_for_dispatch_result(&run_dir).await;
    assert_eq!(result, "acpx done");

    let acpx_events =
        std::fs::read_to_string(run_dir.join("acpx_events.jsonl")).expect("acpx events");
    assert!(acpx_events.contains("tool_call_start"));
    let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory");
    assert!(trajectory.contains("execution_backend_prepared"));
    assert!(trajectory.contains("acpx_tool_event"));
    assert!(trajectory.contains("acpx_events_persisted"));
    let status = wait_for_dispatch_status(&run_dir).await;
    assert_eq!(status["execution_backend"], json!("acpx"));
    assert_eq!(status["acpx"]["mode"], json!("exec"));
    assert_eq!(
        status["acpx_events"]["final_response_extracted"],
        json!(true)
    );
}
