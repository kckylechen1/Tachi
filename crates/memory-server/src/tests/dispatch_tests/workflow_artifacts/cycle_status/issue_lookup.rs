use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_cycle_status_can_locate_local_flow_by_issue_ref() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260626T000002Z_cycle_status_issue_lookup";
    let issue = issue_snapshot(
        440,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        vec!["docs/engineering/specs/project-cycle-read-model.md"],
    );
    write_intake_flow(flow_id, &issue);

    let mut params = task_params("cycle_status");
    params.issue_ref = Some("kckylechen1/tachi#440".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("cycle_status should find local flow");
    let parsed: Value = serde_json::from_str(&raw).expect("cycle_status JSON");

    assert_eq!(parsed["flow_id"], json!(flow_id));
    assert_eq!(parsed["issue_ref"], json!("kckylechen1/tachi#440"));
    assert_eq!(parsed["source"]["github_read"], json!(false));
    assert_eq!(
        parsed["github"]["issue_snapshot"]["source"],
        json!("flow_status")
    );
}
