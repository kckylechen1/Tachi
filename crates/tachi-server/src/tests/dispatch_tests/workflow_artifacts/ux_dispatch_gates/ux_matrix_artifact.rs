use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_ux_matrix_writes_feature_workflow_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000005Z_ux_matrix_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some("## Acceptance criteria\n- UX matrix includes workflow gates.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()],
        spec_paths: vec!["docs/engineering/specs/dispatch-policy.md".to_string()],
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: Some("feat/task-intake-link-pr".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    crate::shell_ops::merge_github_status(
        &run_dir,
        json!({
            "merge_state": "ready",
            "policy": "standard",
            "requested_mode": "preview",
            "will_merge": false,
        }),
    )
    .expect("merge github status");
    let mut status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    status["dispatch_ids"] = json!(["dispatch-ux-matrix"]);
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&status).expect("status json"),
    )
    .expect("write status");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "id": "gitleaks", "kind": "gitleaks", "status": "passed", "required": true }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut release_params = task_params("release_note");
    release_params.flow_id = Some(flow_id.to_string());
    server
        .tachi_task(Parameters(release_params))
        .await
        .expect("release note should be generated");

    let mut params = task_params("ux_matrix");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("ux_matrix"));
    assert_eq!(parsed["overall"], json!("ready_for_close_loop"));
    let matrix = parsed["matrix"].as_array().expect("matrix array");
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("dispatch") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("verification") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("pr_status") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("release_note") && step["status"] == json!("passed") }));
    let ux_path = parsed["ux_matrix_path"]
        .as_str()
        .expect("ux matrix artifact path");
    assert!(ux_path.ends_with("ux_matrix.json"), "{ux_path}");
    assert!(run_dir.join("ux_matrix.json").exists());
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert!(status["artifacts"]["ux_matrix"]
        .as_str()
        .is_some_and(|path| path.ends_with("ux_matrix.json")));
}
