use super::*;

const PR_HEAD: &str = "historical-pr-head";
const MAIN_HEAD: &str = "accepted-main-head";

// Drive the public status handler with real claim admission/handoff and the
// production caller-ledger writer. Only canonical receipt execution is seeded.
#[allow(clippy::await_holding_lock)]
async fn cycle_with_verification_heads(
    merge_state: &str,
    claim_heads: Option<(&str, &str)>,
    receipt_head: Option<&str>,
    failed_receipt: bool,
    ledger_head: &str,
) -> Value {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_cycle_status_claim_head";
    write_intake_flow(
        flow_id,
        &issue_snapshot(
            1112,
            vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
            vec!["docs/engineering/specs/project-cycle-read-model.md"],
        ),
    );
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr_snapshot(1951), None)
        .expect("link PR");
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("run dir");
    crate::task_lifecycle::merge_github_status(
        &run_dir,
        json!({"merge_state": merge_state, "head_sha": PR_HEAD}),
    )
    .expect("cache GitHub status");

    let mut binding = None;
    if let Some((initial_head, expected_head)) = claim_heads {
        crate::claims_ops::admit_agent_connection(&server, Some("agent.cycle-status".into()), true)
            .expect("local admission");
        let params: crate::TachiTaskParams = serde_json::from_value(json!({
            "action": "claim", "flow_id": flow_id,
            "issue_ref": "kckylechen1/tachi#1112", "branch": "lane/cycle-status",
            "claim_role": "reviewer", "claim_mode": "read_only", "worktree_path": "",
            "claim_scope": ["crates/tachi-server/src/task_lifecycle/cycle_status.rs"],
            "expected_head": initial_head, "lease_expires_at": "2030-01-01T00:00:00Z",
        }))
        .expect("claim params");
        let claim = crate::claims_ops::handle_task_claim(&server, &params).expect("claim");
        let claim_id = claim["claim_id"].as_str().expect("claim id");
        assert_eq!(claim["transition_version"], 0);
        let revision = if initial_head != expected_head {
            let params: crate::TachiTaskParams = serde_json::from_value(json!({
                "action": "handoff", "claim_id": claim_id, "transition_version": 0,
                "flow_id": flow_id, "claim_role": "reviewer", "claim_mode": "read_only",
                "worktree_path": "",
                "claim_scope": ["crates/tachi-server/src/task_lifecycle/cycle_status.rs"],
                "expected_head": expected_head, "lease_expires_at": "2030-01-02T00:00:00Z",
            }))
            .expect("handoff params");
            let handoff = crate::claims_ops::handle_task_handoff(&server, &params)
                .expect("explicit claim handoff");
            assert_eq!(handoff["transition_version"], 1);
            1
        } else {
            0
        };
        binding = Some((claim_id.to_string(), revision, expected_head));
    }

    let params: crate::TachiVerifyParams = serde_json::from_value(json!({
        "action": "record", "flow_id": flow_id, "head_sha": ledger_head,
        "kind": "fmt", "check_id": "fmt", "required": true,
    }))
    .expect("record params");
    let recorded =
        crate::verify_ops::record_items(&params, "passed").expect("record caller ledger");
    assert_eq!(recorded["verification"]["head_sha"], ledger_head);
    if let Some(head) = receipt_head {
        for kind in crate::verify_ops::MERGE_REQUIRED_RUN_KINDS {
            let failed = failed_receipt && *kind == "fmt";
            let receipt = json!({
                "flow_id": flow_id, "kind": kind, "head_sha": head,
                "status": if failed { "failed" } else { "passed" },
                "reason": if failed { Some("failed") } else { None },
                "exit_code": if failed { 1 } else { 0 },
                "log_path": "/tmp/cycle-status-seed.log", "duration_ms": 1,
                "ran_at": "2026-08-18T00:00:00Z", "timed_out": false, "kill_abandoned": false,
                "source_head": head, "executed_in_detached_copy": true,
                "copy_head_before": head, "copy_head_after": head,
                "copy_clean_before": true, "copy_clean_after": true, "tool_version": "seed-tool-1.0",
            });
            crate::verify_ops::seed_run_receipt_for_test(
                &server.tachi_home_dir(),
                flow_id,
                kind,
                &receipt,
            )
            .expect("seed canonical receipt");
        }
    }
    if let Some((claim_id, revision, expected_head)) = binding {
        let gate = crate::verify_ops::evaluate_verification_gate(&server, Some(flow_id))
            .expect("gate evaluation")
            .expect("gate exists");
        assert_eq!(gate["claim_id"], claim_id);
        assert_eq!(gate["claim_transition_version"], revision);
        assert_eq!(gate["expected_head"], expected_head);
    }
    let mut params = task_params("status");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("public status handler");
    let cycle = cycle_view(&raw);
    assert_eq!(cycle["source"]["github_read"], false);
    assert_eq!(cycle["verification"]["head_sha"], ledger_head);
    cycle
}

fn has_drift(cycle: &Value, kind: &str) -> bool {
    cycle["spec_drift"]
        .as_array()
        .expect("drift array")
        .iter()
        .any(|item| item["kind"] == kind)
}

#[tokio::test]
async fn cycle_status_merged_explicit_rebind_does_not_compare_historical_pr_head() {
    let cycle = cycle_with_verification_heads(
        "merged",
        Some((PR_HEAD, MAIN_HEAD)),
        Some(MAIN_HEAD),
        false,
        MAIN_HEAD,
    )
    .await;
    assert_eq!(cycle["verification_verdict"], "passed");
    assert!(!has_drift(&cycle, "verification_not_passed"));
    assert!(
        !has_drift(&cycle, "verification_head_mismatch"),
        "explicit merged rebind must not coach verification back to historical PR head"
    );
}

#[tokio::test]
async fn cycle_status_merged_failed_rebind_remains_failed() {
    let cycle = cycle_with_verification_heads(
        "merged",
        Some((PR_HEAD, MAIN_HEAD)),
        Some(MAIN_HEAD),
        true,
        MAIN_HEAD,
    )
    .await;
    assert_eq!(cycle["verification_verdict"], "failed");
    assert!(has_drift(&cycle, "verification_not_passed"));
    assert!(!has_drift(&cycle, "verification_head_mismatch"));
}

#[tokio::test]
async fn cycle_status_merged_missing_receipts_remains_pending() {
    let cycle =
        cycle_with_verification_heads("merged", Some((PR_HEAD, MAIN_HEAD)), None, false, MAIN_HEAD)
            .await;
    assert_eq!(cycle["verification_verdict"], "pending");
    assert!(has_drift(&cycle, "verification_not_passed"));
    assert!(!has_drift(&cycle, "verification_head_mismatch"));
}

#[tokio::test]
async fn cycle_status_merged_old_receipts_after_rebind_remains_pending() {
    let cycle = cycle_with_verification_heads(
        "merged",
        Some((PR_HEAD, MAIN_HEAD)),
        Some(PR_HEAD),
        false,
        MAIN_HEAD,
    )
    .await;
    assert_eq!(cycle["verification_verdict"], "pending");
    assert!(has_drift(&cycle, "verification_not_passed"));
    assert!(!has_drift(&cycle, "verification_head_mismatch"));
}

#[tokio::test]
async fn cycle_status_open_claim_mismatch_cannot_be_hidden_by_caller_ledger() {
    let cycle = cycle_with_verification_heads(
        "ready",
        Some((PR_HEAD, MAIN_HEAD)),
        Some(MAIN_HEAD),
        false,
        PR_HEAD,
    )
    .await;
    assert_eq!(cycle["verification_verdict"], "passed");
    assert!(
        has_drift(&cycle, "verification_head_mismatch"),
        "caller ledger copying PR head must not hide authoritative claim mismatch"
    );
}

#[tokio::test]
async fn cycle_status_matching_claim_cannot_gain_drift_from_caller_ledger() {
    let cycle = cycle_with_verification_heads(
        "ready",
        Some((PR_HEAD, PR_HEAD)),
        Some(PR_HEAD),
        false,
        MAIN_HEAD,
    )
    .await;
    assert_eq!(cycle["verification_verdict"], "passed");
    assert!(
        !has_drift(&cycle, "verification_head_mismatch"),
        "caller ledger must not invent head drift when authoritative claim matches PR"
    );
}

#[tokio::test]
async fn cycle_status_missing_claim_cannot_gain_authority_from_caller_ledger() {
    let cycle = cycle_with_verification_heads("merged", None, Some(PR_HEAD), false, PR_HEAD).await;
    assert_eq!(cycle["verification_verdict"], "unverified");
    assert!(has_drift(&cycle, "verification_not_passed"));
    assert!(!has_drift(&cycle, "verification_head_mismatch"));
}
