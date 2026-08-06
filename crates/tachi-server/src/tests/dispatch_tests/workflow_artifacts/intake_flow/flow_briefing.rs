use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_intake_and_link_pr_artifacts_feed_briefing() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000001Z_intake_link_pr_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some("## Acceptance criteria\n- Flow artifacts feed briefing.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()],
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
        number: 229,
        title: "Add task PR status preview".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/229".to_string(),
        head_ref: Some("feat/task-pr-status".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("run dir");
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status json");
    assert_eq!(status["issue_ref"], json!("kckylechen1/tachi#194"));
    assert_eq!(status["pr_ref"], json!("kckylechen1/tachi#229"));
    assert_eq!(status["github"]["issue_number"], json!(194));
    assert_eq!(status["github"]["pr_number"], json!(229));
    assert_eq!(status["github"]["merge_state"], json!("merged"));
    assert!(run_dir.join("instruction.md").exists());
    let instruction = std::fs::read_to_string(run_dir.join("instruction.md")).unwrap();
    assert!(
        instruction.contains("tachi_task(action='cycle_status', flow_id=...)"),
        "intake instruction should route agents through cycle_status: {instruction}"
    );
    assert!(
        !instruction.contains("next_step"),
        "intake instruction must not teach retired next_step for cycle_status: {instruction}"
    );
    let recommended = status["automation_plan"]["recommended_next_action"]
        .as_str()
        .expect("automation_plan.recommended_next_action");
    assert!(
        recommended.contains("follow its next_action"),
        "intake automation_plan must teach cycle_status.next_action, got: {recommended}"
    );
    assert!(
        !recommended.contains("next_step"),
        "intake automation_plan must not teach retired next_step, got: {recommended}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(events.contains("github_issue_linked"), "{events}");
    assert!(events.contains("github_pr_updated"), "{events}");
    assert_eq!(
        crate::task_lifecycle::resolve_link_pr_issue_ref(flow_id, None).expect("inherited issue"),
        Some("kckylechen1/tachi#194".to_string())
    );
    assert!(
        crate::task_lifecycle::resolve_link_pr_issue_ref(flow_id, Some("other/repo#999"))
            .expect_err("mismatched issue_ref should be rejected")
            .contains("link_pr issue_ref mismatch")
    );

    let mut params = task_params("briefing");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("briefing should read flow docs");
    let briefing: Value = serde_json::from_str(&raw).expect("briefing JSON");
    assert!(briefing["canonical_docs"]
        .as_array()
        .is_some_and(|docs| docs.iter().any(|doc| {
            doc["kind"] == json!("flow_doc")
                && doc["path"] == json!("docs/engineering/architecture/subagent-eval-system.md")
        })));
    assert!(briefing["run_artifacts"]
        .as_array()
        .is_some_and(|artifacts| artifacts.iter().any(|artifact| {
            artifact["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("instruction.md"))
                && artifact["exists"] == json!(true)
        })));
    assert!(
        briefing["next_action"]
            .as_str()
            .is_some_and(|action| action.contains("tachi_task(action='cycle_status'")),
        "flow-bound briefing should route agents through cycle_status when no worker is active: {briefing:#}"
    );
}
