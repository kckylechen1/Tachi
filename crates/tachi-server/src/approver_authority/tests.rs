//! #1382 server-side tests.
//!
//! These cover the parts of the live probe that are decidable without
//! GitHub: API-path construction (so a caller-supplied repo/org/team/login
//! string cannot steer a request), configuration parsing (so a malformed
//! owner policy refuses instead of silently defaulting), and the
//! fail-closed reading of GitHub's `permissions` object.
//!
//! The authorization decisions themselves are exercised in
//! `tachi_params::approver_authority::tests` against an injected probe; this
//! module deliberately does not try to fake a network.

use super::*;

// ─── API path construction ──────────────────────────────────────────────────

#[test]
fn repo_segments_reject_traversal_and_injection() {
    assert_eq!(
        split_repo("kckylechen1/tachi").expect("a plain owner/name repo is fine"),
        ("kckylechen1".to_string(), "tachi".to_string())
    );

    for hostile in [
        "",
        "tachi",
        "kckylechen1/tachi/extra",
        "../../orgs/evil/teams",
        "kckylechen1/..",
        "kckylechen1/tachi?per_page=1",
        "kckylechen1/tachi#frag",
        "kckylechen1/ tachi",
        "kckylechen1//tachi",
        "/tachi",
        "kckylechen1/",
    ] {
        let denial = split_repo(hostile)
            .err()
            .unwrap_or_else(|| panic!("repo '{hostile}' must be rejected"));
        assert_eq!(denial.kind(), "authority_unavailable");
    }
}

#[test]
fn team_path_segments_reject_injection() {
    validate_path_segment("org", "kckylechen1").expect("plain org");
    validate_path_segment("team slug", "precedent-approvers").expect("plain slug");
    validate_path_segment("login", "kckylechen1").expect("plain login");

    for hostile in ["", "..", ".", "a/b", "a?b", "a#b", "a b", "a%2Fb", "a\nb"] {
        assert!(
            validate_path_segment("login", hostile).is_err(),
            "segment '{hostile}' must be rejected"
        );
    }
}

#[test]
fn git_refs_allow_real_refs_and_reject_traversal() {
    for ok in ["refs/heads/main", "main", "v1.9.0", "refs/tags/v1.9.0"] {
        validate_git_ref(ok).unwrap_or_else(|e| panic!("ref '{ok}' should be accepted: {e}"));
    }
    for hostile in [
        "",
        "/refs/heads/main",
        "refs/heads/main/",
        "refs//heads/main",
        "../../../etc/passwd",
        "refs/heads/main?x=1",
        "refs/heads/ma in",
        "refs/heads/main#frag",
    ] {
        assert!(
            validate_git_ref(hostile).is_err(),
            "ref '{hostile}' must be rejected"
        );
    }
}

// ─── owner policy configuration ─────────────────────────────────────────────

#[test]
fn policy_defaults_are_repository_owner_only_with_an_admin_floor() {
    let policy = build_policy(PolicyInputs::default()).expect("defaults are usable");
    assert_eq!(policy.policy_id, DEFAULT_POLICY_ID);
    assert!(policy.allow_repository_owner);
    assert!(policy.authorized_teams.is_empty());
    assert_eq!(policy.required_permission, RepoPermissionLevelV1::Admin);
    assert_eq!(policy.max_receipt_age_secs, 300);
    assert_eq!(policy.future_skew_tolerance_secs, 60);
}

#[test]
fn authorized_team_entries_require_an_explicit_role() {
    let policy = build_policy(PolicyInputs {
        allow_repository_owner: Some("false".to_string()),
        teams: Some("kckylechen1/approvers:maintainer, kckylechen1/leads:member".to_string()),
        required_permission: Some("maintain".to_string()),
        ..PolicyInputs::default()
    })
    .expect("a well-formed delegation policy");
    assert_eq!(policy.authorized_teams.len(), 2);
    assert_eq!(policy.authorized_teams[0].org, "kckylechen1");
    assert_eq!(policy.authorized_teams[0].team_slug, "approvers");
    assert_eq!(
        policy.authorized_teams[0].required_role,
        TeamRoleV1::Maintainer
    );
    assert_eq!(policy.authorized_teams[1].required_role, TeamRoleV1::Member);

    // A role-less entry is a refusal, not an implied `member`.
    for hostile in [
        "kckylechen1/approvers",
        "kckylechen1/approvers:owner",
        "approvers:member",
        "kckylechen1/:member",
        "/approvers:member",
    ] {
        let denial = build_policy(PolicyInputs {
            teams: Some(hostile.to_string()),
            ..PolicyInputs::default()
        })
        .err()
        .unwrap_or_else(|| panic!("team entry '{hostile}' must be rejected"));
        assert_eq!(denial.kind(), "policy_unusable");
    }
}

#[test]
fn a_present_but_malformed_value_refuses_instead_of_defaulting() {
    let cases = [
        PolicyInputs {
            allow_repository_owner: Some("maybe".to_string()),
            ..PolicyInputs::default()
        },
        PolicyInputs {
            required_permission: Some("read".to_string()),
            ..PolicyInputs::default()
        },
        PolicyInputs {
            max_receipt_age_secs: Some("five minutes".to_string()),
            ..PolicyInputs::default()
        },
        PolicyInputs {
            future_skew_tolerance_secs: Some("1.5".to_string()),
            ..PolicyInputs::default()
        },
    ];
    for inputs in cases {
        let denial = build_policy(inputs)
            .err()
            .expect("a malformed configuration value must refuse");
        assert_eq!(denial.kind(), "policy_unusable");
    }
}

#[test]
fn read_and_triage_are_not_expressible_as_an_approval_floor() {
    for level in ["pull", "read", "triage", "none"] {
        let denial = build_policy(PolicyInputs {
            required_permission: Some(level.to_string()),
            ..PolicyInputs::default()
        })
        .err()
        .unwrap_or_else(|| panic!("'{level}' must not be an approval floor"));
        assert_eq!(denial.kind(), "policy_unusable");
    }
}

#[test]
fn configuration_cannot_exceed_the_receipt_lifetime_ceilings() {
    let denial = build_policy(PolicyInputs {
        max_receipt_age_secs: Some("86400".to_string()),
        ..PolicyInputs::default()
    })
    .err()
    .expect("a day-long approval receipt is refused");
    assert_eq!(denial.kind(), "policy_unusable");

    let skew = build_policy(PolicyInputs {
        future_skew_tolerance_secs: Some("3600".to_string()),
        ..PolicyInputs::default()
    })
    .err()
    .expect("an hour of tolerated skew is refused");
    assert_eq!(skew.kind(), "policy_unusable");
}

#[test]
fn a_policy_authorizing_nobody_is_refused_not_silently_denying() {
    let denial = build_policy(PolicyInputs {
        allow_repository_owner: Some("false".to_string()),
        ..PolicyInputs::default()
    })
    .err()
    .expect("no owner and no team authorizes nobody");
    assert_eq!(denial.kind(), "policy_unusable");
}

#[test]
fn changing_configuration_changes_the_authorization_revision() {
    let base = build_policy(PolicyInputs::default()).expect("defaults");
    let widened = build_policy(PolicyInputs {
        teams: Some("kckylechen1/approvers:member".to_string()),
        ..PolicyInputs::default()
    })
    .expect("owner plus one team");

    assert_ne!(
        base.authorization_revision().expect("revision"),
        widened.authorization_revision().expect("revision"),
        "adding an authorized team must invalidate receipts issued before it"
    );
}

// ─── reading GitHub's permissions object ────────────────────────────────────

#[test]
fn a_missing_permission_bit_is_not_a_granted_bit() {
    let permissions = serde_json::json!({ "pull": true, "push": false });
    assert!(permission_bit(&permissions, "pull"));
    assert!(!permission_bit(&permissions, "push"));
    // Absent entirely.
    assert!(!permission_bit(&permissions, "admin"));
    assert!(!permission_bit(&permissions, "maintain"));
    // Present but not a boolean — a string "true" is not a grant.
    let stringly = serde_json::json!({ "admin": "true" });
    assert!(!permission_bit(&stringly, "admin"));
    let numeric = serde_json::json!({ "admin": 1 });
    assert!(!permission_bit(&numeric, "admin"));
}

#[test]
fn probe_failures_are_named_availability_refusals() {
    let denial = unavailable("repo_facts", "connection reset by peer");
    assert_eq!(denial.kind(), "authority_unavailable");
    assert!(denial.is_unavailable());
    let message = denial.to_string();
    assert!(message.contains("refused"), "{message}");
    assert!(message.contains("repo_facts"), "{message}");
    assert!(message.contains("connection reset by peer"), "{message}");
}
