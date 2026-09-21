use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_gh_pr_handoff_writes_pr_body_with_verification_and_gaps() {
    let _lock = crate::utils::global_test_lock()
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
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "command": "cargo test -p tachi-server tachi_gh_pr_handoff", "status": "passed" }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");
    // The pr_handoff readiness verdict is claim-bound gate output over the
    // server-owned receipt store. Seed the claim and full canonical receipt
    // set at one fixed head so the happy path stays green.
    let receipt_head = "pr-handoff-fixture-head";
    crate::claims_ops::admit_agent_connection(&server, Some("agent.handoff".into()), true)
        .expect("local admission");
    let claim_params: crate::TachiTaskParams = serde_json::from_value(json!({
        "action": "claim",
        "flow_id": flow_id,
        "issue_ref": "kckylechen1/tachi#380",
        "branch": "lane/pr-handoff",
        "claim_role": "reviewer",
        "claim_mode": "read_only",
        "worktree_path": "",
        "claim_scope": ["crates/tachi-server/src/task_lifecycle/issue_flow.rs"],
        "expected_head": receipt_head,
        "lease_expires_at": "2030-01-01T00:00:00Z",
    }))
    .expect("claim params");
    crate::claims_ops::handle_task_claim(&server, &claim_params).expect("work claim");
    for kind in crate::verify_ops::MERGE_REQUIRED_RUN_KINDS {
        let receipt = serde_json::json!({
            "flow_id": flow_id,
            "kind": kind,
            "head_sha": receipt_head,
            "status": "passed",
            "reason": null,
            "exit_code": 0,
            "log_path": "/tmp/pr-handoff-seed.log",
            "duration_ms": 1,
            "ran_at": "2026-08-18T00:00:00Z",
            "timed_out": false,
            "kill_abandoned": false,
            "source_head": receipt_head,
            "executed_in_detached_copy": true,
            "copy_head_before": receipt_head,
            "copy_head_after": receipt_head,
            "copy_clean_before": true,
            "copy_clean_after": true,
            "tool_version": "seed-tool-1.0",
        });
        crate::verify_ops::seed_run_receipt_for_test(
            &server.tachi_home_dir(),
            flow_id,
            kind,
            &receipt,
        )
        .expect("seed receipt");
    }

    let mut params = task_params("status");
    params.flow_id = Some(flow_id.to_string());
    // pr_handoff is no longer a tachi_task facade action (#757/#974 moved
    // GitHub PR lifecycle actions to tachi_gh exclusively); call the
    // lifecycle handler directly, same as tachi_gh's router does.
    let raw = crate::task_lifecycle::handle_task_pr_handoff(&server, &params)
        .expect("pr_handoff should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("pr_handoff JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["safe_to_open"], json!(true));
    // #527: default receipt omits pr_body (path only); format=full restores it.
    assert!(
        parsed.get("pr_body").is_none(),
        "default pr_handoff must not echo full pr_body: {parsed}"
    );
    assert!(
        parsed["timing_ms"]["total"].as_u64().is_some(),
        "pr_handoff must expose timing_ms: {parsed}"
    );
    let handoff_path = parsed["pr_handoff_path"].as_str().expect("handoff path");
    assert!(handoff_path.ends_with("pr_handoff.md"), "{handoff_path}");
    assert!(run_dir.join("pr_handoff.md").exists());
    let file_body = std::fs::read_to_string(run_dir.join("pr_handoff.md")).expect("read handoff");
    assert!(file_body.contains("Linked issue: kckylechen1/tachi#380"));
    assert!(file_body.contains("Overall: `passed`"));
    assert!(file_body.contains("Known Gaps / Review Gates"));
    assert!(file_body.contains("None recorded by Tachi automation gate"));

    let mut full_params = params;
    full_params.format = Some("full".to_string());
    let full_raw = crate::task_lifecycle::handle_task_pr_handoff(&server, &full_params)
        .expect("pr_handoff format=full");
    let full: Value = serde_json::from_str(&full_raw).expect("full JSON");
    let body = full["pr_body"].as_str().expect("full pr_body");
    assert!(body.contains("Linked issue: kckylechen1/tachi#380"));
}
