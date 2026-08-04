use super::*;

#[tokio::test]
async fn safe_merge_strict_requires_flow_id_before_merge() {
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let mut policy = MergeGatePolicy::strict();
    policy.require_head_consistency = false;
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        None,
        &[],
        policy,
        None,
        false,
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "pending");
    assert_eq!(v["will_merge"], false);
    assert_eq!(v["merge_attempted"], false);
    assert!(v["decision"]["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "flow_or_issue:missing"));
    assert!(client.merge_calls().is_empty());
}

#[tokio::test]
async fn safe_merge_strict_accepts_linked_issue_without_flow_id() {
    let mut pr = ready_pr();
    pr.linked_issue_refs = vec!["https://github.com/o/r/issues/99".to_string()];
    let client = MockGhClient::new()
        .with_pr("o/r", pr)
        .with_checks("o/r", 42, vec![]);
    let mut policy = MergeGatePolicy::strict();
    policy.require_head_consistency = false;
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        None,
        &[],
        policy,
        None,
        false,
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "ready");
    assert_eq!(v["status_patch"]["flow"]["has_linked_issue"], true);
    assert_eq!(
        v["status_patch"]["flow"]["linked_issue_refs"][0],
        "https://github.com/o/r/issues/99"
    );
}

#[tokio::test]
async fn safe_merge_head_consistency_required_blocks_merge() {
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let mut policy = MergeGatePolicy::standard();
    policy.require_head_consistency = true;
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        None,
        &[],
        policy,
        None,
        false,
    )
    .await
    .expect("ok");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "pending");
    assert_eq!(v["will_merge"], false);
    assert_eq!(v["status_patch"]["head_consistency"]["state"], "unknown");
    assert_eq!(
        v["status_patch"]["head_consistency"]["head_consistent"],
        false
    );
    assert!(v["decision"]["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "head:consistency_unavailable"));
    assert!(client.merge_calls().is_empty());
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_with_flow_id_missing_verification_waits() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);

    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        Some("flow_missing-verification"),
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("ok");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "pending");
    assert_eq!(v["will_merge"], false);
    assert!(v["decision"]["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:missing"));
    assert!(client.merge_calls().is_empty());
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_failed_verification_blocks_even_permissive() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let flow = "flow_failed-verification";
    write_verification(tmp.path(), flow, "failed", "deadbeef");
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);

    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        Some(flow),
        &[],
        MergeGatePolicy::permissive(),
        None,
        false,
    )
    .await
    .expect("ok");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "blocked");
    assert!(v["decision"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:gitleaks:failed"));
    assert!(client.merge_calls().is_empty());
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_stale_verification_waits_on_head_mismatch() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let flow = "flow_stale-verification";
    write_verification(tmp.path(), flow, "passed", "oldsha");
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);

    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        Some(flow),
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("ok");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "pending");
    assert!(v["decision"]["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:gitleaks:stale"));
    assert!(client.merge_calls().is_empty());
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_strict_uses_passed_verification_for_head_consistency() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let flow = "flow_strict-verification";
    write_verification(tmp.path(), flow, "passed", "deadbeef");
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);

    let out = handle_github_safe_merge(
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
    .expect("ok");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "ready");
    assert_eq!(
        v["status_patch"]["head_consistency"]["state"],
        "verified_by_tachi_verification"
    );
    assert_eq!(
        v["status_patch"]["head_consistency"]["head_consistent"],
        true
    );
    assert!(client.merge_calls().is_empty());
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_strict_does_not_treat_not_required_verification_as_head_proof() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let flow = "flow_strict-not-required";
    let run_dir = tmp.path().join(flow);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow,
            "overall": "passed",
            "items": [
                {"id":"optional-check","status":"passed","head_sha":"deadbeef","required":false}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);

    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        Some(flow),
        &[],
        MergeGatePolicy::strict(),
        None,
        false,
    )
    .await
    .expect("ok");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "pending");
    assert_eq!(
        v["status_patch"]["head_consistency"]["head_consistent"],
        false
    );
    assert!(v["decision"]["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "head:consistency_unavailable"));
    assert!(client.merge_calls().is_empty());
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_records_missing_verification_from_tests_run() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let flow = "flow_record-tests-run";
    let tests_run = vec![
        "cargo test -p tachi-server gh_ops::safe_merge_tests".to_string(),
        "gitleaks detect --source .".to_string(),
    ];
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);

    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &tests_run,
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("ok");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], "ready");
    assert_eq!(v["status_patch"]["verification"]["overall"], "passed");
    assert_eq!(v["status_patch"]["verification"]["required_total"], 2);

    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(flow).join("verification.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ledger["overall"], "passed");
    assert_eq!(ledger["pr_ref"], "o/r#42");
    assert_eq!(ledger["head_sha"], "deadbeef");
    assert_eq!(ledger["items"].as_array().unwrap().len(), 2);
    assert!(ledger["items"].as_array().unwrap().iter().all(|item| {
        item.get("status").and_then(serde_json::Value::as_str) == Some("passed")
            && item.get("head_sha").and_then(serde_json::Value::as_str) == Some("deadbeef")
    }));
    assert!(client.merge_calls().is_empty());
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
