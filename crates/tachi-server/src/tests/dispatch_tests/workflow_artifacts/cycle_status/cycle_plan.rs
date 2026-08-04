use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_cycle_plan_returns_ordered_next_actions() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260628T000001Z_cycle_plan_ready_release";
    let issue = issue_snapshot(
        450,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        vec!["docs/engineering/specs/project-cycle-read-model.md"],
    );
    write_intake_flow(flow_id, &issue);
    let pr = pr_snapshot(451);
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

    let mut params = task_params("cycle_plan");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("cycle_plan should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("cycle_plan JSON");

    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("cycle_plan"));
    assert_eq!(parsed["flow_id"], json!(flow_id));
    assert_eq!(parsed["next_step"]["id"], json!("release_note"));
    assert_eq!(parsed["next_step"]["status"], json!("ready"));
    assert_eq!(parsed["readiness"]["ready_for_release_note"], json!(true));
    assert!(parsed["steps"].as_array().is_some_and(|steps| {
        steps
            .iter()
            .any(|step| step["id"] == json!("pr_status") && step["status"] == json!("passed"))
    }));
    assert!(parsed["status_summary"]["linked_specs"]
        .as_array()
        .is_some_and(
            |specs| specs.contains(&json!("docs/engineering/specs/project-cycle-read-model.md"))
        ));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_cycle_plan_stops_on_docs_without_specs() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260628T000002Z_cycle_plan_missing_spec";
    let issue = issue_snapshot(
        452,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        Vec::new(),
    );
    write_intake_flow(flow_id, &issue);

    let mut params = task_params("cycle_plan");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("cycle_plan should report contract gap");
    let parsed: Value = serde_json::from_str(&raw).expect("cycle_plan JSON");

    assert_eq!(parsed["next_step"]["id"], json!("contract"));
    assert_eq!(parsed["next_step"]["status"], json!("needs_confirmation"));
    assert_eq!(
        parsed["readiness"]["dispatch_needs_leader_confirmation"],
        json!(true)
    );
    assert!(parsed["current_blockers"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("explicit spec"))
                || item["kind"] == json!("missing_linked_specs")
        })
    }));
}
