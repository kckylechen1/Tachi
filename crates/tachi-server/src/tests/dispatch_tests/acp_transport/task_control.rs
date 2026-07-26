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
async fn tachi_task_wait_returns_terminal_dispatch_status() {
    let server = make_server();
    let dispatch_id = "dispatch-wait-complete";
    let run_dir = crate::dispatch_ops::runs_dir_for_server(&server).join(dispatch_id);
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

/// #1001 round 2 item 1: `tachi_task(action='cancel')` must release the
/// presence claim its dispatch registered — same discipline as
/// `tachi_complete` (see `completion_eval::completion_record::
/// presence_claim_release`), and the module doc in
/// `memcore::session_claims` promises manual `release`, `complete`, and
/// `cancel` all route through the single release path.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_cancel_releases_the_presence_claim_registered_for_its_dispatch_id() {
    let (server, temp_home) = make_server_with_temp_home();
    let temp_python = tempfile::tempdir().expect("temp python module");
    write_fake_acpx_control_module(temp_python.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let dispatch_id = "99991231T235955Z-presence-release-cancel";
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(dispatch_id);
    write_acpx_control_fixture(&run_dir, dispatch_id);

    crate::claims_ops::auto_register_or_heartbeat_claim(
        &server,
        &crate::claims_ops::ClaimHookInput {
            issue_ref: Some("org/repo#1001".to_string()),
            flow_id: None,
            dispatch_id: Some(dispatch_id.to_string()),
            branch: Some("feat/x".to_string()),
            declared_file_scope: None,
        },
    );
    let live_before = crate::claims_ops::list_live_claims_for_briefing(&server);
    assert!(
        live_before
            .iter()
            .any(|c| c.dispatch_id.as_deref() == Some(dispatch_id)),
        "fixture sanity: claim must be live before cancel"
    );

    let mut params = task_params("cancel");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(5);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("cancel should succeed");
    let parsed: Value = serde_json::from_str(&response).expect("cancel JSON");
    assert_eq!(parsed["status"], json!("cancel_requested"));

    let live_after = crate::claims_ops::list_live_claims_for_briefing(&server);
    assert!(
        !live_after
            .iter()
            .any(|c| c.dispatch_id.as_deref() == Some(dispatch_id)),
        "claim for dispatch_id={dispatch_id} must no longer be active after cancel: {live_after:?}"
    );
}

/// The already-terminal early-return branch in `handle_tachi_task_cancel`
/// must ALSO release the claim — a dispatch that reached a terminal state
/// through some other path (e.g. the watchdog) but was never explicitly
/// completed/cancelled should not leave its presence claim stuck active
/// forever just because a caller's cancel request arrives after the fact.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_cancel_on_already_terminal_dispatch_still_releases_the_claim() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = "99991231T235954Z-presence-release-cancel-terminal";
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "already terminal before cancel arrives",
            "state": "TASK_STATE_COMPLETED",
            "exit_code": 0,
            "updated_at": Utc::now().to_rfc3339(),
        }))
        .expect("status json"),
    )
    .expect("write status");

    crate::claims_ops::auto_register_or_heartbeat_claim(
        &server,
        &crate::claims_ops::ClaimHookInput {
            issue_ref: Some("org/repo#1001".to_string()),
            flow_id: None,
            dispatch_id: Some(dispatch_id.to_string()),
            branch: Some("feat/x".to_string()),
            declared_file_scope: None,
        },
    );

    let mut params = task_params("cancel");
    params.dispatch_id = Some(dispatch_id.to_string());
    params.timeout_secs = Some(5);
    let response = server
        .tachi_task(Parameters(params))
        .await
        .expect("cancel on already-terminal dispatch should still succeed");
    let parsed: Value = serde_json::from_str(&response).expect("cancel JSON");
    assert_eq!(parsed["status"], json!("already_terminal"));

    let live_after = crate::claims_ops::list_live_claims_for_briefing(&server);
    assert!(
        !live_after
            .iter()
            .any(|c| c.dispatch_id.as_deref() == Some(dispatch_id)),
        "claim must be released even on the already-terminal early-return branch: {live_after:?}"
    );
}
