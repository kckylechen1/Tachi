use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_cycle_status_reports_missing_spec_contract() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260626T000003Z_cycle_status_missing_spec";
    let issue = issue_snapshot(
        441,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        Vec::new(),
    );
    write_intake_flow(flow_id, &issue);

    let mut params = task_params("cycle_status");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("cycle_status should report drift");
    let parsed: Value = serde_json::from_str(&raw).expect("cycle_status JSON");

    assert_eq!(parsed["stage"], json!("intake"));
    assert!(parsed["spec_drift"].as_array().is_some_and(|drift| {
        drift
            .iter()
            .any(|item| item["kind"] == json!("missing_linked_specs"))
    }));
    assert!(parsed["next_action"]
        .as_str()
        .is_some_and(|next| next.contains("spec")));
}
