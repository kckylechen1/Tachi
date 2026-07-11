use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn lifecycle_release_note_writes_flow_artifact_with_refs() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000002Z_release_note_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some(
            "## Acceptance criteria\n- Release note includes issue, PR, and verification."
                .to_string(),
        ),
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
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: Some("feat/task-intake-link-pr".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "command": "cargo test -p tachi-server lifecycle_release_note", "status": "passed" },
                { "kind": "gitleaks", "status": "passed" }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut params = task_params("status");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = crate::task_lifecycle::handle_task_release_note(&server, &params)
        .await
        .expect("release note should be generated");
    let parsed: Value = serde_json::from_str(&raw).expect("release_note response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("release_note"));
    assert_eq!(parsed["flow_id"], json!(flow_id));
    assert_eq!(parsed["issue_ref"], json!("kckylechen1/tachi#194"));
    assert_eq!(parsed["pr_ref"], json!("kckylechen1/tachi#230"));
    assert_eq!(parsed["inputs"]["verification_present"], json!(true));
    let note_path = parsed["release_note_path"]
        .as_str()
        .expect("release note path");
    assert!(note_path.ends_with("release_note.md"), "{note_path}");
    assert!(run_dir.join("release_note.md").exists());
    let note = std::fs::read_to_string(run_dir.join("release_note.md")).expect("release note");
    for expected in [
        "# Release Note",
        "Issue: `kckylechen1/tachi#194`",
        "PR: `kckylechen1/tachi#230`",
        "Merge state: `merged`",
        "spec: `docs/engineering/specs/dispatch-policy.md`",
        "doc: `docs/engineering/architecture/subagent-eval-system.md`",
        "Overall: `passed`",
        "`passed` cargo test -p tachi-server lifecycle_release_note",
    ] {
        assert!(note.contains(expected), "missing {expected}: {note}");
    }
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert_eq!(status["state"], json!("release_note_generated"));
    assert!(status["release_note_path"]
        .as_str()
        .is_some_and(|path| path.ends_with("release_note.md")));
}
