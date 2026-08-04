use super::*;

#[tokio::test]
async fn tachi_task_facade_defaults_to_markdown_and_keeps_json_escape_hatch() {
    let server = make_server();

    // tachi#1201 item 1: action='profiles' (like recommend/profile/card) now
    // defaults to markdown when format is omitted entirely.
    let mut default_params = task_params("profiles");
    default_params.format = None;
    let default_body = server
        .tachi_task(Parameters(default_params))
        .await
        .expect("default profiles should succeed");
    assert!(
        default_body.starts_with("## Tachi task profiles"),
        "{default_body}"
    );
    assert!(
        default_body.contains("| name | backend | model | role |"),
        "{default_body}"
    );
    assert!(
        serde_json::from_str::<Value>(&default_body).is_err(),
        "action='profiles' with format omitted must default to markdown, not JSON: {default_body}"
    );

    let mut json_params = task_params("profiles");
    json_params.format = Some("json".to_string());
    let json_body = server
        .tachi_task(Parameters(json_params))
        .await
        .expect("json profiles should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("json profiles JSON");
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
