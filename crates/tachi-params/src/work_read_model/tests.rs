//! #1693 discriminations + the owner-ruled R6-2 transition-debt
//! discriminators, over the REAL CurrentTruth store/mint/consumer path
//! (not hand-built views) so the projection is exercised against what
//! #1696 actually admits and exposes.

use super::projector::{project, rebuild, ApplyOutcome, WorkProjectionIndex};
use super::sources::{
    AdjudicationFactV1, ClaimModeV1, ClaimStateV1, DeliveryObservationV1, ExecEnvFactV1,
    OwnerDispositionFactV1, OwnerDispositionV1, RunReceiptFactV1, SnapshotError, SourceFacts,
    SourceKind, SourceSnapshot, SourceStamp, VerificationFactV1, WorkClaimFactV1,
};
use super::types::{
    DebtClearingV1, DebtStateV1, ExecutionStateV1, ImplementationStatusV1, NextActionKindV1,
    ProjectionOptions, SectionState, WorkKey, WorkReadModelSetV1, WorkReadModelV1,
};
use crate::current_truth::consumer::{self, CallerAuthorizationV1, CurrentTruthViewV1};
use crate::current_truth::refresh::{
    mint_assertions, GithubRepositoryStateV1, SnapshotIssueStateV1, SnapshotIssueV1,
    SnapshotObservationKindV1, SnapshotObservationV1, SnapshotPrStateV1, SnapshotPrV1,
};
use crate::current_truth::store::CurrentTruthSqliteStore;
use crate::current_truth::types::{
    AssertionV1, AssertionValueV1, AuthorityClassV1, GithubObjectRefV1, PredicateV1, ReviewStateV1,
    SourceRefV1, SubjectRefV1, VisibilityClassV1,
};
use crate::taskintent::mapping::adjudication::CanonicalAdjudicationFact;
use crate::taskintent::plan::LifecycleMode;

/// Test-local sugar: every incremental apply in these fixtures must
// succeed; the raw `apply` (with its Result) is exercised where the
// outcome itself is the discrimination.
trait ApplyExt {
    fn apply_ok(&mut self, snapshot: SourceSnapshot);
}
impl ApplyExt for WorkProjectionIndex {
    fn apply_ok(&mut self, snapshot: SourceSnapshot) {
        self.apply(snapshot).expect("apply snapshot");
    }
}

const REPO: &str = "kckylechen1/tachi";
const ISSUE_TOKEN: &str = "kckylechen1/tachi#issue:100";
const READ_AT: &str = "2026-08-27T12:00:00Z";

// ── fixtures ────────────────────────────────────────────────────────────────

fn snap_issue(number: u64, state: SnapshotIssueStateV1, at: &str, rev: &str) -> SnapshotIssueV1 {
    SnapshotIssueV1 {
        number,
        state,
        updated_at: at.to_string(),
        snapshot_revision: rev.to_string(),
        visibility: VisibilityClassV1::Public,
    }
}

fn snap_pr(
    number: u64,
    state: SnapshotPrStateV1,
    merge_sha: Option<&str>,
    at: &str,
    rev: &str,
    linked: Vec<u64>,
) -> SnapshotPrV1 {
    SnapshotPrV1 {
        number,
        state,
        merge_commit_sha: merge_sha.map(str::to_string),
        updated_at: at.to_string(),
        snapshot_revision: rev.to_string(),
        linked_issues: linked,
        visibility: VisibilityClassV1::Public,
    }
}

fn observation(kind: SnapshotObservationKindV1, at: &str, rev: &str) -> SnapshotObservationV1 {
    SnapshotObservationV1 {
        kind,
        observed_at: at.to_string(),
        revision: rev.to_string(),
        visibility: VisibilityClassV1::Public,
    }
}

#[allow(clippy::too_many_arguments)]
fn repo_state(
    revision: &str,
    at: &str,
    issue: SnapshotIssueV1,
    prs: Vec<SnapshotPrV1>,
    observations: Vec<SnapshotObservationV1>,
) -> GithubRepositoryStateV1 {
    GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: revision.to_string(),
        refreshed_at: at.to_string(),
        issues: vec![issue],
        pull_requests: prs,
        observations,
    }
}

/// The canonical R6-2 timeline's steady bases.
fn state_v2_merged_open() -> GithubRepositoryStateV1 {
    repo_state(
        "r2",
        "2026-08-26T11:00:00Z",
        snap_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T11:00:00Z",
            "rev2-iss",
        ),
        vec![snap_pr(
            200,
            SnapshotPrStateV1::Merged,
            Some("mergeabc123"),
            "2026-08-26T11:00:00Z",
            "rev2-pr",
            vec![100],
        )],
        vec![],
    )
}

fn state_v4_revert_reopen() -> GithubRepositoryStateV1 {
    repo_state(
        "r4",
        "2026-08-26T13:00:00Z",
        snap_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T13:00:00Z",
            "rev4-iss",
        ),
        vec![snap_pr(
            200,
            SnapshotPrStateV1::Merged,
            Some("mergeabc123"),
            "2026-08-26T13:00:00Z",
            "rev4-pr",
            vec![100],
        )],
        vec![
            observation(
                SnapshotObservationKindV1::IssueReopened { number: 100 },
                "2026-08-26T13:30:00Z",
                "rev4-reopen",
            ),
            observation(
                SnapshotObservationKindV1::MergeReverted {
                    number: 200,
                    revert_commit_sha: "revertdef456".to_string(),
                    original_merge_sha: "mergeabc123".to_string(),
                },
                "2026-08-26T13:30:00Z",
                "rev4-revert",
            ),
        ],
    )
}

/// Revert only, no reopen — isolates the revert-debt axis.
fn state_v4_revert_only() -> GithubRepositoryStateV1 {
    repo_state(
        "r4",
        "2026-08-26T13:00:00Z",
        snap_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T13:00:00Z",
            "rev4-iss",
        ),
        vec![snap_pr(
            200,
            SnapshotPrStateV1::Merged,
            Some("mergeabc123"),
            "2026-08-26T13:00:00Z",
            "rev4-pr",
            vec![100],
        )],
        vec![observation(
            SnapshotObservationKindV1::MergeReverted {
                number: 200,
                revert_commit_sha: "revertdef456".to_string(),
                original_merge_sha: "mergeabc123".to_string(),
            },
            "2026-08-26T13:30:00Z",
            "rev4-revert",
        )],
    )
}

/// Steady state AFTER the revert: GitHub still reports PR 200 merged.
fn state_v6_steady_after_revert() -> GithubRepositoryStateV1 {
    repo_state(
        "r6",
        "2026-08-26T14:00:00Z",
        snap_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T14:00:00Z",
            "rev6-iss",
        ),
        vec![snap_pr(
            200,
            SnapshotPrStateV1::Merged,
            Some("mergeabc123"),
            "2026-08-26T14:00:00Z",
            "rev6-pr",
            vec![100],
        )],
        vec![],
    )
}

/// Authoritative close AFTER the reopen.
fn state_v7_issue_closed() -> GithubRepositoryStateV1 {
    repo_state(
        "r7",
        "2026-08-26T15:00:00Z",
        snap_issue(
            100,
            SnapshotIssueStateV1::Closed,
            "2026-08-26T15:00:00Z",
            "rev7-iss",
        ),
        vec![snap_pr(
            200,
            SnapshotPrStateV1::Merged,
            Some("mergeabc123"),
            "2026-08-26T15:00:00Z",
            "rev7-pr",
            vec![100],
        )],
        vec![],
    )
}

/// A NEW explicitly linked repair PR merged after the revert.
fn state_v8_repair_pr() -> GithubRepositoryStateV1 {
    repo_state(
        "r8",
        "2026-08-26T16:00:00Z",
        snap_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T16:00:00Z",
            "rev8-iss",
        ),
        vec![
            snap_pr(
                200,
                SnapshotPrStateV1::Merged,
                Some("mergeabc123"),
                "2026-08-26T16:00:00Z",
                "rev8-pr200",
                vec![100],
            ),
            snap_pr(
                300,
                SnapshotPrStateV1::Merged,
                Some("repairstu789"),
                "2026-08-26T16:00:00Z",
                "rev8-pr300",
                vec![100],
            ),
        ],
        vec![],
    )
}

fn issue_subject(number: u64) -> SubjectRefV1 {
    SubjectRefV1 {
        repo: REPO.to_string(),
        object: GithubObjectRefV1::Issue(number),
    }
}

fn owner_acceptance(at: &str, rev: &str) -> AssertionV1 {
    AssertionV1 {
        assertion_id: format!("owner-accept-{rev}"),
        subject: issue_subject(100),
        predicate: PredicateV1::OwnerAcceptancePresent,
        value: AssertionValueV1::Unit,
        issuer: "owner".to_string(),
        authority_class: AuthorityClassV1::OwnerDecision,
        source_ref: SourceRefV1 {
            source: "owner-decision".to_string(),
            revision: rev.to_string(),
        },
        observed_at: at.to_string(),
        effective_at: at.to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec!["owner-comment-1".to_string()],
        review_state: ReviewStateV1::Reviewed,
        visibility: VisibilityClassV1::Public,
    }
}

fn ct_view(
    states: &[GithubRepositoryStateV1],
    extra_assertions: &[AssertionV1],
    sees_private: bool,
) -> CurrentTruthViewV1 {
    let store = CurrentTruthSqliteStore::open_in_memory().expect("open store");
    for state in states {
        store
            .append_all(&mint_assertions(state))
            .expect("append state");
    }
    for assertion in extra_assertions {
        store.append(assertion).expect("append extra");
    }
    let last = states.last().expect("at least one state");
    store
        .record_refresh(
            REPO,
            true,
            Some(&last.refresh_revision),
            Some(&last.refreshed_at),
            &last.refreshed_at,
            None,
        )
        .expect("record posture");
    consumer::read_view(&store, REPO, CallerAuthorizationV1 { sees_private })
        .expect("consumer view")
}

fn ct_snapshot(view: CurrentTruthViewV1, revision: &str, observed_at: &str) -> SourceSnapshot {
    ct_snapshot_scoped(view, revision, observed_at, false)
}

/// Snapshot with an explicit MINTING authorization scope — the wrapper
// records the scope the view was actually minted under.
fn ct_snapshot_scoped(
    view: CurrentTruthViewV1,
    revision: &str,
    observed_at: &str,
    minted_sees_private: bool,
) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceKind::CurrentTruth {
            repo: REPO.to_string(),
        },
        revision,
        observed_at,
        SourceFacts::CurrentTruth(Box::new(super::sources::CurrentTruthFactsV1 {
            view,
            minted_authorization: CallerAuthorizationV1 {
                sees_private: minted_sees_private,
            },
        })),
    )
    .expect("current truth snapshot")
}

#[allow(clippy::too_many_arguments)]
fn claim_fact(
    claim_id: &str,
    issue_ref: Option<&str>,
    dispatch_id: Option<&str>,
    state: ClaimStateV1,
    expected_head: Option<&str>,
    visibility: VisibilityClassV1,
) -> WorkClaimFactV1 {
    WorkClaimFactV1 {
        claim_id: claim_id.to_string(),
        agent_identity_id: Some("agent-1".to_string()),
        session_client: Some("cli".to_string()),
        issue_ref: issue_ref.map(str::to_string),
        dispatch_id: dispatch_id.map(str::to_string),
        branch: "feat/x".to_string(),
        worktree_path: Some("/wt/x".to_string()),
        role: Some("worker".to_string()),
        mode: ClaimModeV1::Writable,
        expected_head: expected_head.map(str::to_string),
        lease_expires_at: Some("2026-08-27T13:00:00Z".to_string()),
        transition_version: 1,
        exec_env_id: None,
        state,
        heartbeat_at: "2026-08-27T10:00:00Z".to_string(),
        visibility,
    }
}

fn run_fact(
    dispatch_id: &str,
    started: bool,
    finished: bool,
    exit_code: Option<i32>,
) -> RunReceiptFactV1 {
    RunReceiptFactV1 {
        dispatch_id: dispatch_id.to_string(),
        assignment_id: None,
        lifecycle_owner: LifecycleMode::TachiManagedBatch,
        state_token: "working".to_string(),
        started_at: started.then(|| "2026-08-27T10:05:00Z".to_string()),
        finished_at: finished.then(|| "2026-08-27T11:00:00Z".to_string()),
        exit_code,
        run_dir: "/runs/x".to_string(),
        visibility: VisibilityClassV1::Public,
    }
}

fn claims_snapshot(
    claims: Vec<WorkClaimFactV1>,
    revision: &str,
    observed_at: &str,
) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceKind::WorkClaims,
        revision,
        observed_at,
        SourceFacts::WorkClaims(claims),
    )
    .expect("claims snapshot")
}

fn runs_snapshot(runs: Vec<RunReceiptFactV1>, revision: &str, observed_at: &str) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceKind::RunReceipts,
        revision,
        observed_at,
        SourceFacts::RunReceipts(runs),
    )
    .expect("runs snapshot")
}

fn verification_snapshot(
    facts: Vec<VerificationFactV1>,
    revision: &str,
    observed_at: &str,
) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceKind::Verification,
        revision,
        observed_at,
        SourceFacts::Verification(facts),
    )
    .expect("verification snapshot")
}

fn adjudication_snapshot(
    facts: Vec<AdjudicationFactV1>,
    revision: &str,
    observed_at: &str,
) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceKind::Adjudication,
        revision,
        observed_at,
        SourceFacts::Adjudication(facts),
    )
    .expect("adjudication snapshot")
}

fn dispositions_snapshot(
    facts: Vec<OwnerDispositionFactV1>,
    revision: &str,
    observed_at: &str,
) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceKind::OwnerDispositions,
        revision,
        observed_at,
        SourceFacts::OwnerDispositions(facts),
    )
    .expect("dispositions snapshot")
}

fn delivery_snapshot(observed_at: &str) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceKind::Delivery,
        "delivery-1",
        observed_at,
        SourceFacts::Delivery(DeliveryObservationV1::NotIntegrated {
            note: "fixture".to_string(),
        }),
    )
    .expect("delivery snapshot")
}

fn env_snapshot(envs: Vec<ExecEnvFactV1>, revision: &str, observed_at: &str) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceKind::ExecEnvs,
        revision,
        observed_at,
        SourceFacts::ExecEnvs(envs),
    )
    .expect("env snapshot")
}

fn find<'a>(set: &'a WorkReadModelSetV1, token: &str) -> &'a WorkReadModelV1 {
    set.items
        .iter()
        .find(|model| model.work_token() == token)
        .unwrap_or_else(|| panic!("work item {token} missing"))
}

fn github_section(model: &WorkReadModelV1) -> &super::types::GithubSectionV1 {
    match &model.github {
        SectionState::Available(section) => section,
        other => panic!("github section not available: {other:?}"),
    }
}

fn has_blocker(model: &WorkReadModelV1, kind: super::types::BlockerKindV1) -> bool {
    model.blockers.iter().any(|b| b.kind == kind)
}

fn has_action(model: &WorkReadModelV1, kind: NextActionKindV1) -> bool {
    model.next_actions.iter().any(|a| a.kind == kind)
}

fn options() -> ProjectionOptions {
    ProjectionOptions::new(READ_AT)
}

/// The full happy-path snapshot set: CurrentTruth merged+open, one active
/// claim pinned to the merge SHA, a finished verified run, and an accepted
/// adjudication.
fn full_snapshot_set() -> Vec<SourceSnapshot> {
    vec![
        ct_snapshot(
            ct_view(&[state_v2_merged_open()], &[], false),
            "ct-1",
            READ_AT,
        ),
        claims_snapshot(
            vec![claim_fact(
                "c1",
                Some(&format!("{REPO}#100")),
                Some("d1"),
                ClaimStateV1::Active,
                Some("mergeabc123"),
                VisibilityClassV1::Public,
            )],
            "claims-1",
            READ_AT,
        ),
        runs_snapshot(vec![run_fact("d1", true, true, Some(0))], "runs-1", READ_AT),
        verification_snapshot(
            vec![VerificationFactV1 {
                dispatch_id: Some("d1".to_string()),
                issue_ref: Some(format!("{REPO}#100")),
                verification_present: true,
                diff_present: true,
                evidence_refs: vec!["e1".to_string()],
                visibility: VisibilityClassV1::Public,
            }],
            "verif-1",
            READ_AT,
        ),
        adjudication_snapshot(
            vec![AdjudicationFactV1 {
                dispatch_id: "d1".to_string(),
                fact: CanonicalAdjudicationFact::Accepted,
                visibility: VisibilityClassV1::Public,
            }],
            "adj-1",
            READ_AT,
        ),
    ]
}

// ── R6-2 owner-ruled discriminators ────────────────────────────────────────

/// Ruling discriminator 1: PR merged -> merge reverted -> later snapshot
/// still says the original PR `merged`: the WorkReadModel stays
/// repair-blocked. The steady-state snapshot does NOT clear the debt.
#[test]
fn steady_state_merged_resnapshot_does_not_clear_revert_debt() {
    let view = ct_view(
        &[
            state_v2_merged_open(),
            state_v4_revert_reopen(),
            state_v6_steady_after_revert(),
        ],
        &[],
        true,
    );
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    let set = project(&index, &options());

    let model = find(&set, ISSUE_TOKEN);
    let section = github_section(model);
    assert!(
        matches!(
            &section.transition_debt.revert,
            DebtStateV1::Outstanding { .. }
        ),
        "revert debt must stay outstanding behind a newer steady-state merged snapshot"
    );
    assert_eq!(
        section.implementation_status,
        ImplementationStatusV1::Reverted
    );
    assert!(has_blocker(
        model,
        super::types::BlockerKindV1::OutstandingTransitionDebt
    ));
    assert!(has_action(model, NextActionKindV1::RepairRevertOrReopen));
    assert!(
        !model.success_shaped,
        "outstanding transition debt blocks success-shaped projection"
    );
}

/// Ruling discriminator 2: reopen -> later ordinary `issue_open` refresh:
/// reopen debt remains.
#[test]
fn plain_issue_open_refresh_does_not_clear_reopen_debt() {
    let view = ct_view(
        &[
            state_v2_merged_open(),
            state_v4_revert_reopen(),
            state_v6_steady_after_revert(),
        ],
        &[],
        true,
    );
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    let set = project(&index, &options());

    let model = find(&set, ISSUE_TOKEN);
    assert!(
        matches!(
            &github_section(model).transition_debt.reopen,
            DebtStateV1::Outstanding { .. }
        ),
        "a plain issue_open refresh must not clear reopen debt"
    );
}

/// Ruling discriminator 3: reopen -> causally later authoritative
/// `issue_closed`: reopen debt clears, but owner acceptance does NOT
/// appear unless separately evidenced.
#[test]
fn causally_later_issue_closed_clears_reopen_debt_without_acceptance() {
    let view = ct_view(
        &[
            state_v2_merged_open(),
            state_v4_revert_reopen(),
            state_v7_issue_closed(),
        ],
        &[],
        true,
    );
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    let set = project(&index, &options());

    let model = find(&set, ISSUE_TOKEN);
    let section = github_section(model);
    match &section.transition_debt.reopen {
        DebtStateV1::Cleared { by, .. } => {
            assert_eq!(*by, DebtClearingV1::LaterAuthoritativeIssueClosed)
        }
        other => panic!("reopen debt should be cleared, got {other:?}"),
    }
    // Close is NOT owner acceptance.
    let subject = section.subject.as_ref().expect("subject row");
    let acceptance = subject
        .predicates
        .iter()
        .find(|p| p.predicate == PredicateV1::OwnerAcceptancePresent)
        .expect("acceptance row exists in view");
    assert_ne!(
        acceptance.status,
        crate::current_truth::types::ReductionStatusV1::Current
    );
    assert!(!has_action(model, NextActionKindV1::AuthorizedGithubClose));

    // The revert debt on PR 200 is STILL outstanding — the close cleared
    // only the reopen axis.
    assert!(matches!(
        &section.transition_debt.revert,
        DebtStateV1::Outstanding { .. }
    ));
}

/// Ruling discriminator 4: revert -> causally later explicitly linked
/// repair PR merged: revert debt clears.
#[test]
fn post_revert_repair_pr_clears_revert_debt() {
    let view = ct_view(
        &[
            state_v2_merged_open(),
            state_v4_revert_only(),
            state_v8_repair_pr(),
        ],
        &[],
        true,
    );
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    let set = project(&index, &options());

    let model = find(&set, ISSUE_TOKEN);
    match &github_section(model).transition_debt.revert {
        DebtStateV1::Cleared { by, .. } => {
            assert_eq!(*by, DebtClearingV1::PostRevertRepairPrMerged)
        }
        other => panic!("revert debt should be cleared by repair PR, got {other:?}"),
    }
    assert!(!has_blocker(
        model,
        super::types::BlockerKindV1::OutstandingTransitionDebt
    ));
}

/// Ruling discriminator 5: revert -> owner-reviewed `no_repair_required`:
/// revert debt clears with that disposition as evidence.
#[test]
fn owner_no_repair_required_disposition_clears_revert_debt() {
    let view = ct_view(
        &[
            state_v2_merged_open(),
            state_v4_revert_reopen(),
            state_v6_steady_after_revert(),
        ],
        &[],
        true,
    );
    let disposition = OwnerDispositionFactV1 {
        issue_ref: format!("{REPO}#100"),
        observed_at: "2026-08-26T14:30:00Z".to_string(),
        disposition: OwnerDispositionV1::NoRepairRequired {
            note: "revert was intended".to_string(),
        },
        visibility: VisibilityClassV1::Public,
    };
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(dispositions_snapshot(
        vec![disposition],
        "disp-1",
        "2026-08-26T14:30:00Z",
    ));
    let set = project(&index, &options());

    let model = find(&set, ISSUE_TOKEN);
    match &github_section(model).transition_debt.revert {
        DebtStateV1::Cleared { by, evidence_heads } => {
            assert_eq!(*by, DebtClearingV1::OwnerNoRepairRequired);
            assert!(
                evidence_heads
                    .iter()
                    .any(|h| h.source == "owner_dispositions"),
                "the disposition itself is the clearing evidence"
            );
        }
        other => panic!("revert debt should be cleared by disposition, got {other:?}"),
    }
}

/// Hardening: a STALE (pre-revert) owner disposition cannot clear a newer
/// revert debt.
#[test]
fn stale_pre_revert_disposition_cannot_clear_revert_debt() {
    let view = ct_view(
        &[state_v2_merged_open(), state_v4_revert_reopen()],
        &[],
        true,
    );
    let disposition = OwnerDispositionFactV1 {
        issue_ref: format!("{REPO}#100"),
        observed_at: "2026-08-26T12:00:00Z".to_string(),
        disposition: OwnerDispositionV1::NoRepairRequired {
            note: "made before the revert existed".to_string(),
        },
        visibility: VisibilityClassV1::Public,
    };
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(dispositions_snapshot(
        vec![disposition],
        "disp-0",
        "2026-08-26T12:00:00Z",
    ));
    let set = project(&index, &options());

    let model = find(&set, ISSUE_TOKEN);
    assert!(
        matches!(
            &github_section(model).transition_debt.revert,
            DebtStateV1::Outstanding { .. }
        ),
        "a pre-revert disposition must not clear a newer revert debt"
    );
}

/// Ruling discriminator 6 + #1693 discrimination 10: full rebuild equals
/// incremental for every R6-2 case, in any arrival order.
#[test]
fn full_rebuild_equals_incremental_across_r6_2_cases_and_arrival_orders() {
    let view = ct_view(
        &[
            state_v2_merged_open(),
            state_v4_revert_reopen(),
            state_v6_steady_after_revert(),
            state_v7_issue_closed(),
            state_v8_repair_pr(),
        ],
        &[owner_acceptance("2026-08-26T17:00:00Z", "accept-1")],
        true,
    );
    let snapshots = vec![
        ct_snapshot(view, "ct-1", READ_AT),
        claims_snapshot(
            vec![claim_fact(
                "c1",
                Some(&format!("{REPO}#100")),
                Some("d1"),
                ClaimStateV1::Active,
                Some("repairstu789"),
                VisibilityClassV1::Public,
            )],
            "claims-1",
            READ_AT,
        ),
        runs_snapshot(vec![run_fact("d1", true, true, Some(0))], "runs-1", READ_AT),
        adjudication_snapshot(
            vec![AdjudicationFactV1 {
                dispatch_id: "d1".to_string(),
                fact: CanonicalAdjudicationFact::Accepted,
                visibility: VisibilityClassV1::Public,
            }],
            "adj-1",
            READ_AT,
        ),
    ];

    let incremental = {
        let mut index = WorkProjectionIndex::new();
        for snapshot in &snapshots {
            index.apply_ok(snapshot.clone());
        }
        project(&index, &options())
    };
    let canonical = project(&rebuild(snapshots.clone()).expect("rebuild"), &options());
    assert_eq!(incremental, canonical);

    // Arrival-order independence: every permutation of the same multiset
    // (5 rotations of a shuffled base) rebuilds the same projection.
    let mut rotated = snapshots.clone();
    for _ in 0..5 {
        rotated.rotate_left(1);
        let permuted = project(&rebuild(rotated.clone()).expect("rebuild"), &options());
        assert_eq!(
            permuted, canonical,
            "arrival order must never change the projection"
        );
    }
}

// ── #1693 required discriminations ────────────────────────────────────────

/// Discrimination 1: issue open -> PR merged -> issue still open -> owner
/// closes later. States remain distinct and current at every step.
#[test]
fn issue_open_merged_open_owner_close_states_remain_distinct() {
    let cases: Vec<(
        GithubRepositoryStateV1,
        Vec<AssertionV1>,
        NextActionKindV1,
        ImplementationStatusV1,
    )> = vec![
        // Open, no implementation link.
        (
            repo_state(
                "r1",
                "2026-08-26T10:00:00Z",
                snap_issue(
                    100,
                    SnapshotIssueStateV1::Open,
                    "2026-08-26T10:00:00Z",
                    "rev1-iss",
                ),
                vec![],
                vec![],
            ),
            vec![],
            NextActionKindV1::AwaitImplementation,
            ImplementationStatusV1::NotLinked,
        ),
        // Merged, issue still open: verification is the pending step.
        (
            state_v2_merged_open(),
            vec![],
            NextActionKindV1::RunVerification,
            ImplementationStatusV1::Present,
        ),
        // Owner closed, no acceptance record: closure is not acceptance.
        (
            state_v7_issue_closed(),
            vec![],
            NextActionKindV1::AwaitOwnerAcceptance,
            ImplementationStatusV1::Present,
        ),
    ];
    for (state, extra, expected_action, expected_status) in cases {
        let view = ct_view(&[state], &extra, false);
        let mut index = WorkProjectionIndex::new();
        index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
        let set = project(&index, &options());
        let model = find(&set, ISSUE_TOKEN);
        assert!(
            has_action(model, expected_action),
            "expected {expected_action:?}, got {:?}",
            model.next_actions
        );
        assert_eq!(github_section(model).implementation_status, expected_status);
    }
}

/// Discrimination 2: a brief generated before merge becomes stale after
/// merge while history remains (the old revision is not rewritten).
#[test]
fn brief_generated_before_merge_becomes_stale_after_merge() {
    let before_view = ct_view(
        &[repo_state(
            "r1",
            "2026-08-26T10:00:00Z",
            snap_issue(
                100,
                SnapshotIssueStateV1::Open,
                "2026-08-26T10:00:00Z",
                "rev1-iss",
            ),
            vec![],
            vec![],
        )],
        &[],
        true,
    );
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(before_view, "ct-1", "2026-08-26T10:00:05Z"));
    let before_brief = super::views::brief_view(find(&project(&index, &options()), ISSUE_TOKEN));

    let after_view = ct_view(&[state_v2_merged_open()], &[], false);
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(after_view, "ct-2", "2026-08-26T11:00:05Z"));
    let after_brief = super::views::brief_view(find(&project(&index, &options()), ISSUE_TOKEN));

    assert_ne!(
        before_brief.revision, after_brief.revision,
        "revision moves with the source"
    );
    assert_ne!(
        before_brief.sections, after_brief.sections,
        "the old brief is stale"
    );
    // History remains: the old brief is still renderable from the old
    // revision — nothing was rewritten in place.
    let mut replay = WorkProjectionIndex::new();
    replay.apply_ok(ct_snapshot(
        ct_view(
            &[repo_state(
                "r1",
                "2026-08-26T10:00:00Z",
                snap_issue(
                    100,
                    SnapshotIssueStateV1::Open,
                    "2026-08-26T10:00:00Z",
                    "rev1-iss",
                ),
                vec![],
                vec![],
            )],
            &[],
            true,
        ),
        "ct-1",
        "2026-08-26T10:00:05Z",
    ));
    let replayed = super::views::brief_view(find(&project(&replay, &options()), ISSUE_TOKEN));
    assert_eq!(replayed, before_brief);
}

/// Discrimination 3: PR merged then reverted/reopened changes the
/// projection without rewriting old events (assertion count in the
/// authority store only grows; the projection follows the debt).
#[test]
fn merged_reverted_reopened_changes_projection_without_rewriting() {
    let store = CurrentTruthSqliteStore::open_in_memory().expect("store");
    store
        .append_all(&mint_assertions(&state_v2_merged_open()))
        .expect("append v2");
    let count_v2 = store.assertions().expect("rows").len();

    let mut index = WorkProjectionIndex::new();
    store
        .record_refresh(
            REPO,
            true,
            Some("r2"),
            Some("2026-08-26T11:00:00Z"),
            "2026-08-26T11:00:05Z",
            None,
        )
        .expect("posture v2");
    let view_v2 = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("view v2");
    index.apply_ok(ct_snapshot(view_v2, "ct-1", READ_AT));
    let merged = find(&project(&index, &options()), ISSUE_TOKEN).clone();
    assert_eq!(
        github_section(&merged).implementation_status,
        ImplementationStatusV1::Present
    );

    store
        .append_all(&mint_assertions(&state_v4_revert_reopen()))
        .expect("append v4");
    let count_v4 = store.assertions().expect("rows").len();
    assert!(count_v4 > count_v2, "authority history only grows");

    let mut index = WorkProjectionIndex::new();
    let view_v4 = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("view v4");
    index.apply_ok(ct_snapshot(view_v4, "ct-2", READ_AT));
    let projected = project(&index, &options());
    let reverted = find(&projected, ISSUE_TOKEN);
    assert_eq!(
        github_section(reverted).implementation_status,
        ImplementationStatusV1::Reverted
    );
    assert_ne!(merged.revision, reverted.revision);
}

/// Discrimination 4: worker submit/exit cannot project accepted/complete
/// without independent adjudication — and can once adjudication accepts.
#[test]
fn worker_submit_exit_never_projects_complete_without_adjudication() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            Some("mergeabc123"),
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    index.apply_ok(runs_snapshot(
        vec![run_fact("d1", true, true, Some(0))],
        "runs-1",
        READ_AT,
    ));
    index.apply_ok(verification_snapshot(
        vec![VerificationFactV1 {
            dispatch_id: Some("d1".to_string()),
            issue_ref: Some(format!("{REPO}#100")),
            verification_present: true,
            diff_present: true,
            evidence_refs: vec!["e1".to_string()],
            visibility: VisibilityClassV1::Public,
        }],
        "verif-1",
        READ_AT,
    ));

    let unadjudicated = project(&index, &options());
    let model = find(&unadjudicated, ISSUE_TOKEN);
    assert!(
        !model.success_shaped,
        "a finished worker run alone is never complete"
    );
    assert!(!model.can_project_complete());
    assert!(has_blocker(
        model,
        super::types::BlockerKindV1::AwaitingAdjudication
    ));
    assert!(has_action(model, NextActionKindV1::Adjudicate));

    index.apply_ok(adjudication_snapshot(
        vec![AdjudicationFactV1 {
            dispatch_id: "d1".to_string(),
            fact: CanonicalAdjudicationFact::Accepted,
            visibility: VisibilityClassV1::Public,
        }],
        "adj-1",
        READ_AT,
    ));
    let adjudicated = project(&index, &options());
    let model = find(&adjudicated, ISSUE_TOKEN);
    assert!(
        model.success_shaped,
        "accepted adjudication + no blockers projects complete"
    );
    assert!(model.can_project_complete());
}

/// Discrimination 5: managed and attached run modes preserve their
/// lifecycle owners side by side.
#[test]
fn managed_and_attached_modes_preserve_lifecycle_owner() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let mut managed = run_fact("d1", true, false, None);
    managed.lifecycle_owner = LifecycleMode::TachiManagedBatch;
    let mut attached = run_fact("d2", true, false, None);
    attached.lifecycle_owner = LifecycleMode::HarnessNativeAttached;

    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(claims_snapshot(
        vec![
            claim_fact(
                "c1",
                Some(&format!("{REPO}#100")),
                Some("d1"),
                ClaimStateV1::Active,
                Some("mergeabc123"),
                VisibilityClassV1::Public,
            ),
            claim_fact(
                "c2",
                Some(&format!("{REPO}#100")),
                Some("d2"),
                ClaimStateV1::Active,
                Some("mergeabc123"),
                VisibilityClassV1::Public,
            ),
        ],
        "claims-1",
        READ_AT,
    ));
    index.apply_ok(runs_snapshot(vec![managed, attached], "runs-1", READ_AT));
    let set = project(&index, &options());

    let model = find(&set, ISSUE_TOKEN);
    let runs = match &model.run {
        SectionState::Available(section) => &section.runs,
        other => panic!("run section not available: {other:?}"),
    };
    assert_eq!(runs.len(), 2);
    assert!(runs
        .iter()
        .any(|r| r.fact.lifecycle_owner == LifecycleMode::TachiManagedBatch));
    assert!(
        runs.iter()
            .any(|r| r.fact.lifecycle_owner == LifecycleMode::HarnessNativeAttached),
        "attached lifecycle ownership stays distinct from managed"
    );
}

/// Discrimination 6: branch/worktree/head drift blocks complete/merge-ready
/// and names the repair owner.
#[test]
fn head_drift_blocks_and_names_repair_owner() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            Some("staleAAA111"),
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    let set = project(&index, &options());

    let model = find(&set, ISSUE_TOKEN);
    assert!(has_blocker(model, super::types::BlockerKindV1::HeadDrift));
    let blocker = model
        .blockers
        .iter()
        .find(|b| b.kind == super::types::BlockerKindV1::HeadDrift)
        .expect("head drift blocker");
    assert!(
        matches!(
            blocker.owner_class,
            crate::current_truth::projection::ActionOwnerClassV1::Worker
        ),
        "drift names its repair owner"
    );
    assert!(has_action(model, NextActionKindV1::RepairHeadDrift));
    assert!(!model.success_shaped);
}

/// Discrimination 7: delivery (not integrated) never rewrites execution or
/// adjudication state, and stays recoverable-shaped (`not_integrated`, not
/// a fabricated pending table).
#[test]
fn delivery_unavailable_never_rewrites_execution_or_adjudication() {
    let base = full_snapshot_set();
    let mut with_delivery = rebuild(base.clone()).expect("rebuild");
    with_delivery.apply_ok(delivery_snapshot(READ_AT));

    let without = project(&rebuild(base).expect("rebuild"), &options());
    let with = project(&with_delivery, &options());

    let model_without = find(&without, ISSUE_TOKEN);
    let model_with = find(&with, ISSUE_TOKEN);
    assert_eq!(
        model_with.run, model_without.run,
        "delivery never rewrites execution"
    );
    assert_eq!(
        model_with.adjudication, model_without.adjudication,
        "delivery never rewrites adjudication"
    );
    assert_eq!(model_with.delivery.observation.as_str(), "not_integrated");
    assert_eq!(
        model_without.delivery.observation.as_str(),
        "not_integrated"
    );
}

/// Discrimination 8: conflicting terminal/GitHub/current-truth inputs yield
/// inconsistent/conflicted, never a success-shaped guess.
#[test]
fn conflicting_inputs_yield_conflicted_never_success() {
    // GitHub-side conflict: a reviewed rival implementation link contradicts
    // the typed snapshot link.
    let rival = AssertionV1 {
        assertion_id: "reviewed-rival-1".to_string(),
        subject: issue_subject(100),
        predicate: PredicateV1::ImplementationPrLinked,
        value: AssertionValueV1::ObjectRef(GithubObjectRefV1::PullRequest(999)),
        issuer: "reviewer-1".to_string(),
        authority_class: AuthorityClassV1::ReviewedDisposition,
        source_ref: SourceRefV1 {
            source: "reviewed-mapping".to_string(),
            revision: "review-1".to_string(),
        },
        observed_at: "2026-08-26T12:00:00Z".to_string(),
        effective_at: "2026-08-26T12:00:00Z".to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec!["review-comment-7".to_string()],
        review_state: ReviewStateV1::Reviewed,
        visibility: VisibilityClassV1::Public,
    };
    let view = ct_view(&[state_v2_merged_open()], &[rival], false);
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    let set = project(&index, &options());
    let model = find(&set, ISSUE_TOKEN);
    assert!(github_section(model).conflicted);
    assert!(has_blocker(
        model,
        super::types::BlockerKindV1::GithubConflict
    ));
    assert!(has_action(model, NextActionKindV1::ResolveConflict));
    assert!(!model.success_shaped);

    // Adjudication-side conflict: differing verdicts over one outcome.
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(
        ct_view(&[state_v2_merged_open()], &[], false),
        "ct-1",
        READ_AT,
    ));
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            Some("mergeabc123"),
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    index.apply_ok(adjudication_snapshot(
        vec![
            AdjudicationFactV1 {
                dispatch_id: "d1".to_string(),
                fact: CanonicalAdjudicationFact::Accepted,
                visibility: VisibilityClassV1::Public,
            },
            AdjudicationFactV1 {
                dispatch_id: "d1".to_string(),
                fact: CanonicalAdjudicationFact::Rejected,
                visibility: VisibilityClassV1::Public,
            },
        ],
        "adj-conflict",
        READ_AT,
    ));
    let set = project(&index, &options());
    let model = find(&set, ISSUE_TOKEN);
    assert!(has_blocker(
        model,
        super::types::BlockerKindV1::AdjudicationInconsistent
    ));
    assert!(has_action(model, NextActionKindV1::ResolveConflict));
    assert!(!model.success_shaped);
}

/// Discrimination 9: out-of-order/stale source update cannot regress a
/// newer projection.
#[test]
fn out_of_order_stale_update_cannot_regress_newer_projection() {
    let newer = claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            Some("mergeabc123"),
            VisibilityClassV1::Public,
        )],
        "claims-9",
        "2026-08-27T11:00:00Z",
    );
    let older = claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            None,
            ClaimStateV1::Released,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-2",
        "2026-08-27T09:00:00Z",
    );

    let mut index = WorkProjectionIndex::new();
    assert_eq!(
        index.apply(newer.clone()).expect("apply newer"),
        ApplyOutcome::Applied
    );
    assert_eq!(
        index.apply(older).expect("apply older"),
        ApplyOutcome::StaleIgnored
    );
    assert_eq!(
        index.apply(newer.clone()).expect("re-apply"),
        ApplyOutcome::StaleIgnored,
        "idempotent re-apply"
    );

    let guarded = project(&index, &options().with_sees_private(true));
    let model = find(&guarded, ISSUE_TOKEN);
    let claims = match &model.claim {
        SectionState::Available(section) => &section.claims,
        other => panic!("claim section: {other:?}"),
    };
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].fact.state, ClaimStateV1::Active);
    assert_eq!(claims[0].fact.expected_head.as_deref(), Some("mergeabc123"));
}

/// Discrimination 10: full rebuild equals incremental projection
/// canonically (also covered for R6-2 above; this pins the general set).
#[test]
fn full_rebuild_equals_incremental_projection() {
    let incremental = {
        let mut index = WorkProjectionIndex::new();
        for snapshot in full_snapshot_set() {
            index.apply_ok(snapshot);
        }
        project(&index, &options())
    };
    let rebuilt = project(&rebuild(full_snapshot_set()).expect("rebuild"), &options());
    assert_eq!(incremental, rebuilt);
}

/// Discrimination 11: board/status/brief/complete consume the same work
/// id/revision/state.
#[test]
fn consumers_share_work_id_revision_and_state() {
    let set = project(&rebuild(full_snapshot_set()).expect("rebuild"), &options());
    let model = find(&set, ISSUE_TOKEN);

    let board = super::views::board_view(model);
    let status = super::views::status_view(model);
    let brief = super::views::brief_view(model);
    assert_eq!(board.work_token, model.work_token());
    assert_eq!(status.work_token, model.work_token());
    assert_eq!(brief.work_token, model.work_token());
    assert_eq!(board.revision, model.revision);
    assert_eq!(status.revision, model.revision);
    assert_eq!(brief.revision, model.revision);
    assert_eq!(status.success_shaped, model.can_project_complete());
    assert_eq!(board.column, "complete");
}

/// Discrimination 12: an unauthorized caller cannot infer private
/// work/result existence through projection counts/refs.
#[test]
fn unauthorized_caller_cannot_infer_private_work_existence() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let private_claim = claim_fact(
        "secret-1",
        Some(&format!("{REPO}#100")),
        Some("d9"),
        ClaimStateV1::Active,
        Some("mergeabc123"),
        VisibilityClassV1::Private,
    );
    let snapshots = vec![
        ct_snapshot(view, "ct-1", READ_AT),
        claims_snapshot(vec![private_claim], "claims-1", READ_AT),
    ];

    let unauthorized = project(
        &rebuild(snapshots.clone()).expect("rebuild"),
        &ProjectionOptions::new(READ_AT),
    );
    assert!(
        unauthorized.items.is_empty(),
        "a private-claim work item is hidden entirely"
    );
    assert_eq!(unauthorized.health.visible_work_count, 0);

    let authorized = project(
        &rebuild(snapshots).expect("rebuild"),
        &ProjectionOptions::new(READ_AT).with_sees_private(true),
    );
    assert_eq!(authorized.items.len(), 1);
    assert_eq!(authorized.health.visible_work_count, 1);
}

/// Discrimination 12 (subject-hiding half): a public claim whose issue
/// subject is absent from an unauthorized CurrentTruth view hides the work
/// item — the projection cannot distinguish private from nonexistent.
#[test]
fn public_claim_over_hidden_subject_does_not_leak() {
    // The view is minted UNAUTHORIZED: issue 500 is private, so its subject
    // row is absent from the view entirely.
    let store = CurrentTruthSqliteStore::open_in_memory().expect("store");
    let mut private_state = state_v2_merged_open();
    private_state.issues = vec![SnapshotIssueV1 {
        number: 500,
        state: SnapshotIssueStateV1::Open,
        updated_at: "2026-08-26T11:00:00Z".to_string(),
        snapshot_revision: "rev-iss500".to_string(),
        visibility: VisibilityClassV1::Private,
    }];
    private_state.pull_requests = vec![];
    store
        .append_all(&mint_assertions(&private_state))
        .expect("append");
    store
        .record_refresh(
            REPO,
            true,
            Some("r2"),
            Some("2026-08-26T11:00:00Z"),
            "2026-08-26T11:00:05Z",
            None,
        )
        .expect("posture");
    let unauthorized_view = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("view");
    assert!(
        unauthorized_view
            .subjects
            .iter()
            .all(|s| s.subject_token != format!("{REPO}#issue:500")),
        "fixture guard: the private subject is absent from the unauthorized view"
    );

    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(unauthorized_view, "ct-1", READ_AT));
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c500",
            Some(&format!("{REPO}#500")),
            None,
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    let set = project(&index, &ProjectionOptions::new(READ_AT));
    assert!(
        set.items
            .iter()
            .all(|m| m.work_token() != format!("{REPO}#issue:500")),
        "the claim must not leak the hidden subject's existence"
    );
}

/// Discrimination 13: the next_action fixture is deterministic and
/// source-bound and invokes no LLM.
#[test]
fn next_action_is_deterministic_and_source_bound() {
    let set_a = project(&rebuild(full_snapshot_set()).expect("rebuild"), &options());
    let set_b = project(&rebuild(full_snapshot_set()).expect("rebuild"), &options());
    assert_eq!(set_a, set_b, "same sources, same projection");

    let model = find(&set_a, ISSUE_TOKEN);
    assert!(!model.next_actions.is_empty());
    for action in &model.next_actions {
        assert!(
            !action.source_revisions.is_empty(),
            "every action carries its source revisions"
        );
    }
    // Frozen priority ordering.
    let ranks: Vec<u8> = model
        .next_actions
        .iter()
        .map(|a| a.kind.priority_rank())
        .collect();
    let mut sorted = ranks.clone();
    sorted.sort();
    assert_eq!(ranks, sorted);
}

// ── build/replay contract hardening ───────────────────────────────────────

/// Missing sources degrade honestly: sections name their unavailable
/// source; delivery is `not_integrated`; nothing is guessed.
#[test]
fn missing_sources_degrade_honestly() {
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    let set = project(&index, &options().with_sees_private(true));
    let model = find(&set, ISSUE_TOKEN);

    match &model.github {
        SectionState::Unavailable { source } => {
            assert_eq!(
                *source,
                SourceKind::CurrentTruth {
                    repo: REPO.to_string()
                }
            );
        }
        other => panic!("github should be unavailable, got {other:?}"),
    }
    assert!(!model.github.is_available());
    assert!(!model.run.is_available());
    assert!(!model.verification.is_available());
    assert!(!model.adjudication.is_available());
    assert_eq!(model.delivery.observation.as_str(), "not_integrated");
    assert!(
        !model.success_shaped,
        "unknown github is never success-shaped"
    );
}

/// No write path: projecting never mutates the source snapshots, and the
/// CurrentTruth authority store is byte-identical after a projection read.
#[test]
fn projection_never_writes_back_to_sources() {
    let store = CurrentTruthSqliteStore::open_in_memory().expect("store");
    store
        .append_all(&mint_assertions(&state_v2_merged_open()))
        .expect("append");
    store
        .record_refresh(
            REPO,
            true,
            Some("r2"),
            Some("2026-08-26T11:00:00Z"),
            "2026-08-26T11:00:05Z",
            None,
        )
        .expect("posture");
    let before: Vec<String> = store
        .assertions()
        .expect("rows")
        .iter()
        .map(|a| format!("{}:{}", a.assertion_id, a.source_ref.revision))
        .collect();

    let mut index = WorkProjectionIndex::new();
    let snapshots = vec![
        ct_snapshot(
            consumer::read_view(
                &store,
                REPO,
                CallerAuthorizationV1 {
                    sees_private: false,
                },
            )
            .expect("view"),
            "ct-1",
            READ_AT,
        ),
        claims_snapshot(
            vec![claim_fact(
                "c1",
                Some(&format!("{REPO}#100")),
                Some("d1"),
                ClaimStateV1::Active,
                Some("mergeabc123"),
                VisibilityClassV1::Public,
            )],
            "claims-1",
            READ_AT,
        ),
    ];
    for snapshot in &snapshots {
        index.apply_ok(snapshot.clone());
    }
    let _ = project(&index, &options());

    let after: Vec<String> = store
        .assertions()
        .expect("rows")
        .iter()
        .map(|a| format!("{}:{}", a.assertion_id, a.source_ref.revision))
        .collect();
    assert_eq!(
        before, after,
        "the authority store is untouched by projection"
    );
    // The snapshots themselves are unchanged (the projector consumed refs).
    let mut replay = WorkProjectionIndex::new();
    for snapshot in &snapshots {
        replay.apply_ok(snapshot.clone());
    }
    assert_eq!(
        project(&replay, &options()),
        project(&index, &options()),
        "re-consuming the same snapshots reproduces the projection"
    );
}

/// Reader-side claim expiry: an expired heartbeat reads as orphaned-shaped
/// (handoff/release action) without any write to the claim store.
#[test]
fn reader_expired_claim_yields_handoff_or_release() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            Some("mergeabc123"),
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    let options = ProjectionOptions::new(READ_AT).with_claim_ttl_secs(3600);
    let set = project(&index, &options);
    let model = find(&set, ISSUE_TOKEN);
    assert!(has_blocker(
        model,
        super::types::BlockerKindV1::ClaimOrphaned
    ));
    assert!(has_action(model, NextActionKindV1::HandoffOrRelease));

    // The reader-side verdict is data on the row (never a claim-store
    // write): the recorded state stays `active` while the row reads
    // expired.
    let claims = match &model.claim {
        SectionState::Available(section) => &section.claims,
        other => panic!("{other:?}"),
    };
    assert_eq!(claims[0].effectively_expired, Some(true));
    assert_eq!(claims[0].fact.state, ClaimStateV1::Active);
}

/// Two active claims on one work item are a typed collision blocker.
#[test]
fn claim_collision_is_a_typed_blocker() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(claims_snapshot(
        vec![
            claim_fact(
                "c1",
                Some(&format!("{REPO}#100")),
                Some("d1"),
                ClaimStateV1::Active,
                Some("mergeabc123"),
                VisibilityClassV1::Public,
            ),
            claim_fact(
                "c2",
                Some(&format!("{REPO}#100")),
                Some("d2"),
                ClaimStateV1::Active,
                Some("mergeabc123"),
                VisibilityClassV1::Public,
            ),
        ],
        "claims-1",
        READ_AT,
    ));
    let set = project(&index, &options());
    let model = find(&set, ISSUE_TOKEN);
    assert!(has_blocker(
        model,
        super::types::BlockerKindV1::ClaimCollision
    ));
    assert!(!model.success_shaped);
}

/// A verification-missing terminal run blocks and names run-verification.
#[test]
fn terminal_run_without_verification_blocks() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            Some("mergeabc123"),
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    index.apply_ok(runs_snapshot(
        vec![run_fact("d1", true, true, Some(0))],
        "runs-1",
        READ_AT,
    ));
    let set = project(&index, &options());
    let model = find(&set, ISSUE_TOKEN);
    assert!(has_blocker(
        model,
        super::types::BlockerKindV1::VerificationMissing
    ));
    assert!(has_action(model, NextActionKindV1::RunVerification));
}

/// Snapshot minting is fail-closed: kind/facts mismatches and empty
/// revisions are rejected, never coerced.
#[test]
fn snapshot_minting_is_fail_closed() {
    assert!(matches!(
        SourceSnapshot::new(
            SourceKind::WorkClaims,
            "",
            READ_AT,
            SourceFacts::WorkClaims(vec![])
        ),
        Err(SnapshotError::EmptyRevision(_))
    ));
    assert!(matches!(
        SourceSnapshot::new(
            SourceKind::Adjudication,
            "r1",
            READ_AT,
            SourceFacts::WorkClaims(vec![])
        ),
        Err(SnapshotError::KindMismatch { .. })
    ));
    let wrong_repo_view = CurrentTruthViewV1 {
        repo: "other/repo".to_string(),
        posture: crate::current_truth::store::RefreshPostureRowV1 {
            fresh: true,
            last_fresh_revision: Some("r1".to_string()),
            last_fresh_at: Some(READ_AT.to_string()),
            last_attempt_at: READ_AT.to_string(),
            unavailable_reason: None,
        },
        subjects: vec![],
        health: crate::current_truth::projection::ProjectionHealthV1::default(),
    };
    assert!(matches!(
        SourceSnapshot::new(
            SourceKind::CurrentTruth {
                repo: REPO.to_string()
            },
            "r1",
            READ_AT,
            SourceFacts::CurrentTruth(Box::new(super::sources::CurrentTruthFactsV1 {
                view: wrong_repo_view,
                minted_authorization: CallerAuthorizationV1 {
                    sees_private: false
                },
            }))
        ),
        Err(SnapshotError::RepoMismatch { .. })
    ));
}

/// A run with only a started timestamp projects Running; execution state
/// derives from typed timestamps, never prose.
#[test]
fn execution_state_derives_from_typed_timestamps() {
    let running = run_fact("d1", true, false, None);
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(runs_snapshot(vec![running], "runs-1", READ_AT));
    let set = project(&index, &options());
    let model = find(&set, "dispatch:d1");
    let runs = match &model.run {
        SectionState::Available(section) => &section.runs,
        other => panic!("{other:?}"),
    };
    assert_eq!(runs[0].execution_state, ExecutionStateV1::Running);
    // The state token remains source-bound data, never re-derived prose.
    assert_eq!(runs[0].fact.state_token, "working");
}

/// Stamps and the revision fingerprint: contributing sources only, sorted,
/// deduped.
#[test]
fn revision_fingerprint_covers_contributing_sources_only() {
    let set = project(&rebuild(full_snapshot_set()).expect("rebuild"), &options());
    let model = find(&set, ISSUE_TOKEN);
    let tokens: Vec<String> = model
        .source_stamps
        .iter()
        .map(SourceStamp::as_token)
        .collect();
    let mut sorted = tokens.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(tokens, sorted);
    assert!(model
        .revision
        .contains("current_truth[kckylechen1/tachi]@ct-1"));
    assert!(model.revision.contains("work_claims@claims-1"));
    // A non-contributing source (no snapshot at all) contributes no stamp.
    assert!(!model.revision.contains("exec_envs@"));
}

/// Env-only join: an env bound by claim id joins the claim's work item and
/// drift compares claim expected head vs env base when GitHub evidence is
/// absent.
#[test]
fn env_joins_by_claim_and_detects_drift_without_github() {
    let env = ExecEnvFactV1 {
        env_id: "env-1".to_string(),
        kind: "worktree".to_string(),
        path: "/wt/x".to_string(),
        repo_root: "/repo".to_string(),
        branch: "feat/x".to_string(),
        base_sha: Some("baseAAA000".to_string()),
        dispatch_id: None,
        claim_id: Some("c1".to_string()),
        state_token: "active".to_string(),
        reclaim_reason: None,
        visibility: VisibilityClassV1::Public,
    };
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            None,
            None,
            ClaimStateV1::Active,
            Some("expectedFFF999"),
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    index.apply_ok(env_snapshot(vec![env], "envs-1", READ_AT));
    let set = project(&index, &options());
    let model = find(&set, "claim:c1");
    assert!(model.exec_env.is_available());
    assert!(has_blocker(model, super::types::BlockerKindV1::HeadDrift));
}

/// WorkKey parsing round-trips the CurrentTruth subject token shape.
#[test]
fn work_key_parses_issue_refs() {
    assert_eq!(
        WorkKey::parse_issue_ref("kckylechen1/tachi#100"),
        Some(WorkKey::Issue {
            repo: "kckylechen1/tachi".to_string(),
            number: 100
        })
    );
    assert_eq!(WorkKey::parse_issue_ref("no-slash#1"), None);
    assert_eq!(WorkKey::parse_issue_ref("a/b#notanumber"), None);
    assert_eq!(WorkKey::parse_issue_ref("a/b#"), None);
}

// ── codex R1 accepted findings: new discriminations ───────────────────────

/// Equal ordering key with DIFFERENT content is rejected fail-closed in
/// every arrival order (the #1696 `ContradictsExistingRevision` law
/// mirrored at the snapshot boundary — arrival order never picks a winner).
#[test]
fn equal_key_different_content_is_rejected_in_every_arrival_order() {
    let a = claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-same",
        READ_AT,
    );
    let b = claims_snapshot(
        vec![claim_fact(
            "c9",
            Some(&format!("{REPO}#100")),
            Some("d9"),
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-same",
        READ_AT,
    );

    let mut forward = WorkProjectionIndex::new();
    forward.apply(a.clone()).expect("first applies");
    let forward_err = forward.apply(b.clone()).expect_err("conflict rejected");
    let mut reverse = WorkProjectionIndex::new();
    reverse.apply(b.clone()).expect("first applies");
    let reverse_err = reverse.apply(a.clone()).expect_err("conflict rejected");
    assert_eq!(
        forward_err, reverse_err,
        "the rejection is order-independent"
    );

    assert!(matches!(
        rebuild(vec![a.clone(), b.clone()]),
        Err(SnapshotError::ContentConflict { .. })
    ));
    // An identical same-key duplicate stays idempotent.
    let mut index = WorkProjectionIndex::new();
    index.apply(a.clone()).expect("applies");
    assert_eq!(
        index.apply(a).expect("re-apply"),
        ApplyOutcome::StaleIgnored
    );
}

/// An empty snapshot is Available knowledge ("no facts at this revision"),
/// not Unavailability — and it stamps the sections it made Available.
#[test]
fn empty_snapshot_is_available_knowledge_not_unavailability() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let with_empty = vec![
        ct_snapshot(view.clone(), "ct-1", READ_AT),
        claims_snapshot(vec![], "claims-empty", READ_AT),
    ];
    let set = project(&rebuild(with_empty).expect("rebuild"), &options());
    let model = find(&set, ISSUE_TOKEN);
    assert!(
        model.claim.is_available(),
        "an empty claims snapshot asserts 'no claims bind', it is not unavailability"
    );
    assert!(model.revision.contains("work_claims@claims-empty"));

    let without = vec![ct_snapshot(view, "ct-1", READ_AT)];
    let set = project(&rebuild(without).expect("rebuild"), &options());
    let model = find(&set, ISSUE_TOKEN);
    assert!(!model.claim.is_available());
    assert!(!model.revision.contains("work_claims@"));
}

/// An issue with no implementation link never projects success-shaped,
/// even with an accepted adjudication bound through a claim.
#[test]
fn not_linked_never_success_shaped_even_with_accepted_adjudication() {
    let view = ct_view(
        &[repo_state(
            "r1",
            "2026-08-26T10:00:00Z",
            snap_issue(
                100,
                SnapshotIssueStateV1::Open,
                "2026-08-26T10:00:00Z",
                "rev1-iss",
            ),
            vec![],
            vec![],
        )],
        &[],
        true,
    );
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    index.apply_ok(adjudication_snapshot(
        vec![AdjudicationFactV1 {
            dispatch_id: "d1".to_string(),
            fact: CanonicalAdjudicationFact::Accepted,
            visibility: VisibilityClassV1::Public,
        }],
        "adj-1",
        READ_AT,
    ));
    let set = project(&index, &options());
    let model = find(&set, ISSUE_TOKEN);
    assert!(
        !model.success_shaped,
        "positive implementation evidence is required"
    );
    assert_eq!(
        github_section(model).implementation_status,
        ImplementationStatusV1::NotLinked
    );
}

/// The board never presents an outstanding reverted implementation as
/// `landed` (R6-2 owner ruling).
#[test]
fn reverted_board_column_is_not_landed() {
    let view = ct_view(
        &[
            state_v2_merged_open(),
            state_v4_revert_reopen(),
            state_v6_steady_after_revert(),
        ],
        &[],
        true,
    );
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    let set = project(&index, &options());
    let model = find(&set, ISSUE_TOKEN);
    let board = super::views::board_view(model);
    assert_eq!(board.column, "reverted");
    assert_ne!(board.column, "landed");
}

/// A private verification fact hides the whole work item from an
/// unauthorized caller.
#[test]
fn private_verification_fact_hides_the_work_item() {
    let view = ct_view(&[state_v2_merged_open()], &[], false);
    let snapshots = vec![
        ct_snapshot(view, "ct-1", READ_AT),
        verification_snapshot(
            vec![VerificationFactV1 {
                dispatch_id: Some("d1".to_string()),
                issue_ref: Some(format!("{REPO}#100")),
                verification_present: true,
                diff_present: true,
                evidence_refs: vec!["e1".to_string()],
                visibility: VisibilityClassV1::Private,
            }],
            "verif-1",
            READ_AT,
        ),
    ];
    let unauthorized = project(
        &rebuild(snapshots.clone()).expect("rebuild"),
        &ProjectionOptions::new(READ_AT),
    );
    assert!(
        unauthorized.items.is_empty(),
        "private verification evidence hides the item"
    );
    let authorized = project(
        &rebuild(snapshots).expect("rebuild"),
        &ProjectionOptions::new(READ_AT).with_sees_private(true),
    );
    assert_eq!(authorized.items.len(), 1);
}

/// With no CurrentTruth view at all, an unauthorized caller cannot see an
/// issue-keyed item (fail-closed: hidden subject and unknown repo are
/// indistinguishable).
#[test]
fn unauthorized_missing_repo_view_hides_issue_keyed_work() {
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-1",
        READ_AT,
    ));
    let unauthorized = project(&index, &ProjectionOptions::new(READ_AT));
    assert!(
        unauthorized.items.is_empty(),
        "no repo view + unauthorized = no issue-keyed leakage"
    );
    let authorized = project(
        &index,
        &ProjectionOptions::new(READ_AT).with_sees_private(true),
    );
    let model = find(&authorized, ISSUE_TOKEN);
    assert!(
        !model.github.is_available(),
        "authorized sees the item with an honestly unknown github section"
    );
}

/// A reverted PR that a later authoritative link-set update UNLINKS from
/// its issue still carries R6-2 debt: it is counted (content-free) so it
/// cannot disappear silently. Unlinking is not a causal resolution.
#[test]
fn unlinked_reverted_pr_counts_as_orphaned_debt() {
    // v9: PR 200 no longer linked to issue 100 (authoritative link
    // removal), but the revert observation remains in history and the PR
    // subject still reports merged.
    let v9 = repo_state(
        "r9",
        "2026-08-26T17:00:00Z",
        snap_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T17:00:00Z",
            "rev9-iss",
        ),
        vec![snap_pr(
            200,
            SnapshotPrStateV1::Merged,
            Some("mergeabc123"),
            "2026-08-26T17:00:00Z",
            "rev9-pr",
            vec![],
        )],
        vec![],
    );
    let view = ct_view(
        &[state_v2_merged_open(), state_v4_revert_reopen(), v9],
        &[],
        true,
    );
    let mut index = WorkProjectionIndex::new();
    index.apply_ok(ct_snapshot(view, "ct-1", READ_AT));
    let set = project(&index, &options());
    assert_eq!(
        set.health.orphaned_revert_debt_count, 1,
        "the unlinked reverted PR's transition debt stays visible (content-free count)"
    );
    // What the per-issue projection can still see: the issue no longer
    // names the reverted PR, so its own debt row is gone — the health
    // counter is the honest residue until the consumer view exposes
    // superseded link lineage (#1696 integration-slice follow-up).
    let model = find(&set, ISSUE_TOKEN);
    assert!(matches!(
        github_section(model).transition_debt.revert,
        DebtStateV1::None
    ));
}

/// A superseding snapshot arriving BETWEEN two equal-key contradictory
/// snapshots cannot mask the conflict — every arrival order of the multiset
/// rejects it (codex R2 round-2 finding 1).
#[test]
fn superseding_snapshot_cannot_mask_an_equal_key_conflict() {
    let a = claims_snapshot(
        vec![claim_fact(
            "c1",
            Some(&format!("{REPO}#100")),
            Some("d1"),
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-key-1",
        "2026-08-27T09:00:00Z",
    );
    let b = claims_snapshot(
        vec![claim_fact(
            "c9",
            Some(&format!("{REPO}#100")),
            Some("d9"),
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-key-1",
        "2026-08-27T09:00:00Z",
    );
    let newer = claims_snapshot(
        vec![claim_fact(
            "c5",
            Some(&format!("{REPO}#100")),
            Some("d5"),
            ClaimStateV1::Active,
            None,
            VisibilityClassV1::Public,
        )],
        "claims-key-2",
        "2026-08-27T10:00:00Z",
    );

    let mut perms: Vec<Vec<SourceSnapshot>> = vec![
        vec![a.clone(), newer.clone(), b.clone()],
        vec![a.clone(), b.clone(), newer.clone()],
        vec![newer.clone(), a.clone(), b.clone()],
        vec![newer.clone(), b.clone(), a.clone()],
        vec![b.clone(), a.clone(), newer.clone()],
        vec![b.clone(), newer.clone(), a.clone()],
    ];
    for permutation in perms.drain(..) {
        let outcome = rebuild(permutation);
        assert!(
            matches!(outcome, Err(SnapshotError::ContentConflict { .. })),
            "every arrival order must reject the equal-key contradiction"
        );
    }

    // Without the contradiction the fold succeeds and keeps the newest.
    let index = rebuild(vec![a, newer]).expect("clean multiset rebuilds");
    let set = project(&index, &options().with_sees_private(true));
    let model = find(&set, ISSUE_TOKEN);
    let claims = match &model.claim {
        SectionState::Available(section) => &section.claims,
        other => panic!("{other:?}"),
    };
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].fact.claim_id, "c5");
}

/// A CurrentTruth view minted under `sees_private=true` cannot serve an
/// unauthorized read at all: the snapshot is unusable for that read
/// (github Unavailable, items hidden, health counters excluded) — never
/// treated as filtered content (codex R2 round-2 finding 2).
#[test]
fn private_scoped_current_truth_view_is_unusable_for_unauthorized_reads() {
    let view = ct_view(&[state_v2_merged_open()], &[], true);
    let snapshot = ct_snapshot_scoped(view, "ct-1", READ_AT, true);

    let unauthorized = project(
        &rebuild(vec![snapshot.clone()]).expect("rebuild"),
        &ProjectionOptions::new(READ_AT),
    );
    assert!(
        unauthorized.items.is_empty(),
        "a private-scoped view must not serve an unauthorized read"
    );
    assert_eq!(unauthorized.health.visible_work_count, 0);

    let authorized = project(
        &rebuild(vec![snapshot]).expect("rebuild"),
        &ProjectionOptions::new(READ_AT).with_sees_private(true),
    );
    let model = find(&authorized, ISSUE_TOKEN);
    assert!(model.github.is_available());
}
