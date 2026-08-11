use super::*;

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
