use super::*;

#[tokio::test]
async fn safe_merge_dry_run_ready_does_not_call_pr_merge() {
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        None,
        &[],
        MergeGatePolicy::standard(),
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "ready");
    assert_eq!(v["mode"], "standard");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["will_merge"], false);
    assert_eq!(v["requested_mode"], "preview");
    assert_eq!(v["merge_attempted"], false);
    assert_eq!(v["merge_executed"], false);
    assert!(v["merged_sha"].is_null());
    assert_eq!(v["status_patch"]["checks"]["required"], true);
    assert_eq!(v["status_patch"]["review"]["required"], true);
    assert_eq!(
        v["status_patch"]["head_consistency"]["head_sha"],
        "deadbeef"
    );
    assert_eq!(v["status_patch"]["head_consistency"]["state"], "unknown");
    assert_eq!(
        v["status_patch"]["head_consistency"]["head_consistent"],
        false
    );
    assert_eq!(
        v["status_patch"]["head_consistency"]["source"],
        "single_pr_snapshot"
    );
    assert_eq!(v["event"]["payload"]["requested_mode"], "preview");
    assert_eq!(v["event"]["kind"], "github_review_gate_passed");
    assert!(client.merge_calls().is_empty());
}

#[tokio::test]
async fn safe_merge_ready_executes_merge_when_not_dry_run() {
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        None,
        &[],
        MergeGatePolicy::standard(),
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "merged");
    assert_eq!(v["will_merge"], true);
    assert_eq!(v["requested_mode"], "merge_requested");
    assert_eq!(v["merge_attempted"], true);
    assert_eq!(v["merge_executed"], true);
    assert!(v["merged_sha"].is_string());
    assert_eq!(v["event"]["kind"], "github_pr_merged");
    assert_eq!(v["event"]["payload"]["requested_mode"], "merge_requested");
    assert_eq!(v["event"]["payload"]["merge_attempted"], true);
    assert_eq!(v["event"]["payload"]["merge_executed"], true);
    let calls = client.merge_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "o/r");
    assert_eq!(calls[0].1, 42);
    assert_eq!(calls[0].3, "deadbeef");
}

#[tokio::test]
async fn safe_merge_observes_already_merged_pr_without_blocking() {
    let client = MockGhClient::new()
        .with_pr("o/r", already_merged_pr())
        .with_checks("o/r", 42, vec![]);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        None,
        &[],
        MergeGatePolicy::strict(),
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "merged");
    assert_eq!(v["decision"]["decision"], "already_merged");
    assert_eq!(v["already_merged"], true);
    assert_eq!(v["will_merge"], false);
    assert_eq!(v["merge_attempted"], false);
    assert_eq!(v["merge_executed"], false);
    assert_eq!(v["status_patch"]["pr_state"], "MERGED");
    assert_eq!(v["status_patch"]["merge_state"], "merged");
    assert_eq!(
        v["status_patch"]["head_consistency"]["state"],
        "not_required_for_merged_pr"
    );
    assert_eq!(
        v["status_patch"]["head_consistency"]["head_consistent"],
        true
    );
    assert_eq!(v["event"]["kind"], "github_pr_merged");
    assert_eq!(v["event"]["payload"]["already_merged"], true);
    assert!(client.merge_calls().is_empty());
}

#[tokio::test]
async fn safe_merge_reports_head_sha_mismatch_from_merge_client() {
    let client = MockGhClient::new().with_pr("o/r", ready_pr());
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        None,
        &[],
        MergeGatePolicy::standard(),
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["event"]["payload"]["head_sha"], "deadbeef");
    assert_eq!(client.merge_calls()[0].3, "deadbeef");
}

#[tokio::test]
async fn safe_merge_blocked_does_not_call_pr_merge_even_when_not_dry_run() {
    let client = MockGhClient::new()
        .with_pr("o/r", blocked_draft_pr())
        .with_checks("o/r", 42, vec![]);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        None,
        &[],
        MergeGatePolicy::standard(),
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "blocked");
    assert_eq!(v["event"]["kind"], "github_merge_blocked");
    let reasons = v["event"]["payload"]["reasons"].as_array().unwrap();
    assert!(reasons.iter().any(|r| r == "draft"));
    assert!(client.merge_calls().is_empty());
}

#[tokio::test]
async fn safe_merge_pending_emits_checks_polled() {
    let client = MockGhClient::new()
        .with_pr("o/r", pending_pr())
        .with_checks("o/r", 42, vec![]);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        None,
        &[],
        MergeGatePolicy::standard(),
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "pending");
    assert_eq!(v["event"]["kind"], "github_checks_polled");
    assert!(client.merge_calls().is_empty());
}

#[tokio::test]
async fn safe_merge_skipped_checks_waits_and_labels_check_state() {
    let client = MockGhClient::new()
        .with_pr("o/r", skipped_checks_pr())
        .with_checks(
            "o/r",
            42,
            vec![CheckRun {
                name: "conditional-ci".to_string(),
                status: "completed".to_string(),
                conclusion: Some("skipped".to_string()),
            }],
        );
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        None,
        &[],
        MergeGatePolicy::standard(),
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "pending");
    assert_eq!(v["status_patch"]["checks"]["state"], "skipped");
    assert!(v["decision"]["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item == "checks:skipped"));
    assert!(client.merge_calls().is_empty());
}
