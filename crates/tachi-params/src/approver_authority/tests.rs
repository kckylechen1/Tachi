//! #1382 discrimination suite.
//!
//! Every test here names an attack or a revocation event and proves the gate
//! refuses it. The fixtures are deliberately hostile: the caller-asserted
//! context always claims to be the repository owner with a full agent
//! identity, work claim, execution receipt, and delegation capability, so a
//! passing authorization test also proves those fields bought nothing.

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;

// ─── fixture constants ──────────────────────────────────────────────────────

const REPO: &str = "kckylechen1/tachi";
const OWNER_LOGIN: &str = "kckylechen1";
const OWNER_ID: u64 = 4242;
const OWNER_NODE: &str = "MDQ6VXNlcjQyNDI=";
const HEAD_SHA: &str = "794d3764540b5a5b3fe1a9aaac7d7b2c90a5f5d8";
const OTHER_SHA: &str = "0000000000000000000000000000000000000000";
/// A fixture credential. No test may find this substring in a receipt, a
/// denial message, or a fingerprint.
const TOKEN: &str = "ghp_fixture_secret_value_do_not_leak";
const NOW: &str = "2026-07-25T12:00:00Z";

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixture timestamp parses")
        .with_timezone(&Utc)
}

fn now() -> DateTime<Utc> {
    at(NOW)
}

// ─── fake probe ─────────────────────────────────────────────────────────────

/// Test double for [`ApproverAuthorityProbe`].
///
/// Unset team memberships answer `NotMember` (a decided negative, the common
/// fixture case). Unset repository revisions answer `AuthorityUnavailable`,
/// so a test that forgets to pin a revision fails loudly instead of
/// accidentally proving something about a path it never exercised.
#[derive(Clone)]
struct FakeProbe {
    principal: Result<VerifiedPrincipalV1, AuthorityDenialV1>,
    repo_facts: Result<RepoFactsV1, AuthorityDenialV1>,
    memberships: BTreeMap<String, Result<TeamMembershipProbeV1, AuthorityDenialV1>>,
    revisions: BTreeMap<String, Result<RepoRevisionV1, AuthorityDenialV1>>,
    calls: RefCell<Vec<String>>,
}

impl FakeProbe {
    fn new(principal: VerifiedPrincipalV1, repo_facts: RepoFactsV1) -> Self {
        let mut revisions = BTreeMap::new();
        revisions.insert(
            format!("{REPO}@refs/heads/main"),
            Ok(repo_revision(REPO, "refs/heads/main", HEAD_SHA)),
        );
        Self {
            principal: Ok(principal),
            repo_facts: Ok(repo_facts),
            memberships: BTreeMap::new(),
            revisions,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn with_membership(
        mut self,
        org: &str,
        team_slug: &str,
        answer: Result<TeamMembershipProbeV1, AuthorityDenialV1>,
    ) -> Self {
        self.memberships.insert(format!("{org}/{team_slug}"), answer);
        self
    }

    fn with_revision(
        mut self,
        repo: &str,
        git_ref: &str,
        answer: Result<RepoRevisionV1, AuthorityDenialV1>,
    ) -> Self {
        self.revisions.insert(format!("{repo}@{git_ref}"), answer);
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl ApproverAuthorityProbe for FakeProbe {
    fn authenticated_principal(&self) -> Result<VerifiedPrincipalV1, AuthorityDenialV1> {
        self.calls.borrow_mut().push("user".to_string());
        self.principal.clone()
    }

    fn repo_facts(&self, repo: &str) -> Result<RepoFactsV1, AuthorityDenialV1> {
        self.calls.borrow_mut().push(format!("repo:{repo}"));
        self.repo_facts.clone()
    }

    fn team_membership(
        &self,
        org: &str,
        team_slug: &str,
        login: &str,
    ) -> Result<TeamMembershipProbeV1, AuthorityDenialV1> {
        self.calls
            .borrow_mut()
            .push(format!("team:{org}/{team_slug}#{login}"));
        match self.memberships.get(&format!("{org}/{team_slug}")) {
            Some(answer) => answer.clone(),
            None => Ok(TeamMembershipProbeV1::NotMember {
                org: org.to_string(),
                team_slug: team_slug.to_string(),
                login: login.to_string(),
            }),
        }
    }

    fn repo_revision(
        &self,
        repo: &str,
        git_ref: &str,
    ) -> Result<RepoRevisionV1, AuthorityDenialV1> {
        self.calls
            .borrow_mut()
            .push(format!("rev:{repo}@{git_ref}"));
        match self.revisions.get(&format!("{repo}@{git_ref}")) {
            Some(answer) => answer.clone(),
            None => Err(AuthorityDenialV1::AuthorityUnavailable {
                probe: "repo_revision".to_string(),
                detail: format!("fixture pinned no revision for {repo}@{git_ref}"),
            }),
        }
    }
}

// ─── fixture builders ───────────────────────────────────────────────────────

fn credential(token: &str) -> CredentialContextV1 {
    CredentialContextV1 {
        source: "vault:GH_TOKEN".to_string(),
        credential_fingerprint: credential_fingerprint(token),
    }
}

fn principal(login: &str, user_id: u64, node_id: &str) -> VerifiedPrincipalV1 {
    VerifiedPrincipalV1 {
        login: login.to_string(),
        user_id,
        node_id: node_id.to_string(),
        account_type: "User".to_string(),
        credential_context: credential(TOKEN),
        verified_at: NOW.to_string(),
    }
}

fn owner_principal() -> VerifiedPrincipalV1 {
    principal(OWNER_LOGIN, OWNER_ID, OWNER_NODE)
}

fn full_permissions() -> RepoPermissionV1 {
    RepoPermissionV1 {
        admin: true,
        maintain: true,
        push: true,
        triage: true,
        pull: true,
    }
}

fn push_permissions() -> RepoPermissionV1 {
    RepoPermissionV1 {
        admin: false,
        maintain: false,
        push: true,
        triage: true,
        pull: true,
    }
}

fn read_permissions() -> RepoPermissionV1 {
    RepoPermissionV1 {
        pull: true,
        ..RepoPermissionV1::default()
    }
}

fn repo_facts(
    owner_login: &str,
    owner_id: u64,
    owner_type: &str,
    permissions: RepoPermissionV1,
) -> RepoFactsV1 {
    RepoFactsV1 {
        full_name: REPO.to_string(),
        owner_login: owner_login.to_string(),
        owner_id,
        owner_type: owner_type.to_string(),
        permissions,
        observed_at: NOW.to_string(),
    }
}

fn owner_repo_facts() -> RepoFactsV1 {
    repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions())
}

fn repo_revision(repo: &str, git_ref: &str, sha: &str) -> RepoRevisionV1 {
    RepoRevisionV1 {
        repo: repo.to_string(),
        git_ref: git_ref.to_string(),
        commit_sha: sha.to_string(),
        verified_at: NOW.to_string(),
    }
}

fn owner_only_policy() -> ApproverAuthorizationPolicyV1 {
    ApproverAuthorizationPolicyV1::repository_owner_only("tachi/precedent-gate")
}

fn team_policy(
    org: &str,
    team_slug: &str,
    required_role: TeamRoleV1,
) -> ApproverAuthorizationPolicyV1 {
    ApproverAuthorizationPolicyV1 {
        policy_id: "tachi/precedent-gate".to_string(),
        allow_repository_owner: false,
        authorized_teams: vec![AuthorizedTeamV1 {
            org: org.to_string(),
            team_slug: team_slug.to_string(),
            required_role,
        }],
        required_permission: RepoPermissionLevelV1::Maintain,
        max_receipt_age_secs: 300,
        future_skew_tolerance_secs: 60,
    }
}

fn target() -> ApprovalTargetV1 {
    ApprovalTargetV1 {
        repo: REPO.to_string(),
        action: GovernedActionV1::EstablishPrecedent,
        target_ref: "/precedents/tachi/9f2c1a77bd4e0033".to_string(),
        packet_id: "packet-1077-a".to_string(),
        proposal_hash: "proposal-hash-aaa".to_string(),
        source_bundle_hash: "source-bundle-hash-bbb".to_string(),
        source_snapshot_hashes: vec!["issue-snapshot-ccc".to_string()],
        repo_revision_pins: vec![RepoRevisionPinV1 {
            repo: REPO.to_string(),
            git_ref: "refs/heads/main".to_string(),
            commit_sha: HEAD_SHA.to_string(),
        }],
    }
}

/// Every forgeable field, all of them lying, all of them claiming maximal
/// authority. Used as the default caller context in the authorization tests
/// so no test can pass *because* of one of these.
fn forged_context() -> CallerAssertedContextV1 {
    CallerAssertedContextV1 {
        actor: Some(OWNER_LOGIN.to_string()),
        adjudicator: Some("owner".to_string()),
        agent_identity: Some("agent-identity:owner-equivalent".to_string()),
        work_claim: Some("claim-owner-approval".to_string()),
        model: Some("opus".to_string()),
        seat: Some("leader".to_string()),
        execution_receipt: Some("exec-receipt-approved".to_string()),
        delegation_capability: Some("precedent.establish".to_string()),
    }
}

fn other_forged_context() -> CallerAssertedContextV1 {
    CallerAssertedContextV1 {
        actor: Some("someone-else-entirely".to_string()),
        model: Some("glm-5.2".to_string()),
        ..CallerAssertedContextV1::default()
    }
}

fn current(target: ApprovalTargetV1) -> CurrentApprovalContextV1 {
    CurrentApprovalContextV1 { target }
}

fn issue_owner_receipt() -> (FakeProbe, ApproverAuthorizationPolicyV1, ApprovalReceiptV1) {
    let probe = FakeProbe::new(owner_principal(), owner_repo_facts());
    let policy = owner_only_policy();
    let receipt = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect("repository owner is authorized");
    (probe, policy, receipt)
}

// ─── forged caller identity cannot cross the gate ───────────────────────────

#[test]
fn forged_caller_identity_fields_cannot_cross_the_gate() {
    // The credential authenticates a nobody; the caller claims to be the
    // owner and carries every agent-side authority artifact that exists.
    // Note the fixture is deliberately generous: the drive-by principal even
    // holds live admin permission. Permission is necessary, never sufficient.
    let probe = FakeProbe::new(
        principal("drive-by", 999_111, "MDQ6VXNlcjk5OTExMQ=="),
        owner_repo_facts(),
    );
    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("a forged caller context must not authorize");

    match denial {
        AuthorityDenialV1::NotAuthorized { login, repo, .. } => {
            // The denial names the *verified* principal, never the claimed one.
            assert_eq!(login, "drive-by");
            assert_eq!(repo, REPO);
        }
        other => panic!("expected NotAuthorized, got {other:?}"),
    }
}

#[test]
fn caller_asserted_context_cannot_change_an_authorization_outcome() {
    let probe = FakeProbe::new(owner_principal(), owner_repo_facts());
    let policy = owner_only_policy();

    let a = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect("owner authorized");
    let b = resolve_verified_approver(&probe, &policy, &target(), &other_forged_context(), now())
        .expect("owner authorized");

    // Nothing authority-bearing differs...
    assert_eq!(a.principal, b.principal);
    assert_eq!(a.authority, b.authority);
    assert_eq!(a.observed_permission, b.observed_permission);
    assert_eq!(a.required_permission, b.required_permission);
    assert_eq!(a.authorization_revision, b.authorization_revision);
    assert_eq!(a.target, b.target);
    // ...only the descriptive record, and it is covered by the hash so it
    // cannot be rewritten after issuance.
    assert_ne!(a.caller_asserted, b.caller_asserted);
    assert_ne!(a.receipt_hash, b.receipt_hash);
}

#[test]
fn delegation_capability_string_cannot_substitute_for_team_membership() {
    let probe = FakeProbe::new(
        principal("contractor", 555, "MDQ6VXNlcjU1NQ=="),
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions()),
    );
    let policy = team_policy("kckylechen1", "precedent-approvers", TeamRoleV1::Member);

    let denial = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect_err("a delegation-capability string is not team membership");
    assert_eq!(denial.kind(), "not_authorized");
}

#[test]
fn a_bot_or_app_credential_is_not_a_human_approver() {
    let mut bot = owner_principal();
    bot.account_type = "Bot".to_string();
    let probe = FakeProbe::new(bot, owner_repo_facts());

    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("a non-human account cannot approve");
    assert_eq!(denial.kind(), "principal_not_human");
}

// ─── repository ownership ───────────────────────────────────────────────────

#[test]
fn repository_owner_matched_by_numeric_id_is_authorized() {
    let (_probe, _policy, receipt) = issue_owner_receipt();
    assert_eq!(receipt.decision, ApprovalDecisionV1::Approved);
    assert_eq!(receipt.receipt_version, ApprovalReceiptV1::VERSION);
    assert_eq!(receipt.principal.user_id, OWNER_ID);
    match &receipt.authority {
        AuthorityBasisV1::RepositoryOwner { owner_id, .. } => assert_eq!(*owner_id, OWNER_ID),
        other => panic!("expected RepositoryOwner basis, got {other:?}"),
    }
    assert_eq!(receipt.verified_repo_revisions.len(), 1);
    assert_eq!(receipt.verified_repo_revisions[0].commit_sha, HEAD_SHA);
    receipt.verify_integrity().expect("fresh receipt is intact");
}

#[test]
fn matching_login_with_a_different_account_id_is_not_the_owner() {
    // A renamed or re-registered account that happens to hold the owner's
    // old login. The login matches; the stable numeric identity does not.
    let impostor = principal(OWNER_LOGIN, 777_777, "MDQ6VXNlcjc3Nzc3Nw==");
    let probe = FakeProbe::new(impostor, owner_repo_facts());

    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("login collision must not confer ownership");
    assert_eq!(denial.kind(), "not_authorized");
}

#[test]
fn organization_owned_repository_confers_no_owner_authority() {
    // Even when the org's numeric id coincides with the principal's, an
    // Organization owner is not a human owner.
    let probe = FakeProbe::new(
        owner_principal(),
        repo_facts(OWNER_LOGIN, OWNER_ID, "Organization", full_permissions()),
    );
    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("organization ownership is not personal ownership");
    assert_eq!(denial.kind(), "not_authorized");
}

#[test]
fn owner_without_the_required_live_permission_is_refused() {
    let probe = FakeProbe::new(
        owner_principal(),
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", read_permissions()),
    );
    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("read access cannot approve a governed mutation");
    assert_eq!(denial.kind(), "insufficient_repo_permission");
}

#[test]
fn repository_redirect_to_another_full_name_is_refused() {
    let mut facts = owner_repo_facts();
    facts.full_name = "kckylechen1/some-other-repo".to_string();
    let probe = FakeProbe::new(owner_principal(), facts);

    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("authority proven on another repository is not authority here");
    assert_eq!(denial.kind(), "repository_mismatch");
}

// ─── delegated org-team authority ───────────────────────────────────────────

fn active_membership(org: &str, team_slug: &str, login: &str, role: &str) -> TeamMembershipProbeV1 {
    TeamMembershipProbeV1::Member(TeamMembershipV1 {
        org: org.to_string(),
        team_slug: team_slug.to_string(),
        login: login.to_string(),
        state: "active".to_string(),
        role: role.to_string(),
        observed_at: NOW.to_string(),
    })
}

#[test]
fn active_team_member_with_the_required_role_is_authorized() {
    let delegate = principal("delegate", 8_001, "MDQ6VXNlcjgwMDE=");
    let probe = FakeProbe::new(
        delegate,
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions()),
    )
    .with_membership(
        "kckylechen1",
        "precedent-approvers",
        Ok(active_membership(
            "kckylechen1",
            "precedent-approvers",
            "delegate",
            "member",
        )),
    );
    let policy = team_policy("kckylechen1", "precedent-approvers", TeamRoleV1::Member);

    let receipt = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect("active team member is authorized");
    match &receipt.authority {
        AuthorityBasisV1::AuthorizedOrgTeam {
            org,
            team_slug,
            membership_state,
            membership_role,
            ..
        } => {
            assert_eq!(org, "kckylechen1");
            assert_eq!(team_slug, "precedent-approvers");
            assert_eq!(membership_state, "active");
            assert_eq!(membership_role, "member");
        }
        other => panic!("expected AuthorizedOrgTeam basis, got {other:?}"),
    }
}

#[test]
fn pending_team_invitation_is_not_authority() {
    let probe = FakeProbe::new(
        principal("invitee", 8_002, "MDQ6VXNlcjgwMDI="),
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions()),
    )
    .with_membership(
        "kckylechen1",
        "precedent-approvers",
        Ok(TeamMembershipProbeV1::Member(TeamMembershipV1 {
            org: "kckylechen1".to_string(),
            team_slug: "precedent-approvers".to_string(),
            login: "invitee".to_string(),
            state: "pending".to_string(),
            role: "member".to_string(),
            observed_at: NOW.to_string(),
        })),
    );
    let policy = team_policy("kckylechen1", "precedent-approvers", TeamRoleV1::Member);

    let denial = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect_err("a pending invitation is not membership");
    assert_eq!(denial.kind(), "team_membership_not_usable");
}

#[test]
fn team_role_below_the_policy_floor_is_refused() {
    let probe = FakeProbe::new(
        principal("delegate", 8_003, "MDQ6VXNlcjgwMDM="),
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions()),
    )
    .with_membership(
        "kckylechen1",
        "precedent-approvers",
        Ok(active_membership(
            "kckylechen1",
            "precedent-approvers",
            "delegate",
            "member",
        )),
    );
    let policy = team_policy("kckylechen1", "precedent-approvers", TeamRoleV1::Maintainer);

    let denial = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect_err("member role does not satisfy a maintainer floor");
    assert_eq!(denial.kind(), "team_membership_not_usable");
}

#[test]
fn unrecognized_team_role_string_satisfies_nothing() {
    let probe = FakeProbe::new(
        principal("delegate", 8_004, "MDQ6VXNlcjgwMDQ="),
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions()),
    )
    .with_membership(
        "kckylechen1",
        "precedent-approvers",
        Ok(active_membership(
            "kckylechen1",
            "precedent-approvers",
            "delegate",
            "owner-equivalent",
        )),
    );
    let policy = team_policy("kckylechen1", "precedent-approvers", TeamRoleV1::Member);

    let denial = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect_err("an unknown role string must satisfy no floor");
    assert_eq!(denial.kind(), "team_membership_not_usable");
}

#[test]
fn team_member_without_the_required_live_repo_permission_is_refused() {
    let probe = FakeProbe::new(
        principal("delegate", 8_005, "MDQ6VXNlcjgwMDU="),
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", push_permissions()),
    )
    .with_membership(
        "kckylechen1",
        "precedent-approvers",
        Ok(active_membership(
            "kckylechen1",
            "precedent-approvers",
            "delegate",
            "maintainer",
        )),
    );
    // Policy floor is Maintain; the live permission is only push.
    let policy = team_policy("kckylechen1", "precedent-approvers", TeamRoleV1::Member);

    let denial = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect_err("team membership does not substitute for live repo permission");
    assert_eq!(denial.kind(), "insufficient_repo_permission");
}

// ─── GitHub unavailability is a loud refusal, never a pass ──────────────────

#[test]
fn unavailable_principal_probe_refuses_loudly() {
    let mut probe = FakeProbe::new(owner_principal(), owner_repo_facts());
    probe.principal = Err(AuthorityDenialV1::AuthorityUnavailable {
        probe: "user".to_string(),
        detail: "request timed out after 10s".to_string(),
    });

    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("an unreachable GitHub must refuse");
    assert!(denial.is_unavailable());
    let message = denial.to_string();
    assert!(message.contains("refused"), "denial must be loud: {message}");
    assert!(
        message.contains("never treated as approval"),
        "denial must say why: {message}"
    );
}

#[test]
fn unavailable_repo_probe_refuses_loudly() {
    let mut probe = FakeProbe::new(owner_principal(), owner_repo_facts());
    probe.repo_facts = Err(AuthorityDenialV1::AuthorityUnavailable {
        probe: "repo_facts".to_string(),
        detail: "HTTP 403 rate limit exceeded".to_string(),
    });

    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("a rate-limited GitHub must refuse");
    assert!(denial.is_unavailable());
}

#[test]
fn unavailable_team_probe_aborts_and_does_not_fall_through_to_later_teams() {
    let delegate = principal("delegate", 8_006, "MDQ6VXNlcjgwMDY=");
    let probe = FakeProbe::new(
        delegate,
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions()),
    )
    .with_membership(
        "kckylechen1",
        "flaky-team",
        Err(AuthorityDenialV1::AuthorityUnavailable {
            probe: "team_membership".to_string(),
            detail: "connection reset".to_string(),
        }),
    )
    .with_membership(
        "kckylechen1",
        "good-team",
        Ok(active_membership(
            "kckylechen1",
            "good-team",
            "delegate",
            "maintainer",
        )),
    );

    let mut policy = team_policy("kckylechen1", "flaky-team", TeamRoleV1::Member);
    policy.authorized_teams.push(AuthorizedTeamV1 {
        org: "kckylechen1".to_string(),
        team_slug: "good-team".to_string(),
        required_role: TeamRoleV1::Member,
    });

    let denial = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect_err("an unread team probe must not be absorbed into a later answer");
    assert!(denial.is_unavailable());
    let calls = probe.calls();
    assert!(
        calls.iter().any(|c| c.contains("flaky-team")),
        "flaky team must have been probed: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains("good-team")),
        "evaluation must stop at the unavailable probe: {calls:?}"
    );
}

#[test]
fn unavailable_github_at_the_choke_point_mutates_nothing() {
    let (_probe, policy, receipt) = issue_owner_receipt();
    let mut broken = FakeProbe::new(owner_principal(), owner_repo_facts());
    broken.principal = Err(AuthorityDenialV1::AuthorityUnavailable {
        probe: "user".to_string(),
        detail: "dns failure".to_string(),
    });

    let denial = revalidate_approval(&broken, &policy, &receipt, &current(target()), now())
        .expect_err("apply must refuse when authority cannot be revalidated");
    assert!(denial.is_unavailable());
}

// ─── policy hygiene ─────────────────────────────────────────────────────────

#[test]
fn a_policy_that_authorizes_nobody_is_unusable_not_silently_denying() {
    let policy = ApproverAuthorizationPolicyV1 {
        allow_repository_owner: false,
        authorized_teams: Vec::new(),
        ..owner_only_policy()
    };
    let denial = policy.validate().expect_err("policy authorizes nobody");
    assert_eq!(denial.kind(), "policy_unusable");
}

#[test]
fn a_policy_cannot_buy_more_receipt_lifetime_than_the_hard_ceiling() {
    let policy = ApproverAuthorizationPolicyV1 {
        max_receipt_age_secs: MAX_RECEIPT_AGE_CEILING_SECS + 1,
        ..owner_only_policy()
    };
    assert_eq!(
        policy
            .validate()
            .expect_err("ceiling is not negotiable")
            .kind(),
        "policy_unusable"
    );

    let skewed = ApproverAuthorizationPolicyV1 {
        future_skew_tolerance_secs: MAX_FUTURE_SKEW_CEILING_SECS + 1,
        ..owner_only_policy()
    };
    assert_eq!(
        skewed
            .validate()
            .expect_err("skew ceiling is not negotiable")
            .kind(),
        "policy_unusable"
    );
}

#[test]
fn changing_the_authorized_policy_invalidates_receipts_issued_under_it() {
    let (probe, policy, receipt) = issue_owner_receipt();

    let mut widened = policy.clone();
    widened.authorized_teams.push(AuthorizedTeamV1 {
        org: "kckylechen1".to_string(),
        team_slug: "newly-added".to_string(),
        required_role: TeamRoleV1::Member,
    });
    assert_ne!(
        policy.authorization_revision().expect("revision"),
        widened.authorization_revision().expect("revision")
    );

    let denial = revalidate_approval(&probe, &widened, &receipt, &current(target()), now())
        .expect_err("a receipt issued under other rules cannot apply");
    assert_eq!(denial.kind(), "authorization_revision_changed");
}

// ─── revision pinning ───────────────────────────────────────────────────────

#[test]
fn an_unpinned_repository_axis_refuses_issuance() {
    let probe = FakeProbe::new(owner_principal(), owner_repo_facts());
    let mut unpinned = target();
    unpinned.repo_revision_pins.clear();

    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &unpinned,
        &forged_context(),
        now(),
    )
    .expect_err("an unpinned repository axis is unverified, not unchanged");
    match denial {
        AuthorityDenialV1::RevisionDrift { reasons } => {
            assert!(matches!(
                reasons.as_slice(),
                [RevisionDriftV1::RepoRevisionUnpinned { .. }]
            ));
        }
        other => panic!("expected RevisionDrift, got {other:?}"),
    }
}

#[test]
fn a_caller_claimed_commit_that_github_does_not_confirm_refuses_issuance() {
    let probe = FakeProbe::new(owner_principal(), owner_repo_facts()).with_revision(
        REPO,
        "refs/heads/main",
        Ok(repo_revision(REPO, "refs/heads/main", OTHER_SHA)),
    );

    let denial = resolve_verified_approver(
        &probe,
        &owner_only_policy(),
        &target(),
        &forged_context(),
        now(),
    )
    .expect_err("the receipt binds server-observed revisions, not caller claims");
    assert_eq!(denial.kind(), "revision_drift");
}

#[test]
fn repository_revision_drift_between_issuance_and_apply_refuses() {
    let (_probe, policy, receipt) = issue_owner_receipt();
    let moved = FakeProbe::new(owner_principal(), owner_repo_facts()).with_revision(
        REPO,
        "refs/heads/main",
        Ok(repo_revision(REPO, "refs/heads/main", OTHER_SHA)),
    );

    let denial = revalidate_approval(&moved, &policy, &receipt, &current(target()), now())
        .expect_err("main moved since approval");
    assert_eq!(denial.kind(), "revision_drift");
}

#[test]
fn proposal_hash_drift_refuses_at_apply() {
    let (probe, policy, receipt) = issue_owner_receipt();
    let mut drifted = target();
    drifted.proposal_hash = "proposal-hash-rewritten".to_string();

    let denial = revalidate_approval(&probe, &policy, &receipt, &current(drifted), now())
        .expect_err("an edited proposal cannot replay an old approval");
    match denial {
        AuthorityDenialV1::RevisionDrift { reasons } => assert!(matches!(
            reasons.as_slice(),
            [RevisionDriftV1::ProposalHashChanged { .. }]
        )),
        other => panic!("expected RevisionDrift, got {other:?}"),
    }
}

#[test]
fn source_bundle_and_snapshot_drift_refuse_at_apply() {
    let (probe, policy, receipt) = issue_owner_receipt();

    let mut bundle_drift = target();
    bundle_drift.source_bundle_hash = "source-bundle-hash-rewritten".to_string();
    assert_eq!(
        revalidate_approval(&probe, &policy, &receipt, &current(bundle_drift), now())
            .expect_err("source bundle changed")
            .kind(),
        "revision_drift"
    );

    let mut snapshot_drift = target();
    snapshot_drift.source_snapshot_hashes = vec!["issue-snapshot-rewritten".to_string()];
    assert_eq!(
        revalidate_approval(&probe, &policy, &receipt, &current(snapshot_drift), now())
            .expect_err("source snapshots changed")
            .kind(),
        "revision_drift"
    );
}

#[test]
fn an_approval_cannot_be_replayed_onto_another_action_or_target() {
    let (probe, policy, receipt) = issue_owner_receipt();

    let mut other_action = target();
    other_action.action = GovernedActionV1::OverturnPrecedent;
    let denial = revalidate_approval(&probe, &policy, &receipt, &current(other_action), now())
        .expect_err("an establish approval cannot authorize an overturn");
    assert_eq!(denial.kind(), "target_mismatch");
    assert!(denial.to_string().contains("action"));

    let mut other_ref = target();
    other_ref.target_ref = "/precedents/tachi/deadbeefdeadbeef".to_string();
    assert_eq!(
        revalidate_approval(&probe, &policy, &receipt, &current(other_ref), now())
            .expect_err("approval is scoped to one precedent")
            .kind(),
        "target_mismatch"
    );

    let mut other_packet = target();
    other_packet.packet_id = "packet-1077-b".to_string();
    assert_eq!(
        revalidate_approval(&probe, &policy, &receipt, &current(other_packet), now())
            .expect_err("approval is scoped to one packet")
            .kind(),
        "target_mismatch"
    );

    let mut other_repo = target();
    other_repo.repo = "kckylechen1/other".to_string();
    assert_eq!(
        revalidate_approval(&probe, &policy, &receipt, &current(other_repo), now())
            .expect_err("approval is scoped to one repository")
            .kind(),
        "target_mismatch"
    );
}

#[test]
fn adding_an_unapproved_revision_pin_at_apply_is_refused() {
    let (probe, policy, receipt) = issue_owner_receipt();
    let mut widened = target();
    widened.repo_revision_pins.push(RepoRevisionPinV1 {
        repo: "kckylechen1/other".to_string(),
        git_ref: "refs/heads/main".to_string(),
        commit_sha: OTHER_SHA.to_string(),
    });

    let denial = revalidate_approval(&probe, &policy, &receipt, &current(widened), now())
        .expect_err("apply may not cover revisions the approval never saw");
    assert_eq!(denial.kind(), "revision_drift");
}

// ─── revocation takes effect on revalidation ────────────────────────────────

#[test]
fn team_removal_after_issuance_refuses_at_apply() {
    let delegate = principal("delegate", 8_010, "MDQ6VXNlcjgwMTA=");
    let probe = FakeProbe::new(
        delegate.clone(),
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions()),
    )
    .with_membership(
        "kckylechen1",
        "precedent-approvers",
        Ok(active_membership(
            "kckylechen1",
            "precedent-approvers",
            "delegate",
            "maintainer",
        )),
    );
    let policy = team_policy("kckylechen1", "precedent-approvers", TeamRoleV1::Member);
    let receipt = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect("active member authorized");

    // Owner removes the delegate from the team; nothing else changes.
    let after = FakeProbe::new(
        delegate,
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", full_permissions()),
    );
    let denial = revalidate_approval(&after, &policy, &receipt, &current(target()), now())
        .expect_err("revocation takes effect on revalidation");
    assert_eq!(denial.kind(), "authority_evidence_changed");
}

#[test]
fn permission_downgrade_after_issuance_refuses_at_apply() {
    let (_probe, policy, receipt) = issue_owner_receipt();
    let downgraded = FakeProbe::new(
        owner_principal(),
        repo_facts(OWNER_LOGIN, OWNER_ID, "User", read_permissions()),
    );

    let denial = revalidate_approval(&downgraded, &policy, &receipt, &current(target()), now())
        .expect_err("a downgraded permission revokes the approval");
    assert_eq!(denial.kind(), "insufficient_repo_permission");
}

#[test]
fn repository_transfer_after_issuance_refuses_at_apply() {
    let (_probe, policy, receipt) = issue_owner_receipt();
    let transferred = FakeProbe::new(
        owner_principal(),
        repo_facts("new-owner", 90_909, "User", full_permissions()),
    );

    let denial = revalidate_approval(&transferred, &policy, &receipt, &current(target()), now())
        .expect_err("the repository no longer belongs to the approver");
    assert_eq!(denial.kind(), "authority_evidence_changed");
}

#[test]
fn a_credential_swap_after_issuance_refuses_at_apply_without_leaking() {
    let (_probe, policy, receipt) = issue_owner_receipt();
    let mut swapped_principal = owner_principal();
    swapped_principal.credential_context = credential("ghp_a_completely_different_token");
    let swapped = FakeProbe::new(swapped_principal, owner_repo_facts());

    let denial = revalidate_approval(&swapped, &policy, &receipt, &current(target()), now())
        .expect_err("a different credential is a different auth context");
    assert_eq!(denial.kind(), "credential_context_changed");
    let message = denial.to_string();
    assert!(!message.contains(TOKEN));
    assert!(!message.contains("ghp_"));
    assert!(!message.contains(&credential_fingerprint(TOKEN)));
}

#[test]
fn principal_drift_after_issuance_refuses_at_apply() {
    let (_probe, policy, receipt) = issue_owner_receipt();

    let renamed = FakeProbe::new(
        principal("kckylechen1-renamed", OWNER_ID, OWNER_NODE),
        owner_repo_facts(),
    );
    assert_eq!(
        revalidate_approval(&renamed, &policy, &receipt, &current(target()), now())
            .expect_err("a rename must be re-approved")
            .kind(),
        "principal_changed"
    );

    let different_account = FakeProbe::new(
        principal(OWNER_LOGIN, 5_555, "MDQ6VXNlcjU1NTU="),
        owner_repo_facts(),
    );
    assert_eq!(
        revalidate_approval(
            &different_account,
            &policy,
            &receipt,
            &current(target()),
            now()
        )
        .expect_err("a different account cannot inherit the receipt")
        .kind(),
        "principal_changed"
    );
}

// ─── receipt integrity ──────────────────────────────────────────────────────

#[test]
fn an_edited_receipt_fails_integrity() {
    let (_probe, _policy, receipt) = issue_owner_receipt();

    let mut promoted = receipt.clone();
    promoted.required_permission = RepoPermissionLevelV1::Push;
    assert_eq!(
        promoted
            .verify_integrity()
            .expect_err("edited receipt")
            .kind(),
        "receipt_tampered"
    );

    let mut relabelled = receipt.clone();
    relabelled.receipt_version = "approval_receipt_v2".to_string();
    assert_eq!(
        relabelled
            .verify_integrity()
            .expect_err("version mismatch")
            .kind(),
        "receipt_tampered"
    );

    let mut rewritten_actor = receipt;
    rewritten_actor.caller_asserted = other_forged_context();
    assert_eq!(
        rewritten_actor
            .verify_integrity()
            .expect_err("the descriptive record is hash-covered too")
            .kind(),
        "receipt_tampered"
    );
}

#[test]
fn a_tampered_receipt_is_caught_before_any_github_traffic() {
    let (probe, policy, receipt) = issue_owner_receipt();
    let before = probe.calls().len();

    let mut tampered = receipt;
    tampered.observed_permission = full_permissions();
    tampered.required_permission = RepoPermissionLevelV1::Push;

    let denial = revalidate_approval(&probe, &policy, &tampered, &current(target()), now())
        .expect_err("integrity is checked first");
    assert_eq!(denial.kind(), "receipt_tampered");
    assert_eq!(
        probe.calls().len(),
        before,
        "a tampered receipt must not cause GitHub calls"
    );
}

// ─── time is parsed, never string-compared ──────────────────────────────────

#[test]
fn receipt_freshness_uses_parsed_instants_not_lexicographic_order() {
    // `2026-07-25T00:00:00+08:00` is the instant `2026-07-24T16:00:00Z`.
    // As raw text it sorts AFTER `2026-07-24T17:30:00Z`, so a lexicographic
    // implementation would read it as being in the future. Parsed, it is
    // 5400 seconds in the past — past a 300s freshness policy.
    let (_probe_unused, policy, mut receipt) = issue_owner_receipt();
    receipt.checked_at = "2026-07-25T00:00:00+08:00".to_string();
    let rehashed = receipt.compute_hash().expect("hash recomputes");
    receipt.receipt_hash = rehashed;

    let apply_time = at("2026-07-24T17:30:00Z");
    assert!(
        receipt.checked_at.as_str() > "2026-07-24T17:30:00Z",
        "fixture precondition: the strings sort the wrong way round"
    );

    let probe = FakeProbe::new(owner_principal(), owner_repo_facts());
    let denial = revalidate_approval(&probe, &policy, &receipt, &current(target()), apply_time)
        .expect_err("the receipt is 90 minutes old on the real timeline");
    match denial {
        AuthorityDenialV1::ReceiptExpired {
            age_secs,
            max_age_secs,
            ..
        } => {
            assert_eq!(age_secs, 5_400);
            assert_eq!(max_age_secs, 300);
        }
        other => panic!("expected ReceiptExpired, got {other:?}"),
    }
}

#[test]
fn an_unparseable_checked_at_denies_instead_of_falling_back_to_string_order() {
    let (probe, policy, mut receipt) = issue_owner_receipt();
    receipt.checked_at = "2026-07-25 12:00:00".to_string();
    let rehashed = receipt.compute_hash().expect("hash recomputes");
    receipt.receipt_hash = rehashed;

    let denial = revalidate_approval(&probe, &policy, &receipt, &current(target()), now())
        .expect_err("an unparseable timestamp must deny");
    match denial {
        AuthorityDenialV1::TimestampUnparseable { field, .. } => {
            assert_eq!(field, "receipt.checked_at");
        }
        other => panic!("expected TimestampUnparseable, got {other:?}"),
    }
}

#[test]
fn an_expired_receipt_refuses_and_a_fresh_one_does_not() {
    let (probe, policy, receipt) = issue_owner_receipt();

    // 299s later: still inside the 300s policy window.
    revalidate_approval(
        &probe,
        &policy,
        &receipt,
        &current(target()),
        at("2026-07-25T12:04:59Z"),
    )
    .expect("still fresh");

    // 301s later: outside it.
    let denial = revalidate_approval(
        &probe,
        &policy,
        &receipt,
        &current(target()),
        at("2026-07-25T12:05:01Z"),
    )
    .expect_err("stale approvals mutate nothing");
    assert_eq!(denial.kind(), "receipt_expired");
}

#[test]
fn a_receipt_from_the_future_beyond_skew_tolerance_refuses() {
    let (probe, policy, receipt) = issue_owner_receipt();

    // 59s of skew is tolerated.
    revalidate_approval(
        &probe,
        &policy,
        &receipt,
        &current(target()),
        at("2026-07-25T11:59:01Z"),
    )
    .expect("inside the skew tolerance");

    // 61s is not.
    let denial = revalidate_approval(
        &probe,
        &policy,
        &receipt,
        &current(target()),
        at("2026-07-25T11:58:59Z"),
    )
    .expect_err("a receipt cannot be checked in the future");
    match denial {
        AuthorityDenialV1::ReceiptFromFuture {
            ahead_secs,
            tolerance_secs,
            ..
        } => {
            assert_eq!(ahead_secs, 61);
            assert_eq!(tolerance_secs, 60);
        }
        other => panic!("expected ReceiptFromFuture, got {other:?}"),
    }
}

// ─── credential hygiene + determinism ───────────────────────────────────────

#[test]
fn no_credential_value_reaches_the_receipt() {
    let (_probe, _policy, receipt) = issue_owner_receipt();
    let json = serde_json::to_string(&receipt).expect("receipt serializes");
    assert!(!json.contains(TOKEN), "credential value leaked into receipt");
    assert!(!json.contains("ghp_"), "credential-shaped value in receipt");

    let fingerprint = &receipt.principal.credential_context.credential_fingerprint;
    assert_ne!(fingerprint, TOKEN);
    assert_eq!(fingerprint.len(), 64, "sha256 hex");
    assert_ne!(
        credential_fingerprint(TOKEN),
        credential_fingerprint("ghp_a_completely_different_token"),
        "different credentials must fingerprint differently"
    );
    assert_ne!(
        credential_fingerprint(TOKEN),
        sha256_hex(TOKEN.as_bytes()),
        "the fingerprint is domain-separated, not a bare token hash"
    );
}

#[test]
fn resolving_twice_at_the_same_instant_produces_an_identical_receipt() {
    let probe = FakeProbe::new(owner_principal(), owner_repo_facts());
    let policy = owner_only_policy();
    let first = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect("authorized");
    let second = resolve_verified_approver(&probe, &policy, &target(), &forged_context(), now())
        .expect("authorized");
    assert_eq!(first, second, "receipts carry no random identity");
}

#[test]
fn a_valid_receipt_revalidates_against_unchanged_live_evidence() {
    let (probe, policy, receipt) = issue_owner_receipt();
    revalidate_approval(&probe, &policy, &receipt, &current(target()), now())
        .expect("unchanged evidence revalidates");
}

// ─── permission floor semantics ─────────────────────────────────────────────

#[test]
fn permission_floor_has_no_default_allow_arm() {
    let none = RepoPermissionV1::default();
    for level in [
        RepoPermissionLevelV1::Admin,
        RepoPermissionLevelV1::Maintain,
        RepoPermissionLevelV1::Push,
    ] {
        assert!(!none.satisfies(level), "empty permissions satisfy nothing");
        assert!(!read_permissions().satisfies(level), "read satisfies nothing");
        assert!(full_permissions().satisfies(level), "admin satisfies all");
    }
    assert!(push_permissions().satisfies(RepoPermissionLevelV1::Push));
    assert!(!push_permissions().satisfies(RepoPermissionLevelV1::Maintain));
    assert!(!push_permissions().satisfies(RepoPermissionLevelV1::Admin));
}
