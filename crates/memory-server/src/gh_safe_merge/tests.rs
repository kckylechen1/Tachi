use super::*;

fn open_pr() -> PrState {
    PrState {
        number: 1,
        state: PrLifecycleState::Open,
        mergeable: Mergeable::Mergeable,
        review_decision: Some(ReviewDecision::Approved),
        checks: ChecksState::Success,
        is_draft: false,
        head_sha: "abc123".to_string(),
        head_ref: Some("feat/test-pr".to_string()),
        linked_issue_refs: Vec::new(),
        closing_issue_labels: Vec::new(),
        head_consistent: None,
    }
}

fn closing_issue(reference: &str, labels: &[&str]) -> ClosingIssueLabels {
    ClosingIssueLabels {
        reference: reference.to_string(),
        labels: Some(labels.iter().map(|label| label.to_string()).collect()),
    }
}

// ─── evaluate_merge_gate ─────────────────────────────────────────────

#[test]
fn gate_ready_when_all_green() {
    assert_eq!(evaluate_merge_gate(&open_pr()), MergeDecision::Ready);
}

#[test]
fn gate_permissive_ready_when_no_review_required_and_no_checks() {
    let mut pr = open_pr();
    pr.review_decision = None; // repo has no review policy
    pr.checks = ChecksState::None; // no CI configured
    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::permissive()),
        MergeDecision::Ready
    );
}

#[test]
fn gate_standard_waits_on_missing_checks_and_review_decision() {
    let mut pr = open_pr();
    pr.review_decision = None;
    pr.checks = ChecksState::None;
    match evaluate_merge_gate(&pr) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "checks:none"));
            assert!(waiting_on.iter().any(|r| r == "review:missing_decision"));
        }
        other => panic!("expected pending, got {other:?}"),
    }
}

#[test]
fn gate_policy_modes_for_checks_none() {
    let mut pr = open_pr();
    pr.checks = ChecksState::None;

    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::permissive()),
        MergeDecision::Ready
    );
    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "checks:none"));
            assert!(!waiting_on.iter().any(|r| r == "review:missing_decision"));
        }
        other => panic!("expected pending, got {other:?}"),
    }
    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::strict()) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "checks:none"));
        }
        other => panic!("expected pending, got {other:?}"),
    }
}

#[test]
fn gate_policy_modes_for_skipped_checks() {
    let mut pr = open_pr();
    pr.checks = ChecksState::Skipped;

    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::permissive()),
        MergeDecision::Ready
    );
    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "checks:skipped"));
        }
        other => panic!("expected pending, got {other:?}"),
    }
    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::strict()) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "checks:skipped"));
        }
        other => panic!("expected pending, got {other:?}"),
    }
}

#[test]
fn gate_policy_modes_for_missing_review_decision() {
    let mut pr = open_pr();
    pr.review_decision = None;

    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::permissive()),
        MergeDecision::Ready
    );
    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "review:missing_decision"));
            assert!(!waiting_on.iter().any(|r| r == "checks:none"));
        }
        other => panic!("expected pending, got {other:?}"),
    }
    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::strict()) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "review:missing_decision"));
        }
        other => panic!("expected pending, got {other:?}"),
    }
}

#[test]
fn gate_strict_waits_when_head_consistency_is_unavailable() {
    let pr = open_pr();
    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::strict()) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on
                .iter()
                .any(|r| r == "head:consistency_unavailable"));
        }
        other => panic!("expected pending, got {other:?}"),
    }
}

#[test]
fn gate_strict_ready_when_head_consistency_is_proven() {
    let mut pr = open_pr();
    pr.head_consistent = Some(true);
    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::strict()),
        MergeDecision::Ready
    );
}

#[test]
fn gate_strict_blocks_when_head_consistency_mismatches() {
    let mut pr = open_pr();
    pr.head_consistent = Some(false);
    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::strict()) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons.iter().any(|r| r == "head:consistency_mismatch"));
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_blocked_when_draft() {
    let mut pr = open_pr();
    pr.is_draft = true;
    let dec = evaluate_merge_gate(&pr);
    assert!(matches!(dec, MergeDecision::Blocked { .. }), "got {dec:?}");
    if let MergeDecision::Blocked { reasons } = dec {
        assert!(reasons.contains(&"draft".to_string()));
    }
}

#[test]
fn gate_blocked_when_state_closed_or_merged() {
    let mut pr = open_pr();
    pr.state = PrLifecycleState::Closed;
    match evaluate_merge_gate(&pr) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons.iter().any(|r| r == "state:closed"))
        }
        other => panic!("expected blocked, got {other:?}"),
    }
    pr.state = PrLifecycleState::Merged;
    match evaluate_merge_gate(&pr) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons.iter().any(|r| r == "state:merged"))
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_blocked_when_mergeable_conflicting() {
    let mut pr = open_pr();
    pr.mergeable = Mergeable::Conflicting;
    match evaluate_merge_gate(&pr) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons.iter().any(|r| r == "mergeable:conflicting"))
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_blocked_when_changes_requested() {
    let mut pr = open_pr();
    pr.review_decision = Some(ReviewDecision::ChangesRequested);
    match evaluate_merge_gate(&pr) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons.iter().any(|r| r == "review:changes_requested"))
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_blocked_when_checks_failure() {
    let mut pr = open_pr();
    pr.checks = ChecksState::Failure;
    match evaluate_merge_gate(&pr) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons.iter().any(|r| r == "checks:failure"))
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_pending_when_mergeable_unknown() {
    let mut pr = open_pr();
    pr.mergeable = Mergeable::Unknown;
    match evaluate_merge_gate(&pr) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "mergeable:unknown"))
        }
        other => panic!("expected pending, got {other:?}"),
    }
}

#[test]
fn gate_pending_when_checks_pending() {
    let mut pr = open_pr();
    pr.checks = ChecksState::Pending;
    match evaluate_merge_gate(&pr) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "checks:pending"))
        }
        other => panic!("expected pending, got {other:?}"),
    }
}

#[test]
fn gate_pending_when_review_required() {
    let mut pr = open_pr();
    pr.review_decision = Some(ReviewDecision::ReviewRequired);
    match evaluate_merge_gate(&pr) {
        MergeDecision::Pending { waiting_on } => {
            assert!(waiting_on.iter().any(|r| r == "review:required"))
        }
        other => panic!("expected pending, got {other:?}"),
    }
}

#[test]
fn gate_blocked_short_circuits_pending() {
    // Both a hard fail (changes_requested) AND a soft wait (checks pending)
    // → Blocked wins, but BOTH the hard-fail reasons are surfaced. This
    // matters for the operator UX: a single `github_merge_blocked` event
    // should list every red gate so they can fix them in one pass.
    let mut pr = open_pr();
    pr.review_decision = Some(ReviewDecision::ChangesRequested);
    pr.checks = ChecksState::Pending; // soft, but blocked beats it
    pr.is_draft = true; // another hard gate
    match evaluate_merge_gate(&pr) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons.contains(&"draft".to_string()));
            assert!(reasons.contains(&"review:changes_requested".to_string()));
            // pending checks NOT surfaced under a Blocked decision —
            // the soft state is moot once a hard gate exists.
            assert!(!reasons.iter().any(|r| r == "checks:pending"));
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_decision_to_merge_state_label() {
    assert_eq!(MergeDecision::Ready.merge_state_label(), "ready");
    assert_eq!(
        MergeDecision::Blocked { reasons: vec![] }.merge_state_label(),
        "blocked"
    );
    assert_eq!(
        MergeDecision::Pending { waiting_on: vec![] }.merge_state_label(),
        "pending"
    );
}

#[test]
fn gate_blocks_protected_closing_issue_labels() {
    let mut pr = open_pr();
    pr.closing_issue_labels = vec![closing_issue("#769", &["agent:no-close", "type:umbrella"])];

    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons
                .iter()
                .any(|reason| { reason.contains("#769") && reason.contains("agent:no-close") }));
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_allows_protected_closing_issue_when_policy_override_disables_gate() {
    let mut pr = open_pr();
    pr.closing_issue_labels = vec![closing_issue("#769", &["agent:no-close", "type:umbrella"])];
    let mut policy = MergeGatePolicy::standard();
    policy.block_protected_umbrella_close = false;

    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, policy),
        MergeDecision::Ready
    );
}

#[test]
fn gate_allows_unprotected_closing_issue_labels() {
    let mut pr = open_pr();
    pr.closing_issue_labels = vec![closing_issue("#12", &["type:bug", "status:ready"])];

    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()),
        MergeDecision::Ready
    );
}

#[test]
fn gate_allows_empty_closing_issue_labels() {
    let pr = open_pr();

    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()),
        MergeDecision::Ready
    );
}

// #483 hardening (fail-closed): a KNOWN-empty label set (lookup succeeded, no
// labels) is safe to auto-close; UNKNOWN labels (lookup failed → None) fail
// CLOSED so a transient GitHub error can't silently auto-close a protected issue.
#[test]
fn gate_allows_closing_issue_with_known_empty_labels() {
    let mut pr = open_pr();
    pr.closing_issue_labels = vec![ClosingIssueLabels {
        reference: "#5".to_string(),
        labels: Some(Vec::new()),
    }];

    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()),
        MergeDecision::Ready
    );
}

#[test]
fn gate_blocks_closing_issue_with_unknown_labels() {
    let mut pr = open_pr();
    pr.closing_issue_labels = vec![ClosingIssueLabels {
        reference: "#769".to_string(),
        labels: None,
    }];

    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons
                .iter()
                .any(|reason| reason == "closes_protected_unknown:#769"));
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_allows_unknown_labels_when_override_disables_gate() {
    let mut pr = open_pr();
    pr.closing_issue_labels = vec![ClosingIssueLabels {
        reference: "#769".to_string(),
        labels: None,
    }];
    let mut policy = MergeGatePolicy::standard();
    policy.block_protected_umbrella_close = false;

    assert_eq!(
        evaluate_merge_gate_with_policy(&pr, policy),
        MergeDecision::Ready
    );
}

#[test]
fn gate_reports_draft_and_protected_closing_issue_together() {
    let mut pr = open_pr();
    pr.is_draft = true;
    pr.closing_issue_labels = vec![closing_issue("#769", &["agent:no-close"])];

    match evaluate_merge_gate_with_policy(&pr, MergeGatePolicy::standard()) {
        MergeDecision::Blocked { reasons } => {
            assert!(reasons.iter().any(|reason| reason == "draft"));
            assert!(reasons
                .iter()
                .any(|reason| { reason.contains("#769") && reason.contains("agent:no-close") }));
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn gate_policy_modes_default_to_blocking_protected_umbrella_close() {
    assert!(MergeGatePolicy::permissive().block_protected_umbrella_close);
    assert!(MergeGatePolicy::standard().block_protected_umbrella_close);
    assert!(MergeGatePolicy::strict().block_protected_umbrella_close);
}

// ─── ChecksState::aggregate ──────────────────────────────────────────

fn check(name: &str, status: &str, conclusion: Option<&str>) -> CheckRun {
    CheckRun {
        name: name.to_string(),
        status: status.to_string(),
        conclusion: conclusion.map(str::to_string),
    }
}

#[test]
fn checks_aggregate_empty_is_none() {
    assert_eq!(ChecksState::aggregate(&[]), ChecksState::None);
}

#[test]
fn checks_aggregate_all_success() {
    let runs = vec![
        check("ci", "completed", Some("success")),
        check("lint", "completed", Some("success")),
    ];
    assert_eq!(ChecksState::aggregate(&runs), ChecksState::Success);
}

#[test]
fn checks_aggregate_success_with_neutral_or_partial_skips_is_success() {
    let runs = vec![
        check("ci", "completed", Some("success")),
        check("optional", "completed", Some("neutral")),
        check("conditional", "completed", Some("skipped")),
    ];
    assert_eq!(ChecksState::aggregate(&runs), ChecksState::Success);
}

#[test]
fn checks_aggregate_all_skipped_is_skipped_not_success() {
    let runs = vec![
        check("ci", "completed", Some("skipped")),
        check("lint", "completed", Some("skipped")),
    ];
    assert_eq!(ChecksState::aggregate(&runs), ChecksState::Skipped);
}

#[test]
fn checks_aggregate_any_failure_short_circuits() {
    let runs = vec![
        check("ci", "completed", Some("success")),
        check("lint", "completed", Some("failure")),
        check("test", "in_progress", None), // would otherwise be pending
    ];
    assert_eq!(ChecksState::aggregate(&runs), ChecksState::Failure);
}

#[test]
fn checks_aggregate_in_progress_is_pending() {
    let runs = vec![
        check("ci", "completed", Some("success")),
        check("test", "in_progress", None),
    ];
    assert_eq!(ChecksState::aggregate(&runs), ChecksState::Pending);
}

#[test]
fn checks_aggregate_completed_with_no_conclusion_is_pending() {
    // Defensive: if a check is reported as `completed` but with no
    // `conclusion`, treat it as pending rather than success — the data
    // is incomplete and we should wait one more poll cycle.
    let runs = vec![check("ci", "completed", None)];
    assert_eq!(ChecksState::aggregate(&runs), ChecksState::Pending);
}

#[test]
fn checks_aggregate_cancelled_and_timed_out_are_failure() {
    for conclusion in &["cancelled", "timed_out", "action_required"] {
        let runs = vec![check("ci", "completed", Some(conclusion))];
        assert_eq!(
            ChecksState::aggregate(&runs),
            ChecksState::Failure,
            "conclusion `{conclusion}` should aggregate to Failure"
        );
    }
}

// ─── MockGhClient ────────────────────────────────────────────────────

#[tokio::test]
async fn mock_pr_view_returns_registered_state() {
    let pr = open_pr();
    let mock = MockGhClient::new().with_pr("o/r", pr.clone());
    let got = mock.pr_view("o/r", 1).await.expect("pr_view");
    assert_eq!(got, pr);
}

#[tokio::test]
async fn mock_pr_view_populates_closing_issue_labels_from_registered_issue_labels() {
    let mut pr = open_pr();
    pr.linked_issue_refs = vec!["#769".to_string()];
    let mock = MockGhClient::new().with_pr("o/r", pr).with_issue_labels(
        "o/r",
        769,
        vec!["agent:no-close", "type:umbrella"],
    );

    let got = mock.pr_view("o/r", 1).await.expect("pr_view");

    assert_eq!(got.closing_issue_labels.len(), 1);
    assert_eq!(got.closing_issue_labels[0].reference, "#769");
    assert!(got.closing_issue_labels[0]
        .labels
        .as_ref()
        .expect("labels fetched")
        .iter()
        .any(|label| label == "agent:no-close"));
}

#[tokio::test]
async fn mock_pr_view_unknown_returns_not_found() {
    let mock = MockGhClient::new();
    match mock.pr_view("o/r", 999).await {
        Err(GhError::NotFound(msg)) => assert!(msg.contains("999")),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn mock_pr_merge_records_call_and_returns_deterministic_sha() {
    let pr = open_pr();
    let mock = MockGhClient::new().with_pr("o/r", pr);
    let result = mock
        .pr_merge("o/r", 1, MergeStrategy::Squash, "abc123")
        .await
        .expect("merge");
    assert_eq!(result.pr_number, 1);
    assert_eq!(result.strategy, MergeStrategy::Squash);
    assert_eq!(result.merge_sha, "mock-sha-for-abc123");

    let calls = mock.merge_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (
            "o/r".to_string(),
            1,
            MergeStrategy::Squash,
            "abc123".to_string()
        )
    );
}

#[tokio::test]
async fn mock_pr_merge_records_call_even_on_not_found() {
    // Critical for test-driving the orchestrator: we want to assert that
    // it did NOT call merge when the gate was Blocked. The mock must
    // record the attempt regardless of whether it succeeds, so the
    // orchestrator test can `assert_eq!(merge_calls.len(), 0)`.
    let mock = MockGhClient::new();
    let _ = mock
        .pr_merge("o/r", 42, MergeStrategy::Squash, "abc123")
        .await;
    assert_eq!(mock.merge_calls().len(), 1);
}

#[tokio::test]
async fn mock_pr_merge_rejects_head_sha_mismatch() {
    let pr = open_pr();
    let mock = MockGhClient::new().with_pr("o/r", pr);
    let err = mock
        .pr_merge("o/r", 1, MergeStrategy::Squash, "different")
        .await
        .expect_err("head mismatch should fail");
    assert!(err.to_string().contains("head SHA changed"));
}

#[tokio::test]
async fn mock_issue_create_assigns_monotonic_numbers() {
    let mock = MockGhClient::new();
    let i1 = mock
        .issue_create("o/r", "first", None, &[])
        .await
        .expect("create");
    let i2 = mock
        .issue_create("o/r", "second", Some("body"), &["bug".to_string()])
        .await
        .expect("create");
    assert!(i2.number > i1.number);
    // Created issues are then retrievable.
    let got = mock.issue_view("o/r", i1.number).await.expect("view");
    assert_eq!(got.title, "first");
}

#[tokio::test]
async fn mock_checks_list_defaults_to_empty() {
    let mock = MockGhClient::new();
    let runs = mock.checks_list("o/r", 1).await.expect("checks");
    assert!(runs.is_empty());
}

#[tokio::test]
async fn mock_checks_list_returns_registered_runs() {
    let mock =
        MockGhClient::new().with_checks("o/r", 1, vec![check("ci", "completed", Some("success"))]);
    let runs = mock.checks_list("o/r", 1).await.expect("checks");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].name, "ci");
}
