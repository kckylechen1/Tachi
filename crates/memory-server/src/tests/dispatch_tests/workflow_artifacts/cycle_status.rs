use super::*;

fn issue_snapshot(
    number: u64,
    doc_paths: Vec<&str>,
    spec_paths: Vec<&str>,
) -> crate::task_lifecycle::IssueSnapshot {
    crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number,
        title: "Project cycle read model".to_string(),
        body: Some("## Acceptance criteria\n- Cycle status reports linked contracts.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: format!("https://github.com/kckylechen1/tachi/issues/{number}"),
        doc_paths: doc_paths.into_iter().map(str::to_string).collect(),
        spec_paths: spec_paths.into_iter().map(str::to_string).collect(),
    }
}

fn pr_snapshot(number: u64) -> crate::task_lifecycle::PrSnapshot {
    crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number,
        title: "Add project cycle read model".to_string(),
        state: Some("OPEN".to_string()),
        url: format!("https://github.com/kckylechen1/tachi/pull/{number}"),
        head_ref: Some("feat/project-cycle-read-model".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    }
}

fn write_intake_flow(flow_id: &str, issue: &crate::task_lifecycle::IssueSnapshot) {
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Project cycle read model",
        issue,
        &automation_plan,
    )
    .expect("write intake artifacts");
}

fn write_passed_verification(flow_id: &str) {
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id,
            "overall": "passed",
            "items": [
                {
                    "id": "cargo-test-cycle-status",
                    "kind": "cargo_test",
                    "command": "cargo test -p memory-server cycle_status",
                    "status": "passed",
                    "required": true
                }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");
}

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
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    crate::shell_ops::merge_github_status(
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

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_cycle_status_reports_missing_spec_contract() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
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

#[tokio::test]
async fn tachi_task_cycle_status_requires_flow_issue_or_pr_target() {
    let server = make_server();
    let params = task_params("cycle_status");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing target should fail before GitHub access");
    assert_eq!(
        err,
        "cycle_status requires flow_id, issue_ref='owner/repo#123', or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}
