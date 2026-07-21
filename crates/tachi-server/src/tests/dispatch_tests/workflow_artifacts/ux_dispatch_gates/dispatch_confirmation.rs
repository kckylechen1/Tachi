use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_dispatch_requires_leader_confirmation_for_blocked_issue_flow() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    // Default `make_server()` uses the standard tool profile, which denies
    // `tachi_task`/`dispatch` before the confirmation gate. Admin is required
    // so this test reaches `dispatch requires leader confirmation`.
    server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
    if !tachi_hub::facade_action_allowed(
        "tachi_task",
        Some("dispatch"),
        server.active_tool_profile(),
    ) {
        eprintln!(
            "SKIP: tachi_task/dispatch still denied after set_tool_profile(admin); \
             cannot reach confirmation gate"
        );
        panic!(
            "precondition failed: tachi_task/dispatch must be allowed under admin \
             tool profile to exercise the confirmation gate"
        );
    }
    let flow_id = "flow_20260614T000001Z_blocked_dispatch_gate";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 380,
        title: "Automate issue dispatch".to_string(),
        body: Some("Do the automation.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/380".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Automate issue dispatch",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");

    let mut params = task_params("dispatch");
    params.flow_id = Some(flow_id.to_string());
    params.issue_ref = Some("kckylechen1/tachi#380".to_string());
    params.task = Some("Automate issue dispatch".to_string());
    params.agent = Some("codex".to_string());
    params.dispatch_reason = Some(tachi_params::TachiDispatchReason::ExplicitUserRequest);
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("dispatch should fail before spawning an agent");
    assert!(
        err.contains("dispatch requires leader confirmation"),
        "unexpected error: {err}"
    );
    assert!(
        err.contains("missing_acceptance_criteria"),
        "unexpected error: {err}"
    );
}
