use super::*;
use crate::gh_safe_merge::{CheckRun, MockGhClient};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn ready_pr() -> PrState {
    PrState {
        number: 42,
        state: PrLifecycleState::Open,
        mergeable: Mergeable::Mergeable,
        review_decision: Some(ReviewDecision::Approved),
        checks: ChecksState::Success,
        is_draft: false,
        head_sha: "deadbeef".to_string(),
        linked_issue_refs: Vec::new(),
        head_consistent: None,
    }
}

fn blocked_draft_pr() -> PrState {
    PrState {
        is_draft: true,
        ..ready_pr()
    }
}

fn pending_pr() -> PrState {
    PrState {
        mergeable: Mergeable::Unknown,
        ..ready_pr()
    }
}

fn skipped_checks_pr() -> PrState {
    PrState {
        checks: ChecksState::Skipped,
        ..ready_pr()
    }
}

fn write_verification(root: &std::path::Path, flow: &str, status: &str, head_sha: &str) {
    let run_dir = root.join(flow);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow,
            "head_sha": head_sha,
            "overall": status,
            "updated_at": "2026-06-08T00:00:00Z",
            "items": [
                {
                    "id": "gitleaks",
                    "kind": "gitleaks",
                    "status": status,
                    "head_sha": head_sha,
                    "required": true,
                    "summary": "verification fixture"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn pr_comments_merge_preserves_chronological_order() {
    let reviews = vec![json!({
        "kind": "review",
        "id": 2,
        "created_at": "2026-06-06T10:10:00Z",
        "body": "summary",
    })];
    let inline_comments = vec![json!({
        "kind": "inline_comment",
        "id": 1,
        "created_at": "2026-06-06T10:05:00Z",
        "body": "line comment",
    })];

    let merged = merge_pr_comment_entries(reviews, inline_comments);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0]["kind"], "inline_comment");
    assert_eq!(merged[1]["kind"], "review");
}

#[test]
fn pr_comments_flatten_paginated_arrays() {
    let pages = json!([
        [{"id": 1}],
        [{"id": 2}, {"id": 3}]
    ]);

    let flattened = flatten_paginated_array(pages, "comments").expect("flat");

    assert_eq!(flattened.len(), 3);
    assert_eq!(flattened[0]["id"], 1);
    assert_eq!(flattened[2]["id"], 3);
}

#[test]
fn pr_review_digest_filters_gemini_and_builds_candidates() {
    let comments = vec![
        json!({
            "kind": "inline_comment",
            "id": 11,
            "author": "gemini-code-assist",
            "path": "crates/memory-server/src/gh_ops.rs",
            "line": 42,
            "body": "![medium](https://www.gstatic.com/codereviewagent/medium-priority.svg)\nPlease add coverage for paginated comments.",
            "created_at": "2026-06-06T10:05:00Z",
            "url": "https://example.test/comment/11",
        }),
        json!({
            "kind": "inline_comment",
            "id": 12,
            "author": "human-reviewer",
            "path": "README.md",
            "line": 1,
            "body": "Looks good.",
        }),
    ];

    let digest = build_pr_review_digest("o/r", 202, "gemini", &comments);

    assert_eq!(digest["comment_count"], 1);
    assert_eq!(digest["counts"]["tests"], 1);
    assert_eq!(digest["items"][0]["verdict"], "needs_leader_verdict");
    assert_eq!(
        digest["items"][0]["summary"],
        "Please add coverage for paginated comments."
    );
    assert_eq!(digest["memory_candidates"].as_array().unwrap().len(), 1);
    assert_eq!(digest["handbook_candidates"].as_array().unwrap().len(), 1);
    assert!(digest["handbook_candidates"][0]["rule"]
        .as_str()
        .unwrap()
        .contains("regression coverage"));
    let destinations = digest["routing_plan"]["items"][0]["destinations"]
        .as_array()
        .unwrap();
    for expected in [
        "pr_comment",
        "github_issue",
        "feedback_rule",
        "guide",
        "project_wiki",
        "eval",
    ] {
        assert!(
            destinations.iter().any(|value| value == expected),
            "missing {expected} in {destinations:#?}"
        );
    }
    assert_eq!(
        digest["items"][0]["routing"]["primary_destination"],
        json!("github_issue")
    );
    assert_eq!(
        digest["items"][0]["routing"]["promotion_requires"],
        json!("leader_verdict")
    );
    assert_eq!(
        digest["routing_plan"]["status"],
        json!("needs_leader_verdict")
    );
    assert_eq!(
        digest["routing_plan"]["destination_counts"]["feedback_rule"],
        1
    );
}

#[test]
fn pr_review_digest_keeps_style_out_of_handbook_candidates() {
    let comments = vec![json!({
        "kind": "inline_comment",
        "id": 21,
        "author": "gemini-code-assist",
        "path": "src/lib.rs",
        "line": 7,
        "body": "Nit: this naming is a little unclear.",
    })];

    let digest = build_pr_review_digest("o/r", 7, "gemini", &comments);

    assert_eq!(digest["counts"]["style"], 1);
    assert_eq!(digest["memory_candidates"].as_array().unwrap().len(), 1);
    assert_eq!(digest["handbook_candidates"].as_array().unwrap().len(), 0);
    assert_eq!(
        digest["items"][0]["routing"]["primary_destination"],
        json!("pr_comment")
    );
    let destinations = digest["routing_plan"]["items"][0]["destinations"]
        .as_array()
        .unwrap();
    assert!(destinations.iter().any(|value| value == "pr_comment"));
    assert!(destinations.iter().any(|value| value == "eval"));
    assert!(!destinations.iter().any(|value| value == "feedback_rule"));
    assert!(digest["routing_plan"]["destination_counts"]
        .get("feedback_rule")
        .is_none());
}

#[test]
fn pr_review_digest_artifacts_write_json_and_markdown() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_REVIEW_ROOT");
    std::env::set_var("TACHI_REVIEW_ROOT", tmp.path());
    let comments = vec![json!({
        "kind": "inline_comment",
        "id": 31,
        "author": "gemini-code-assist",
        "path": "src/lib.rs",
        "line": 9,
        "body": "Incorrect state handling can cause a regression.",
    })];
    let digest = build_pr_review_digest("owner/repo", 31, "gemini", &comments);

    let artifacts = write_pr_review_digest_artifacts(&digest).unwrap();

    let md_path = PathBuf::from(artifacts["digest_md_path"].as_str().unwrap());
    let json_path = PathBuf::from(artifacts["digest_json_path"].as_str().unwrap());
    assert!(md_path.exists());
    assert!(json_path.exists());
    let markdown = std::fs::read_to_string(md_path).unwrap();
    assert!(markdown.contains("Triage Contract"));
    assert!(markdown.contains("Review Output Routing"));
    assert!(markdown.contains("Primary route:"));
    assert!(markdown.contains("needs_leader_verdict"));
    let leftovers: Vec<_> = std::fs::read_dir(json_path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("digest.json.tmp.") || name.starts_with("digest.md.tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "digest artifact writes should not leave temp files: {leftovers:?}"
    );
    if let Some(v) = original {
        std::env::set_var("TACHI_REVIEW_ROOT", v);
    } else {
        std::env::remove_var("TACHI_REVIEW_ROOT");
    }
}

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
async fn safe_merge_reports_head_sha_mismatch_from_merge_client() {
    let client = MockGhClient::new().with_pr("o/r", ready_pr());
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        None,
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
        policy,
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
        policy,
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
        policy,
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
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
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
        MergeGatePolicy::standard(),
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
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
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
        MergeGatePolicy::permissive(),
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
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
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
        MergeGatePolicy::standard(),
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
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
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
        MergeGatePolicy::strict(),
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
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
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
        MergeGatePolicy::strict(),
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
async fn safe_merge_persists_status_and_event_when_flow_id_supplied() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
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
        MergeGatePolicy::standard(),
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
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
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
        MergeGatePolicy::standard(),
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
        MergeGatePolicy::standard(),
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
        MergeGatePolicy::standard(),
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
        MergeGatePolicy::standard(),
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
        MergeGatePolicy::standard(),
    )
    .await
    .expect_err("should fail");
    assert!(err.contains("pr_view failed"));
    assert!(err.contains("not found") || err.contains("NotFound"));
}

#[test]
fn parse_pr_view_json_happy_path() {
    let v = json!({
        "number": 7,
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "reviewDecision": "APPROVED",
        "isDraft": false,
        "headRefOid": "abc123",
        "closingIssuesReferences": [
            {"number": 99, "url": "https://github.com/o/r/issues/99"}
        ],
    });
    let pr = parse_pr_view_json(&v, vec![]).unwrap();
    assert_eq!(pr.number, 7);
    assert_eq!(pr.state, PrLifecycleState::Open);
    assert_eq!(pr.mergeable, Mergeable::Mergeable);
    assert_eq!(pr.review_decision, Some(ReviewDecision::Approved));
    assert_eq!(pr.checks, ChecksState::None);
    assert!(!pr.is_draft);
    assert_eq!(pr.head_sha, "abc123");
    assert_eq!(
        pr.linked_issue_refs,
        vec!["https://github.com/o/r/issues/99".to_string()]
    );
}

#[test]
fn parse_pr_view_json_aggregates_checks() {
    let v = json!({
        "number": 7,
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "reviewDecision": null,
        "isDraft": false,
        "headRefOid": "abc",
    });
    let runs = vec![
        CheckRun {
            name: "ci".into(),
            conclusion: Some("success".into()),
            status: "completed".into(),
        },
        CheckRun {
            name: "lint".into(),
            conclusion: Some("failure".into()),
            status: "completed".into(),
        },
    ];
    let pr = parse_pr_view_json(&v, runs).unwrap();
    assert_eq!(pr.checks, ChecksState::Failure);
    assert_eq!(pr.review_decision, None);
}

#[test]
fn parse_merge_strategy_defaults_to_squash() {
    assert_eq!(parse_merge_strategy(None).unwrap(), MergeStrategy::Squash);
    assert_eq!(
        parse_merge_strategy(Some("Squash")).unwrap(),
        MergeStrategy::Squash
    );
    assert_eq!(
        parse_merge_strategy(Some("rebase")).unwrap(),
        MergeStrategy::Rebase
    );
    assert!(parse_merge_strategy(Some("foo")).is_err());
}

#[test]
fn parse_merge_gate_policy_defaults_to_standard() {
    assert_eq!(
        parse_merge_gate_policy(None).unwrap().mode,
        MergeGatePolicyMode::Standard
    );
    assert_eq!(
        parse_merge_gate_policy(Some("permissive")).unwrap().mode,
        MergeGatePolicyMode::Permissive
    );
    assert_eq!(
        parse_merge_gate_policy(Some("strict")).unwrap().mode,
        MergeGatePolicyMode::Strict
    );
    assert!(parse_merge_gate_policy(Some("loose")).is_err());
}

#[test]
fn safe_merge_effective_dry_run_requires_confirm() {
    assert!(effective_safe_merge_dry_run(false, None));
    assert!(effective_safe_merge_dry_run(false, Some(false)));
    assert!(effective_safe_merge_dry_run(false, Some(true)));
    assert!(!effective_safe_merge_dry_run(true, None));
    assert!(!effective_safe_merge_dry_run(true, Some(false)));
    assert!(effective_safe_merge_dry_run(true, Some(true)));
}

#[test]
fn safe_merge_classifies_no_checks_reported_as_empty_checks_surface() {
    assert!(is_no_checks_reported(
        "no checks reported on the 'feature' branch"
    ));
    assert!(is_no_checks_reported("No checks reported"));
    assert!(!is_no_checks_reported("API rate limit exceeded"));
}

#[test]
fn classify_gh_error_buckets() {
    assert!(matches!(
        classify_gh_error("HTTP 404 not found"),
        GhError::NotFound(_)
    ));
    assert!(matches!(
        classify_gh_error("API rate limit exceeded"),
        GhError::RateLimited(_)
    ));
    assert!(matches!(
        classify_gh_error("network blip"),
        GhError::Sanitized(_)
    ));
}

#[test]
fn vault_unavailable_errors_allow_fallback() {
    assert!(vault_secret_unavailable("Secret not found: GH_TOKEN"));
    assert!(vault_secret_unavailable("Vault is locked"));
    assert!(vault_secret_unavailable(
        "Vault auto-locked. Call vault_unlock first."
    ));
    assert!(vault_secret_unavailable("Vault not initialized"));
    assert!(!vault_secret_unavailable("Vault decrypt failed"));
}

#[test]
fn env_gh_token_prefers_gh_token_and_falls_back() {
    let _guard = ENV_LOCK.lock().unwrap();
    let old_gh = std::env::var_os("GH_TOKEN");
    let old_github = std::env::var_os("GITHUB_TOKEN");
    std::env::remove_var("GH_TOKEN");
    std::env::remove_var("GITHUB_TOKEN");

    std::env::set_var("GITHUB_TOKEN", "github-token");
    assert_eq!(env_gh_token().as_deref(), Some("github-token"));
    std::env::set_var("GH_TOKEN", "gh-token");
    assert_eq!(env_gh_token().as_deref(), Some("gh-token"));
    std::env::set_var("GH_TOKEN", "   ");
    assert_eq!(env_gh_token().as_deref(), Some("github-token"));

    if let Some(v) = old_gh {
        std::env::set_var("GH_TOKEN", v);
    } else {
        std::env::remove_var("GH_TOKEN");
    }
    if let Some(v) = old_github {
        std::env::set_var("GITHUB_TOKEN", v);
    } else {
        std::env::remove_var("GITHUB_TOKEN");
    }
}

#[test]
fn preserve_gh_env_keeps_auth_proxy_and_platform_env() {
    use std::ffi::OsStr;

    let _guard = ENV_LOCK.lock().unwrap();
    let old_https = std::env::var_os("HTTPS_PROXY");
    let old_cert = std::env::var_os("SSL_CERT_FILE");
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let old_git_ssh_command = std::env::var_os("GIT_SSH_COMMAND");
    let old_ssh_auth_sock = std::env::var_os("SSH_AUTH_SOCK");
    let old_gh = std::env::var_os("GH_TOKEN");
    let old_github = std::env::var_os("GITHUB_TOKEN");

    std::env::set_var("HTTPS_PROXY", "http://proxy.local:8080");
    std::env::set_var("SSL_CERT_FILE", "/tmp/test-ca.pem");
    std::env::set_var("XDG_CONFIG_HOME", "/tmp/test-xdg");
    std::env::set_var("GIT_SSH_COMMAND", "sh -c 'echo should-not-run'");
    std::env::set_var("SSH_AUTH_SOCK", "/tmp/test-ssh-agent.sock");
    std::env::remove_var("GH_TOKEN");
    std::env::set_var("GITHUB_TOKEN", "github-env-token");

    let mut cmd = Command::new("gh");
    cmd.env_clear();
    preserve_gh_env(&mut cmd);

    let envs: Vec<_> = cmd.get_envs().collect();
    let get = |name: &str| {
        envs.iter()
            .find(|(k, _)| *k == OsStr::new(name))
            .and_then(|(_, v)| *v)
            .map(|v| v.to_string_lossy().to_string())
    };

    assert_eq!(
        get("HTTPS_PROXY").as_deref(),
        Some("http://proxy.local:8080")
    );
    assert_eq!(get("SSL_CERT_FILE").as_deref(), Some("/tmp/test-ca.pem"));
    assert_eq!(get("XDG_CONFIG_HOME").as_deref(), Some("/tmp/test-xdg"));
    assert_eq!(
        get("SSH_AUTH_SOCK").as_deref(),
        Some("/tmp/test-ssh-agent.sock")
    );
    assert_eq!(get("GIT_SSH_COMMAND"), None);
    assert_eq!(get("GITHUB_TOKEN").as_deref(), Some("github-env-token"));

    if let Some(v) = old_https {
        std::env::set_var("HTTPS_PROXY", v);
    } else {
        std::env::remove_var("HTTPS_PROXY");
    }
    if let Some(v) = old_cert {
        std::env::set_var("SSL_CERT_FILE", v);
    } else {
        std::env::remove_var("SSL_CERT_FILE");
    }
    if let Some(v) = old_xdg {
        std::env::set_var("XDG_CONFIG_HOME", v);
    } else {
        std::env::remove_var("XDG_CONFIG_HOME");
    }
    if let Some(v) = old_git_ssh_command {
        std::env::set_var("GIT_SSH_COMMAND", v);
    } else {
        std::env::remove_var("GIT_SSH_COMMAND");
    }
    if let Some(v) = old_ssh_auth_sock {
        std::env::set_var("SSH_AUTH_SOCK", v);
    } else {
        std::env::remove_var("SSH_AUTH_SOCK");
    }
    if let Some(v) = old_gh {
        std::env::set_var("GH_TOKEN", v);
    } else {
        std::env::remove_var("GH_TOKEN");
    }
    if let Some(v) = old_github {
        std::env::set_var("GITHUB_TOKEN", v);
    } else {
        std::env::remove_var("GITHUB_TOKEN");
    }
}
