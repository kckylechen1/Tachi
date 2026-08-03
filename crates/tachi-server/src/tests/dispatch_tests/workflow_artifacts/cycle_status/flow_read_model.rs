use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_cycle_status_reads_flow_artifacts_without_github() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260626T000001Z_cycle_status_happy";
    let issue = issue_snapshot(
        438,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        vec!["docs/engineering/specs/project-cycle-read-model.md"],
    );
    write_intake_flow(flow_id, &issue);
    let pr = pr_snapshot(439);
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("run dir");
    crate::task_lifecycle::merge_github_status(
        &run_dir,
        json!({
            "merge_state": "ready",
            "head_sha": "abc123",
            "policy": "standard",
            "requested_mode": "preview"
        }),
    )
    .expect("merge github status");
    write_passed_verification(flow_id);

    let mut params = task_params("cycle_status");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("cycle_status should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("cycle_status JSON");

    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("cycle_status"));
    assert_eq!(parsed["flow_id"], json!(flow_id));
    assert_eq!(parsed["stage"], json!("verified"));
    assert_eq!(parsed["issue_ref"], json!("kckylechen1/tachi#438"));
    assert_eq!(parsed["pr_ref"], json!("kckylechen1/tachi#439"));
    assert_eq!(parsed["github"]["merge_state"], json!("ready"));
    assert_eq!(parsed["verification"]["overall"], json!("passed"));
    assert_eq!(parsed["source"]["github_read"], json!(false));
    assert!(parsed["linked_docs"]
        .as_array()
        .is_some_and(|docs| docs.contains(&json!(
            "docs/engineering/architecture/project-cycle-memory-spine.md"
        ))));
    assert!(parsed["linked_specs"].as_array().is_some_and(
        |specs| specs.contains(&json!("docs/engineering/specs/project-cycle-read-model.md"))
    ));
    assert!(parsed["spec_drift"]
        .as_array()
        .is_some_and(|drift| drift.is_empty()));
    assert!(parsed["next_action"]
        .as_str()
        .is_some_and(|next| next.contains("release_note")));
    assert_eq!(parsed["artifacts"]["verification"]["exists"], json!(true));
    assert!(parsed["events"].as_array().is_some_and(|events| {
        events
            .iter()
            .any(|event| event["event"] == json!("github_pr_updated"))
    }));
}
