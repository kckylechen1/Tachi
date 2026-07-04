use super::*;

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
    assert!(pr.closing_issue_labels.is_empty());
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
    assert!(
        parse_merge_gate_policy(None)
            .unwrap()
            .block_protected_umbrella_close
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
fn safe_merge_policy_from_params_applies_explicit_umbrella_close_override() {
    assert!(
        merge_gate_policy_from_params(None, false)
            .unwrap()
            .block_protected_umbrella_close
    );
    assert!(
        merge_gate_policy_from_params(Some("permissive"), false)
            .unwrap()
            .block_protected_umbrella_close
    );
    assert!(
        !merge_gate_policy_from_params(None, true)
            .unwrap()
            .block_protected_umbrella_close
    );
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
