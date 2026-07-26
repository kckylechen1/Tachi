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

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[cfg(unix)]
const PINNED_TEST_CREDENTIAL: &str = "test-pinned-credential";
#[cfg(unix)]
const ROTATED_TEST_CREDENTIAL: &str = "test-rotated-credential";

#[cfg(unix)]
fn approver_test_server(temp: &tempfile::TempDir) -> crate::MemoryServer {
    crate::MemoryServer::new(temp.path().join("global.sqlite"), None).expect("test server")
}

#[cfg(unix)]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn write_fake_gh(temp: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let trace = temp.path().join("gh-trace");
    let gh = temp.path().join("gh");
    let trace_path = shell_single_quote(&trace.to_string_lossy());
    let script = format!(
        r#"#!/bin/sh
if [ "$1" != "api" ]; then
    printf 'expected gh api invocation\n' >&2
    exit 2
fi
if [ "${{GH_TOKEN:-}}" = "{PINNED_TEST_CREDENTIAL}" ]; then
    marker=pinned
else
    marker=rotated
fi
printf '%s:%s\n' "$marker" "$2" >> {trace_path}
case "$2" in
    user)
        printf '%s\n' '{{"login":"owner","id":7,"node_id":"U_7","type":"User"}}'
        ;;
    repos/owner/repo)
        printf '%s\n' '{{"full_name":"owner/repo","owner":{{"login":"owner","id":7,"type":"User"}},"permissions":{{"admin":true,"maintain":true,"push":true,"triage":true,"pull":true}}}}'
        ;;
    repos/owner/repo/commits/refs/heads/main)
        printf '%s\n' '{{"sha":"head-sha"}}'
        ;;
    *)
        # Deliberately emits the credential so the live probe's redaction path
        # proves that this value cannot reach an authority denial.
        printf 'unexpected endpoint %s credential=%s\n' "$2" "$GH_TOKEN" >&2
        exit 1
        ;;
esac
"#
    );
    std::fs::write(&gh, script).expect("write fake gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700))
        .expect("make fake gh executable");
    (gh, trace)
}

#[cfg(unix)]
fn pinning_target() -> ApprovalTargetV1 {
    ApprovalTargetV1 {
        repo: "owner/repo".to_string(),
        action: tachi_params::GovernedActionV1::EstablishPrecedent,
        target_ref: "/precedents/credential-pinning".to_string(),
        packet_id: "packet-pinning".to_string(),
        proposal_hash: "proposal-hash".to_string(),
        source_bundle_hash: "source-bundle-hash".to_string(),
        source_snapshot_hashes: vec!["source-snapshot-hash".to_string()],
        repo_revision_pins: vec![tachi_params::RepoRevisionPinV1 {
            repo: "owner/repo".to_string(),
            git_ref: "refs/heads/main".to_string(),
            commit_sha: "head-sha".to_string(),
        }],
    }
}

// This drives the production probe through `resolve_verified_approver`, not a
// helper: the resolver returns a different credential after its first call,
// while the fake `gh` records only whether each spawned command received the
// original value. Before pinning, the second and later API calls would be
// marked `rotated` and the resolver count would exceed one.
#[cfg(unix)]
#[test]
fn live_probe_pins_one_credential_context_and_redacts_it_from_surfaces() {
    let temp = tempfile::tempdir().expect("temporary fake gh directory");
    let (gh_path, trace_path) = write_fake_gh(&temp);
    let resolution_calls = Arc::new(AtomicUsize::new(0));
    let resolver_calls = Arc::clone(&resolution_calls);
    let server = approver_test_server(&temp);
    let probe = GhApproverAuthorityProbe::with_context_resolver(&server, move |_| {
        let token = if resolver_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            PINNED_TEST_CREDENTIAL
        } else {
            ROTATED_TEST_CREDENTIAL
        };
        crate::gh_ops::GhApiContext::for_test(
            gh_path.to_string_lossy().into_owned(),
            token.to_string(),
        )
    });
    let policy = build_policy(PolicyInputs::default()).expect("default owner policy");
    let target = pinning_target();

    let receipt = resolve_verified_approver(
        &probe,
        &policy,
        &target,
        &CallerAssertedContextV1::default(),
        chrono::Utc::now(),
    )
    .expect("the pinned credential authenticates the repository owner");

    assert_eq!(
        resolution_calls.load(Ordering::SeqCst),
        1,
        "one issuance round must resolve the executable/credential context once"
    );
    let trace = std::fs::read_to_string(&trace_path).expect("read fake gh trace");
    assert_eq!(
        trace.lines().collect::<Vec<_>>(),
        vec![
            "pinned:user",
            "pinned:repos/owner/repo",
            "pinned:repos/owner/repo/commits/refs/heads/main",
        ],
        "every production gh request in the issuance round must receive the pinned credential"
    );

    let receipt_debug = format!("{receipt:?}");
    let receipt_json = serde_json::to_string(&receipt).expect("serialize receipt");
    for secret in [PINNED_TEST_CREDENTIAL, ROTATED_TEST_CREDENTIAL] {
        assert!(
            !receipt_debug.contains(secret),
            "receipt Debug leaked a credential"
        );
        assert!(
            !receipt_json.contains(secret),
            "receipt JSON leaked a credential"
        );
    }

    let denial = probe
        .repo_facts("owner/missing")
        .expect_err("the fake gh rejects the unexpected endpoint");
    let denial_debug = format!("{denial:?}");
    let denial_display = denial.to_string();
    for secret in [PINNED_TEST_CREDENTIAL, ROTATED_TEST_CREDENTIAL] {
        assert!(
            !denial_debug.contains(secret),
            "denial Debug leaked a credential"
        );
        assert!(
            !denial_display.contains(secret),
            "denial Display leaked a credential"
        );
    }

    let unpinnable = GhApproverAuthorityProbe::with_context_resolver(&server, |_| {
        Err("credential resolver unavailable".to_string())
    });
    let refusal = unpinnable
        .authenticated_principal()
        .expect_err("a probe without a pinned credential context must refuse");
    assert_eq!(refusal.kind(), "authority_unavailable");
    assert!(
        refusal
            .to_string()
            .contains("could not pin the `gh` credential context"),
        "the refusal must name the broken credential-pinning boundary"
    );
}

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
        let denial = build_policy(inputs).expect_err("a malformed configuration value must refuse");
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
    .expect_err("a day-long approval receipt is refused");
    assert_eq!(denial.kind(), "policy_unusable");

    let skew = build_policy(PolicyInputs {
        future_skew_tolerance_secs: Some("3600".to_string()),
        ..PolicyInputs::default()
    })
    .expect_err("an hour of tolerated skew is refused");
    assert_eq!(skew.kind(), "policy_unusable");
}

#[test]
fn a_policy_authorizing_nobody_is_refused_not_silently_denying() {
    let denial = build_policy(PolicyInputs {
        allow_repository_owner: Some("false".to_string()),
        ..PolicyInputs::default()
    })
    .expect_err("no owner and no team authorizes nobody");
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
