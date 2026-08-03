use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn lifecycle_release_note_skips_empty_optional_github_fields() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000004Z_release_note_empty_fields";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some(
            "## Acceptance criteria\n- Empty optional GitHub fields are skipped.".to_string(),
        ),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
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
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: None,
        base_ref: None,
        review_decision: Some(String::new()),
        mergeable: Some(String::new()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let mut params = task_params("status");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = crate::task_lifecycle::handle_task_release_note(&server, &params)
        .await
        .expect("release note should be generated");
    let parsed: Value = serde_json::from_str(&raw).expect("release_note response JSON");
    let note = parsed["release_note"].as_str().expect("release note text");
    assert!(!note.contains("Review: ``"), "{note}");
    assert!(!note.contains("Mergeable: ``"), "{note}");
}
