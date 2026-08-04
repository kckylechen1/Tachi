use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_persists_status_and_event_when_flow_id_supplied() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    // Force shell_runs_root() to the tempdir via env override.
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let flow = "flow_test-safe-merge";
    write_verification(tmp.path(), flow, "passed", "deadbeef");
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["persisted"], true);
    assert_eq!(v["requested_mode"], "preview");
    let run_dir = tmp.path().join(flow);
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["github"]["merge_state"], "ready");
    assert_eq!(status["github"]["policy"], "standard");
    assert_eq!(status["github"]["will_merge"], false);
    assert_eq!(status["github"]["requested_mode"], "preview");
    assert_eq!(status["github"]["merge_attempted"], false);
    assert_eq!(status["github"]["merge_executed"], false);
    assert_eq!(
        status["github"]["head_consistency"]["source"],
        "single_pr_snapshot"
    );
    assert_eq!(status["github"]["pr_number"], 42);
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("\"github_review_gate_passed\""));
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_persists_pending_blocked_and_merged_flow_events() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let pending_client = MockGhClient::new()
        .with_pr("o/r", pending_pr())
        .with_checks("o/r", 42, vec![]);
    handle_github_safe_merge(
        &pending_client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some("flow_pending-safe-merge"),
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("pending ok");
    let pending_dir = tmp.path().join("flow_pending-safe-merge");
    let pending_status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(pending_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(pending_status["github"]["merge_state"], "pending");
    assert!(std::fs::read_to_string(pending_dir.join("events.jsonl"))
        .unwrap()
        .contains("\"github_checks_polled\""));

    let blocked_client = MockGhClient::new()
        .with_pr("o/r", blocked_draft_pr())
        .with_checks("o/r", 42, vec![]);
    handle_github_safe_merge(
        &blocked_client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        Some("flow_blocked-safe-merge"),
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("blocked ok");
    let blocked_dir = tmp.path().join("flow_blocked-safe-merge");
    let blocked_status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(blocked_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(blocked_status["github"]["merge_state"], "blocked");
    assert_eq!(
        blocked_status["github"]["requested_mode"],
        "merge_requested"
    );
    assert_eq!(blocked_status["github"]["merge_attempted"], false);
    assert_eq!(blocked_status["github"]["merge_executed"], false);
    assert!(std::fs::read_to_string(blocked_dir.join("events.jsonl"))
        .unwrap()
        .contains("\"github_merge_blocked\""));
    assert!(blocked_client.merge_calls().is_empty());

    let merged_client =
        MockGhClient::new()
            .with_pr("o/r", ready_pr())
            .with_checks("o/r", 42, vec![]);
    write_verification(tmp.path(), "flow_merged-safe-merge", "passed", "deadbeef");
    handle_github_safe_merge(
        &merged_client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        Some("flow_merged-safe-merge"),
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("merged ok");
    let merged_dir = tmp.path().join("flow_merged-safe-merge");
    let merged_status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(merged_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(merged_status["github"]["merge_state"], "merged");
    assert_eq!(merged_status["github"]["will_merge"], true);
    assert_eq!(merged_status["github"]["requested_mode"], "merge_requested");
    assert_eq!(merged_status["github"]["merge_attempted"], true);
    assert_eq!(merged_status["github"]["merge_executed"], true);
    assert!(std::fs::read_to_string(merged_dir.join("events.jsonl"))
        .unwrap()
        .contains("\"github_pr_merged\""));
    assert_eq!(merged_client.merge_calls().len(), 1);

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_persists_observed_merged_pr_without_overwriting_it_blocked() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let client = MockGhClient::new()
        .with_pr("o/r", already_merged_pr())
        .with_checks("o/r", 42, vec![]);
    let flow = "flow_observed-merged-safe-merge";
    handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &[],
        MergeGatePolicy::strict(),
        None,
        false,
    )
    .await
    .expect("observed merged ok");
    let run_dir = tmp.path().join(flow);
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["github"]["pr_state"], "MERGED");
    assert_eq!(status["github"]["merge_state"], "merged");
    assert_eq!(status["github"]["already_merged"], true);
    assert_eq!(status["github"]["merge_attempted"], false);
    assert_eq!(status["github"]["merge_executed"], false);
    assert_eq!(status["github"]["requested_mode"], "preview");
    assert!(std::fs::read_to_string(run_dir.join("events.jsonl"))
        .unwrap()
        .contains("\"already_merged\":true"));
    assert!(client.merge_calls().is_empty());

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn safe_merge_rejects_invalid_flow_id() {
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let err = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some("../escape"),
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect_err("invalid flow id should fail");
    assert!(err.contains("Invalid flow_id"));
}

#[tokio::test]
async fn safe_merge_propagates_pr_view_not_found() {
    let client = MockGhClient::new(); // no PRs registered
    let err = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        None,
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect_err("should fail");
    assert!(err.contains("pr_view failed"));
    assert!(err.contains("not found") || err.contains("NotFound"));
}
