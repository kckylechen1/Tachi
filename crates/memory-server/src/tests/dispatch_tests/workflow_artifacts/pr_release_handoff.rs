use super::*;

#[test]
fn tachi_task_pr_status_parses_repo_number_and_pr_ref() {
    let mut params = task_params("pr_status");
    params.repo = Some("kckylechen1/tachi".to_string());
    params.number = Some(228);
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("repo+number"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.repo = None;
    params.number = None;
    params.pr_ref = Some("kckylechen1/tachi#228".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("owner/repo#number"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.pr_ref = Some("https://github.com/kckylechen1/tachi/pull/228".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("github PR URL"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.pr_ref = Some(" https://github.com/kckylechen1/tachi/pull/228/ ".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("trimmed github PR URL"),
        ("kckylechen1/tachi".to_string(), 228)
    );
}

#[test]
fn tachi_task_pr_status_rejects_ambiguous_pr_refs() {
    for pr_ref in [
        "",
        "kckylechen1/tachi#",
        "kckylechen1/tachi/extra#228",
        "https://github.com/kckylechen1/tachi/issues/228",
        "https://github.com/kckylechen1/tachi/pull/228/files",
    ] {
        let mut params = task_params("pr_status");
        params.pr_ref = Some(pr_ref.to_string());
        assert!(
            crate::tools::resolve_task_pr_status_target(&params).is_err(),
            "unexpectedly accepted pr_ref={pr_ref:?}"
        );
    }
}

#[test]
fn tachi_task_pr_status_builds_safe_merge_preview_params() {
    let mut params = task_params("pr_status");
    params.pr_ref = Some("kckylechen1/tachi#228".to_string());
    params.flow_id = Some("flow_pr_status".to_string());
    params.merge_policy = Some("strict".to_string());
    params.strategy = Some("squash".to_string());
    params.confirm = true;

    let gh_params =
        crate::tools::build_task_pr_status_gh_params(&params).expect("pr_status params");
    assert_eq!(gh_params.action, "safe_merge");
    assert_eq!(gh_params.repo, "kckylechen1/tachi");
    assert_eq!(gh_params.number, Some(228));
    assert_eq!(gh_params.dry_run, Some(true));
    assert!(!gh_params.confirm);
    assert_eq!(gh_params.flow_id.as_deref(), Some("flow_pr_status"));
    assert_eq!(gh_params.merge_policy.as_deref(), Some("strict"));
    assert_eq!(gh_params.merge_strategy, None);
}

#[tokio::test]
async fn tachi_task_pr_status_requires_repo_number_or_parseable_ref() {
    let server = make_server();
    let params = task_params("pr_status");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing target should fail before GitHub access");
    assert_eq!(
        err,
        "pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}

#[tokio::test]
async fn tachi_task_release_note_requires_flow_or_pr_ref_before_github_access() {
    let server = make_server();
    let params = task_params("release_note");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing release note target should fail before GitHub access");
    assert_eq!(
        err,
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_pr_handoff_writes_pr_body_with_verification_and_gaps() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260614T000002Z_pr_handoff";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 380,
        title: "Automate issue dispatch".to_string(),
        body: Some("## Acceptance criteria\n- PR handoff contains required evidence.".to_string()),
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
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "command": "cargo test -p memory-server tachi_task_pr_handoff", "status": "passed" }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut params = task_params("pr_handoff");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("pr_handoff should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("pr_handoff JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["safe_to_open"], json!(true));
    let body = parsed["pr_body"].as_str().expect("pr body");
    assert!(body.contains("Linked issue: kckylechen1/tachi#380"));
    assert!(body.contains("Overall: `passed`"));
    assert!(body.contains("Known Gaps / Review Gates"));
    assert!(body.contains("None recorded by Tachi automation gate"));
    let handoff_path = parsed["pr_handoff_path"].as_str().expect("handoff path");
    assert!(handoff_path.ends_with("pr_handoff.md"), "{handoff_path}");
    assert!(run_dir.join("pr_handoff.md").exists());
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_writes_flow_artifact_with_refs() {
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
                { "command": "cargo test -p memory-server tachi_task_release_note", "status": "passed" },
                { "kind": "gitleaks", "status": "passed" }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut params = task_params("release_note");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
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
        "`passed` cargo test -p memory-server tachi_task_release_note",
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

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_rejects_mismatched_pr_ref_for_cached_flow_pr() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000003Z_release_note_mismatch";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some("## Acceptance criteria\n- Mismatched PR refs are rejected.".to_string()),
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
        review_decision: None,
        mergeable: None,
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let mut params = task_params("release_note");
    params.flow_id = Some(flow_id.to_string());
    params.pr_ref = Some("kckylechen1/tachi#999".to_string());
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("mismatched pr_ref should be rejected");
    assert!(
        err.contains("release_note pr_ref mismatch"),
        "unexpected error: {err}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_skips_empty_optional_github_fields() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
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

    let mut params = task_params("release_note");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("release note should be generated");
    let parsed: Value = serde_json::from_str(&raw).expect("release_note response JSON");
    let note = parsed["release_note"].as_str().expect("release note text");
    assert!(!note.contains("Review: ``"), "{note}");
    assert!(!note.contains("Mergeable: ``"), "{note}");
}
