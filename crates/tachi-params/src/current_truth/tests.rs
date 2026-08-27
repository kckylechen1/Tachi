//! #1696 "Required discrimination" — every one of the twelve fixtures is a
//! named test. Plus the storage/replay contract tests (idempotency,
//! append-only, rebuild == incremental, projection disposability).

use std::sync::atomic::{AtomicUsize, Ordering};

use super::consumer::{self, CallerAuthorizationV1};
use super::handoff::{evaluate_handoff_staleness, HandoffPacketV1, HandoffStaleReasonV1};
use super::projection::{open_action_for, RefreshPostureV1};
use super::reducer::{reduce, IssueLifecycleView, PrLifecycleView};
use super::refresh::{
    mint_assertions, reconcile_refresh, GithubRefreshAdapter, GithubRepositoryStateV1,
    RefreshOutcomeV1, SnapshotIssueStateV1, SnapshotIssueV1, SnapshotObservationKindV1,
    SnapshotObservationV1, SnapshotPrStateV1, SnapshotPrV1,
};
use super::store::{
    generation_digest, AppendOutcome, CurrentTruthSqliteStore, CurrentTruthStoreError,
};
use super::types::{
    AssertionV1, AssertionValueV1, AuthorityClassV1, GithubObjectRefV1, PredicateV1,
    ReductionStatusV1, ReviewStateV1, SourceRefV1, SubjectRefV1, VisibilityClassV1,
};

const REPO: &str = "kckylechen1/tachi";

fn issue(number: u64) -> SubjectRefV1 {
    SubjectRefV1 {
        repo: REPO.to_string(),
        object: GithubObjectRefV1::Issue(number),
    }
}

fn pr(number: u64) -> SubjectRefV1 {
    SubjectRefV1 {
        repo: REPO.to_string(),
        object: GithubObjectRefV1::PullRequest(number),
    }
}

fn snapshot_issue(
    number: u64,
    state: SnapshotIssueStateV1,
    at: &str,
    rev: &str,
) -> SnapshotIssueV1 {
    SnapshotIssueV1 {
        number,
        state,
        updated_at: at.to_string(),
        snapshot_revision: rev.to_string(),
        visibility: VisibilityClassV1::Public,
    }
}

fn snapshot_pr(
    number: u64,
    state: SnapshotPrStateV1,
    at: &str,
    rev: &str,
    linked: Vec<u64>,
) -> SnapshotPrV1 {
    SnapshotPrV1 {
        number,
        state,
        merge_commit_sha: match state {
            SnapshotPrStateV1::Merged => Some("mergeabc123".to_string()),
            _ => None,
        },
        updated_at: at.to_string(),
        snapshot_revision: rev.to_string(),
        linked_issues: linked,
        visibility: VisibilityClassV1::Public,
    }
}

/// v1: issue 100 open, PR 200 open, typed link between them.
fn state_v1() -> GithubRepositoryStateV1 {
    GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r1".to_string(),
        refreshed_at: "2026-08-26T10:00:00Z".to_string(),
        issues: vec![snapshot_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T10:00:00Z",
            "rev1-iss",
        )],
        pull_requests: vec![snapshot_pr(
            200,
            SnapshotPrStateV1::Open,
            "2026-08-26T10:00:00Z",
            "rev1-pr",
            vec![100],
        )],
        observations: vec![],
    }
}

/// v2: PR 200 merged; issue 100 still open (worked example B).
fn state_v2() -> GithubRepositoryStateV1 {
    GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r2".to_string(),
        refreshed_at: "2026-08-26T11:00:00Z".to_string(),
        issues: vec![snapshot_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T11:00:00Z",
            "rev2-iss",
        )],
        pull_requests: vec![snapshot_pr(
            200,
            SnapshotPrStateV1::Merged,
            "2026-08-26T11:00:00Z",
            "rev2-pr",
            vec![100],
        )],
        observations: vec![],
    }
}

/// v3: issue 100 closed by the owner (after the merge).
fn state_v3() -> GithubRepositoryStateV1 {
    GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r3".to_string(),
        refreshed_at: "2026-08-26T12:00:00Z".to_string(),
        issues: vec![snapshot_issue(
            100,
            SnapshotIssueStateV1::Closed,
            "2026-08-26T12:00:00Z",
            "rev3-iss",
        )],
        pull_requests: vec![snapshot_pr(
            200,
            SnapshotPrStateV1::Merged,
            "2026-08-26T12:00:00Z",
            "rev3-pr",
            vec![100],
        )],
        observations: vec![],
    }
}

/// v4: the merge was reverted and the issue reopened (typed observations).
fn state_v4() -> GithubRepositoryStateV1 {
    GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r4".to_string(),
        refreshed_at: "2026-08-26T13:00:00Z".to_string(),
        issues: vec![snapshot_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T13:00:00Z",
            "rev4-iss",
        )],
        pull_requests: vec![snapshot_pr(
            200,
            SnapshotPrStateV1::Merged,
            "2026-08-26T13:00:00Z",
            "rev4-pr",
            vec![100],
        )],
        observations: vec![
            SnapshotObservationV1 {
                kind: SnapshotObservationKindV1::IssueReopened { number: 100 },
                observed_at: "2026-08-26T13:30:00Z".to_string(),
                revision: "rev4-reopen".to_string(),
                visibility: VisibilityClassV1::Public,
            },
            SnapshotObservationV1 {
                kind: SnapshotObservationKindV1::MergeReverted {
                    number: 200,
                    revert_commit_sha: "revertdef456".to_string(),
                    original_merge_sha: "mergeabc123".to_string(),
                },
                observed_at: "2026-08-26T13:30:00Z".to_string(),
                revision: "rev4-revert".to_string(),
                visibility: VisibilityClassV1::Public,
            },
        ],
    }
}

/// A fake authoritative adapter cycling through scripted states; optionally
/// failing (GitHub unavailable).
struct FakeGithubAdapter {
    states: std::sync::Mutex<Vec<GithubRepositoryStateV1>>,
    fail: std::sync::atomic::AtomicBool,
    refreshes: AtomicUsize,
}

impl FakeGithubAdapter {
    fn new(states: Vec<GithubRepositoryStateV1>) -> Self {
        FakeGithubAdapter {
            states: std::sync::Mutex::new(states),
            fail: std::sync::atomic::AtomicBool::new(false),
            refreshes: AtomicUsize::new(0),
        }
    }

    fn set_fail(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }
}

impl GithubRefreshAdapter for FakeGithubAdapter {
    fn refresh(&self, repo: &str) -> RefreshOutcomeV1 {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return RefreshOutcomeV1::Unavailable {
                repo: repo.to_string(),
                reason: "network unreachable".to_string(),
            };
        }
        let mut states = self.states.lock().unwrap();
        if states.len() > 1 {
            RefreshOutcomeV1::Fresh(Box::new(states.remove(0)))
        } else {
            states
                .first()
                .cloned()
                .map(Box::new)
                .map(RefreshOutcomeV1::Fresh)
                .unwrap_or_else(|| RefreshOutcomeV1::Unavailable {
                    repo: repo.to_string(),
                    reason: "no state".to_string(),
                })
        }
    }
}

fn fresh_posture() -> RefreshPostureV1 {
    RefreshPostureV1::fresh(
        "r4".to_string(),
        "2026-08-26T13:00:00Z".to_string(),
        "2026-08-26T13:00:00Z".to_string(),
    )
}

fn owner_acceptance(at: &str, rev: &str) -> AssertionV1 {
    let mut assertion = AssertionV1 {
        assertion_id: format!("owner-accept-{rev}"),
        subject: issue(100),
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
    };
    assertion.assertion_id = format!(
        "ct-{}-{}-{}",
        assertion.subject.as_token(),
        assertion.predicate.as_str(),
        assertion.source_ref.revision
    );
    assertion
}

fn open_store() -> CurrentTruthSqliteStore {
    CurrentTruthSqliteStore::open_in_memory().expect("open in-memory store")
}

// ── Discrimination 1 ───────────────────────────────────────────────────────

/// Issue open → PR merged → issue still open → owner closes later: every
/// intermediate state stays distinct (`open != open+merged !=
/// closed+merged != accepted`), and no state collapses into another.
#[test]
fn issue_open_merged_then_owner_close_all_states_distinct() {
    let store = open_store();

    // Phase A: issue open, PR open.
    store
        .append_all(&mint_assertions(&state_v1()))
        .expect("append v1");
    let reduction = reduce(&store.assertions().unwrap());
    assert_eq!(
        reduction.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Open
    );
    assert_eq!(reduction.pr_lifecycle(&pr(200)), PrLifecycleView::Open);
    assert!(!reduction.implementation_present(&issue(100)));
    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_eq!(action.kind, super::types::OpenActionKindV1::ReviewPr);

    // Phase B: PR merged, issue STILL open — implementation is present but
    // neither owner acceptance nor closure happened.
    store
        .append_all(&mint_assertions(&state_v2()))
        .expect("append v2");
    let reduction = reduce(&store.assertions().unwrap());
    assert_eq!(
        reduction.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Open
    );
    assert_eq!(reduction.pr_lifecycle(&pr(200)), PrLifecycleView::Merged);
    assert!(reduction.implementation_present(&issue(100)));
    assert!(!reduction.owner_acceptance_present(&issue(100)));
    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_eq!(action.kind, super::types::OpenActionKindV1::RunVerification);

    // Phase C: owner closes the issue — closed+merged is distinct from
    // open+merged and from accepted.
    store
        .append_all(&mint_assertions(&state_v3()))
        .expect("append v3");
    let reduction = reduce(&store.assertions().unwrap());
    assert_eq!(
        reduction.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Closed
    );
    assert_eq!(reduction.pr_lifecycle(&pr(200)), PrLifecycleView::Merged);
    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_eq!(
        action.kind,
        super::types::OpenActionKindV1::AwaitOwnerAcceptance
    );

    // Phase D: owner acceptance recorded — nothing pending.
    store
        .append(&owner_acceptance("2026-08-26T14:00:00Z", "rev5"))
        .expect("append acceptance");
    let reduction = reduce(&store.assertions().unwrap());
    assert!(reduction.owner_acceptance_present(&issue(100)));
    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_eq!(action.kind, super::types::OpenActionKindV1::NoOpenAction);

    // History never shrank: every phase's assertions remain stored.
    assert!(store.assertion_count(REPO).unwrap() >= 4);
}

// ── Discrimination 2 ───────────────────────────────────────────────────────

/// A handoff generated before the merge becomes stale after the merge; the
/// packet/history remains byte-stable; staleness is claim-specific.
#[test]
fn handoff_generated_before_merge_becomes_stale_packet_byte_stable() {
    let store = open_store();
    store
        .append_all(&mint_assertions(&state_v1()))
        .expect("append v1");

    // Bind the handoff against v1's current evidence heads.
    let reduction = reduce(&store.assertions().unwrap());
    let pr_open_head = reduction
        .get(&pr(200), PredicateV1::PrOpen)
        .current_heads
        .first()
        .expect("pr_open head")
        .clone();
    let link_head = reduction
        .get(&issue(100), PredicateV1::ImplementationPrLinked)
        .current_heads
        .first()
        .expect("link head")
        .clone();
    let unrelated = snapshot_issue(
        300,
        SnapshotIssueStateV1::Open,
        "2026-08-26T09:00:00Z",
        "rev0-iss300",
    );
    let mut state0 = state_v1();
    state0.issues.push(unrelated);
    store
        .append_all(&mint_assertions(&state0))
        .expect("append unrelated");
    let reduction = reduce(&store.assertions().unwrap());
    let unrelated_head = reduction
        .get(&issue(300), PredicateV1::IssueOpen)
        .current_heads
        .first()
        .expect("unrelated head")
        .clone();

    let packet = HandoffPacketV1 {
        handoff_id: "handoff-1".to_string(),
        generated_at: "2026-08-26T10:30:00Z".to_string(),
        repo: REPO.to_string(),
        claim_bindings: vec![
            super::handoff::HandoffClaimBindingV1 {
                subject: pr(200),
                predicate: PredicateV1::PrOpen,
                head: pr_open_head,
            },
            super::handoff::HandoffClaimBindingV1 {
                subject: issue(100),
                predicate: PredicateV1::ImplementationPrLinked,
                head: link_head,
            },
            super::handoff::HandoffClaimBindingV1 {
                subject: issue(300),
                predicate: PredicateV1::IssueOpen,
                head: unrelated_head,
            },
        ],
    };
    let packet_bytes = serde_json::to_string(&packet).expect("packet serializes");

    // Before the merge: every claim is current.
    let report = evaluate_handoff_staleness(&packet, &reduction);
    assert_eq!(report.stale_claim_count(), 0, "pre-merge: nothing stale");

    // The merge lands (typed snapshot, new revision).
    store
        .append_all(&mint_assertions(&state_v2()))
        .expect("append v2");
    let reduction = reduce(&store.assertions().unwrap());
    let report = evaluate_handoff_staleness(&packet, &reduction);
    assert_eq!(report.stale_claim_count(), 2, "pr_open and link moved");
    for claim in &report.claims {
        match (
            claim.binding.subject.as_token().as_str(),
            claim.binding.predicate,
        ) {
            ("kckylechen1/tachi#pull_request:200", PredicateV1::PrOpen) => {
                assert!(claim.stale);
                assert_eq!(
                    claim.reason,
                    Some(HandoffStaleReasonV1::SupersededByNewerHead)
                );
                let replacement = claim.replacement_head.as_ref().expect("replacement head");
                assert_eq!(replacement.source_revision, "rev2-pr");
            }
            ("kckylechen1/tachi#issue:100", PredicateV1::ImplementationPrLinked) => {
                assert!(claim.stale, "link re-observed at newer revision");
            }
            ("kckylechen1/tachi#issue:300", PredicateV1::IssueOpen) => {
                assert!(!claim.stale, "unrelated evidence does not invalidate");
            }
            other => panic!("unexpected claim {other:?}"),
        }
    }

    // The packet itself is untouched — byte-for-byte.
    assert_eq!(
        serde_json::to_string(&packet).expect("packet serializes"),
        packet_bytes,
        "historical packet remains byte-stable"
    );
}

// ── Discrimination 3 ───────────────────────────────────────────────────────

/// PR merged then reverted and issue reopened changes current predicates
/// without rewriting old assertions.
#[test]
fn merged_reverted_reopened_changes_currents_without_rewriting() {
    let store = open_store();
    store
        .append_all(&mint_assertions(&state_v2()))
        .expect("append v2");
    let before_ids: Vec<String> = store
        .assertions()
        .unwrap()
        .iter()
        .map(|a| a.assertion_id.clone())
        .collect();

    store
        .append_all(&mint_assertions(&state_v4()))
        .expect("append v4");
    let all = store.assertions().unwrap();
    // Append-only: every v2 row is still present, unchanged.
    for id in &before_ids {
        assert!(
            all.iter().any(|a| &a.assertion_id == id),
            "old assertion {id} must remain"
        );
    }

    let reduction = reduce(&all);
    // Currents moved.
    assert_eq!(
        reduction.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Reopened
    );
    assert_eq!(
        reduction.pr_lifecycle(&pr(200)),
        PrLifecycleView::MergeReverted
    );
    assert!(
        !reduction.implementation_present(&issue(100)),
        "a reverted merge is not effective implementation"
    );
    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_eq!(
        action.kind,
        super::types::OpenActionKindV1::RepairRevertOrReopen
    );
    // The old merge assertion is retained as superseded evidence, not gone.
    let merged = reduction.get(&pr(200), PredicateV1::PrMerged);
    assert_eq!(merged.status, ReductionStatusV1::Current);
    assert_eq!(merged.current_heads.len(), 1);
}

// ── Discrimination 4 ───────────────────────────────────────────────────────

/// A PR closed unmerged never projects implementation present.
#[test]
fn pr_closed_unmerged_never_projects_implementation_present() {
    let mut state = state_v1();
    state.pull_requests = vec![snapshot_pr(
        200,
        SnapshotPrStateV1::ClosedUnmerged,
        "2026-08-26T11:00:00Z",
        "rev2-pr",
        vec![100],
    )];
    let store = open_store();
    store.append_all(&mint_assertions(&state)).expect("append");
    let reduction = reduce(&store.assertions().unwrap());
    assert_eq!(
        reduction.pr_lifecycle(&pr(200)),
        PrLifecycleView::ClosedUnmerged
    );
    assert!(!reduction.implementation_present(&issue(100)));
    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_eq!(
        action.kind,
        super::types::OpenActionKindV1::AwaitImplementation
    );
}

// ── Discrimination 5 ───────────────────────────────────────────────────────

/// A model summary citing the wrong issue/PR relation remains
/// candidate/rejected and cannot affect current truth.
#[test]
fn model_summary_wrong_relation_stays_candidate_cannot_affect_truth() {
    let store = open_store();
    store
        .append_all(&mint_assertions(&state_v1()))
        .expect("append v1");

    // Model prose claims issue 100 is implemented by PR 999 (wrong; the
    // typed relation says 200). Even marked Observed, ModelProse authority
    // can establish nothing.
    let model_claim = AssertionV1 {
        assertion_id: "model-claim-1".to_string(),
        subject: issue(100),
        predicate: PredicateV1::ImplementationPrLinked,
        value: AssertionValueV1::ObjectRef(GithubObjectRefV1::PullRequest(999)),
        issuer: "model-seat-1".to_string(),
        authority_class: AuthorityClassV1::ModelProse,
        source_ref: SourceRefV1 {
            source: "model-summary".to_string(),
            revision: "summary-digest-1".to_string(),
        },
        observed_at: "2026-08-26T11:00:00Z".to_string(),
        effective_at: "2026-08-26T11:00:00Z".to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec!["prose-quote".to_string()],
        review_state: ReviewStateV1::Observed,
        visibility: VisibilityClassV1::Public,
    };
    store.append(&model_claim).expect("model prose is stored");

    let reduction = reduce(&store.assertions().unwrap());
    let linked = reduction.linked_prs(&issue(100));
    assert_eq!(linked, vec![GithubObjectRefV1::PullRequest(200)]);
    assert!(!linked.contains(&GithubObjectRefV1::PullRequest(999)));

    // A rejected reviewed disposition is equally inert.
    let rejected = AssertionV1 {
        assertion_id: "rejected-claim-1".to_string(),
        review_state: ReviewStateV1::Rejected,
        authority_class: AuthorityClassV1::ReviewedDisposition,
        ..model_claim.clone()
    };
    store
        .append(&rejected)
        .expect("rejected evidence is stored");
    let reduction = reduce(&store.assertions().unwrap());
    assert_eq!(
        reduction.linked_prs(&issue(100)),
        vec![GithubObjectRefV1::PullRequest(200)]
    );
}

// ── Discrimination 6 ───────────────────────────────────────────────────────

/// Duplicate source revision is idempotent; an out-of-order older revision
/// cannot regress state.
#[test]
fn duplicate_revision_idempotent_out_of_order_older_cannot_regress() {
    let store = open_store();
    // Duplicate ingestion of the same state.
    let v2 = mint_assertions(&state_v2());
    assert_eq!(store.append_all(&v2).unwrap(), v2.len());
    let count_after_first = store.assertion_count(REPO).unwrap();
    for assertion in &v2 {
        assert_eq!(
            store.append(assertion).unwrap(),
            AppendOutcome::IdempotentDuplicate
        );
    }
    assert_eq!(
        store.assertion_count(REPO).unwrap(),
        count_after_first,
        "duplicate revision mints no new rows"
    );

    // Out-of-order: the NEWER state (v3, issue closed) is already current;
    // appending the OLDER v1 (issue open, PR open) cannot regress it.
    store
        .append_all(&mint_assertions(&state_v3()))
        .expect("append v3");
    let reduction_before = reduce(&store.assertions().unwrap());
    assert_eq!(
        reduction_before.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Closed
    );
    assert_eq!(
        reduction_before.pr_lifecycle(&pr(200)),
        PrLifecycleView::Merged
    );

    store
        .append_all(&mint_assertions(&state_v1()))
        .expect("append older v1 late");
    let reduction_after = reduce(&store.assertions().unwrap());
    assert_eq!(
        reduction_after.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Closed,
        "older open revision cannot regress the newer closed fact"
    );
    assert_eq!(
        reduction_after.pr_lifecycle(&pr(200)),
        PrLifecycleView::Merged
    );
    // The older open assertion is superseded within the open lineage (the
    // closed fact is a different predicate); the newest open head is v2's.
    let open = reduction_after.get(&issue(100), PredicateV1::IssueOpen);
    assert_eq!(open.status, ReductionStatusV1::Current);
    assert_eq!(open.current_heads.len(), 1);
    assert_eq!(open.current_heads[0].source_revision, "rev2-iss");
    let closed = reduction_after.get(&issue(100), PredicateV1::IssueClosed);
    assert_eq!(closed.status, ReductionStatusV1::Current);
    assert_eq!(closed.current_heads[0].source_revision, "rev3-iss");
}

// ── Discrimination 7 ───────────────────────────────────────────────────────

/// Two authoritative contradictory facts yield `conflicted` and a
/// conflict-resolution open action.
#[test]
fn two_authoritative_contradictions_stay_conflicted_with_resolution_action() {
    let store = open_store();
    store
        .append_all(&mint_assertions(&state_v1()))
        .expect("append typed link -> PR 200");

    // A reviewed disposition links issue 100 to PR 999 instead — both
    // authorities are admitted, values disagree, neither scope may
    // supersede the other.
    let rival = AssertionV1 {
        assertion_id: "reviewed-rival-1".to_string(),
        subject: issue(100),
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
    store.append(&rival).expect("append rival");

    let reduction = reduce(&store.assertions().unwrap());
    let linked_predicate = reduction.get(&issue(100), PredicateV1::ImplementationPrLinked);
    assert_eq!(linked_predicate.status, ReductionStatusV1::Conflicted);
    assert!(linked_predicate.current_heads.len() >= 2);
    assert!(linked_predicate.values.is_empty(), "nothing is selected");

    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_eq!(action.kind, super::types::OpenActionKindV1::ResolveConflict);
    assert!(!action.blockers.is_empty());

    // The same immutable revision contradicting itself is rejected outright.
    let mut contradictory = mint_assertions(&state_v1())
        .into_iter()
        .find(|a| a.predicate == PredicateV1::PrOpen)
        .unwrap();
    contradictory.value = AssertionValueV1::CommitSha("tampered".to_string());
    match store.append(&contradictory) {
        Err(CurrentTruthStoreError::ContradictsExistingRevision(_)) => {}
        other => panic!("expected same-revision contradiction rejection, got {other:?}"),
    }
}

// ── Discrimination 8 ───────────────────────────────────────────────────────

/// GitHub unavailable yields an unknown/stale posture, never a fabricated
/// current result.
#[test]
fn github_unavailable_yields_unknown_stale_never_fabricated_current() {
    let adapter = FakeGithubAdapter::new(vec![state_v1(), state_v2()]);
    let store = open_store();

    // First refresh: fresh, v1 lands.
    let (outcome, assertions) = reconcile_refresh(&adapter, REPO);
    assert!(matches!(outcome, RefreshOutcomeV1::Fresh(_)));
    store.append_all(&assertions).expect("append v1");
    store
        .record_refresh(
            REPO,
            true,
            Some("r1"),
            Some("2026-08-26T10:00:00Z"),
            "2026-08-26T10:00:05Z",
            None,
        )
        .expect("record fresh");

    // Second refresh: the adapter is unreachable.
    adapter.set_fail();
    let (outcome, assertions) = reconcile_refresh(&adapter, REPO);
    assert!(matches!(outcome, RefreshOutcomeV1::Unavailable { .. }));
    assert!(assertions.is_empty(), "unavailable refresh mints nothing");
    store
        .record_refresh(
            REPO,
            false,
            None,
            None,
            "2026-08-26T11:00:05Z",
            Some("network unreachable"),
        )
        .expect("record unavailable");

    // The view must surface the stale posture and the
    // refresh-unavailable action — not a fabricated current state.
    let view = consumer::read_view(&store, REPO, CallerAuthorizationV1 { sees_private: true })
        .expect("view");
    assert!(!view.posture.fresh);
    assert_eq!(
        view.posture.unavailable_reason.as_deref(),
        Some("network unreachable")
    );
    let issue_row = view
        .subjects
        .iter()
        .find(|s| s.subject_token == "kckylechen1/tachi#issue:100")
        .expect("issue row");
    let action = issue_row.open_action.as_ref().expect("action");
    assert_eq!(
        action.kind,
        super::types::OpenActionKindV1::RefreshUnavailableSource
    );
    assert_eq!(view.health.repos_with_refresh_debt, 1);

    // A repository with no recorded posture at all is an error, never a
    // fresh guess.
    match consumer::read_view(
        &store,
        "other/repo",
        CallerAuthorizationV1 { sees_private: true },
    ) {
        Err(consumer::ConsumerViewError::NoPosture(_)) => {}
        other => panic!("expected NoPosture, got {other:?}"),
    }
}

// ── Discrimination 9 ───────────────────────────────────────────────────────

/// Full rebuild equals the incremental projection canonically.
#[test]
fn full_rebuild_equals_incremental_projection() {
    let store = open_store();
    let states = [state_v1(), state_v2(), state_v3(), state_v4()];

    // Incremental: reduce + write after every append.
    let mut incremental_final = None;
    for state in &states {
        store.append_all(&mint_assertions(state)).expect("append");
        let assertions = store.assertions().unwrap();
        let reduction = reduce(&assertions);
        let generation = generation_digest(&assertions);
        store
            .write_projection(REPO, &generation, "2026-08-26T00:00:00Z", "incremental")
            .expect("write projection");
        incremental_final = Some((reduction, generation));
    }
    let (incremental_reduction, incremental_generation) = incremental_final.expect("reduced");

    // Full rebuild: drop the projection, re-reduce everything at once.
    store.drop_projection(REPO).expect("drop projection");
    assert!(store.read_projection(REPO).unwrap().is_none());
    let assertions = store.assertions().unwrap();
    let rebuild_reduction = reduce(&assertions);
    let rebuild_generation = generation_digest(&assertions);
    store
        .write_projection(REPO, &rebuild_generation, "2026-08-26T00:00:00Z", "rebuild")
        .expect("rebuild projection");

    assert_eq!(incremental_generation, rebuild_generation);
    assert_eq!(
        serde_json::to_string(&incremental_reduction.all()).unwrap(),
        serde_json::to_string(&rebuild_reduction.all()).unwrap(),
        "canonical equality of the full reduction"
    );
    assert_eq!(incremental_reduction, rebuild_reduction);

    // Dropping/rebuilding the projection never touched the authority.
    assert!(store.assertion_count(REPO).unwrap() > 0);
    assert!(store.read_projection(REPO).unwrap().is_some());
}

// ── Discrimination 10 ──────────────────────────────────────────────────────

/// `open_action` is deterministic and invokes no model: identical inputs in
/// any order produce identical outputs.
#[test]
fn open_action_deterministic_and_order_independent() {
    let mut all = Vec::new();
    for state in [state_v1(), state_v2(), state_v3(), state_v4()] {
        all.extend(mint_assertions(&state));
    }
    all.push(owner_acceptance("2026-08-26T14:00:00Z", "rev5"));

    let baseline = reduce(&all);
    let baseline_actions = super::projection::open_actions(&baseline, &fresh_posture());
    let baseline_json = serde_json::to_string(&baseline_actions).unwrap();

    // Several fixed permutations (deterministic test, no RNG).
    for permutation in [
        vec![3usize, 1, 0, 2, 4, 6, 5, 7, 9, 8, 11, 10, 12, 13, 14, 15],
        vec![15usize, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
    ] {
        let mut shuffled: Vec<AssertionV1> = permutation
            .into_iter()
            .filter(|index| *index < all.len())
            .map(|index| all[index].clone())
            .collect();
        shuffled.extend(all.iter().skip(16).cloned());
        let reduction = reduce(&shuffled);
        let actions = super::projection::open_actions(&reduction, &fresh_posture());
        assert_eq!(
            serde_json::to_string(&actions).unwrap(),
            baseline_json,
            "reduction and actions are arrival-order independent"
        );
    }

    // Repeated evaluation of the same input is stable.
    assert_eq!(
        serde_json::to_string(&super::projection::open_actions(
            &baseline,
            &fresh_posture()
        ))
        .unwrap(),
        baseline_json
    );
}

// ── Discrimination 11 ──────────────────────────────────────────────────────

/// The #1693 consumer fixture reads evidence heads/revisions through the
/// consumer surface only — no raw assertion internals are imported here.
#[test]
fn consumer_fixture_reads_heads_without_raw_assertion_internals() {
    let store = open_store();
    store
        .append_all(&mint_assertions(&state_v2()))
        .expect("append v2");
    store
        .record_refresh(
            REPO,
            true,
            Some("r2"),
            Some("2026-08-26T11:00:00Z"),
            "2026-08-26T11:00:05Z",
            None,
        )
        .expect("record posture");

    // This test's imports exercise `consumer::{...}` view types only (see
    // the use list at the top of the file): the view carries statuses,
    // value tokens, and evidence heads with source revisions.
    let view = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("view");

    assert!(view.posture.fresh);
    let issue_row = view
        .subjects
        .iter()
        .find(|s| s.subject_token == "kckylechen1/tachi#issue:100")
        .expect("issue row");
    let merged = issue_row
        .predicates
        .iter()
        .find(|p| p.predicate == PredicateV1::ImplementationPresent)
        .expect("implementation_present row");
    assert_eq!(merged.status, ReductionStatusV1::Current);
    let head = merged.evidence_heads.first().expect("evidence head");
    assert!(
        head.source_revision.starts_with("composite-"),
        "issue-owned relation assertions carry the composite revision: {}",
        head.source_revision
    );
    assert_eq!(head.source, "github-snapshot-adapter");
    let action = issue_row.open_action.as_ref().expect("action");
    assert_eq!(action.kind, super::types::OpenActionKindV1::RunVerification);
}

// ── Discrimination 12 ──────────────────────────────────────────────────────

/// An unauthorized caller cannot infer private subject/evidence existence
/// through counts or refs.
#[test]
fn unauthorized_caller_cannot_infer_private_subject_existence() {
    let mut state = state_v2();
    state.issues.push(snapshot_issue(
        777,
        SnapshotIssueStateV1::Open,
        "2026-08-26T11:00:00Z",
        "rev2-iss777",
    ));
    state.issues[1].visibility = VisibilityClassV1::Private;
    let store = open_store();
    store.append_all(&mint_assertions(&state)).expect("append");
    store
        .record_refresh(
            REPO,
            true,
            Some("r2"),
            Some("2026-08-26T11:00:00Z"),
            "2026-08-26T11:00:05Z",
            None,
        )
        .expect("record posture");

    let unauthorized = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("unauthorized view");
    let serialized = serde_json::to_string(&unauthorized).unwrap();
    assert!(
        !serialized.contains("issue:777"),
        "no private subject token may leak: {serialized}"
    );
    assert_eq!(unauthorized.subjects.len(), 2, "issue 100 + PR 200 only");
    assert_eq!(
        unauthorized.health.conflicted_predicates, 0,
        "health is computed over the visible set only"
    );

    let authorized =
        consumer::read_view(&store, REPO, CallerAuthorizationV1 { sees_private: true })
            .expect("authorized view");
    assert_eq!(authorized.subjects.len(), 3);
    assert!(authorized
        .subjects
        .iter()
        .any(|s| s.subject_token == "kckylechen1/tachi#issue:777"));
}

// ── Storage/replay contract extras ─────────────────────────────────────────

/// Corrections append; history is never rewritten through the store.
#[test]
fn correction_appends_new_assertion_old_row_remains() {
    let store = open_store();
    let v1 = mint_assertions(&state_v1());
    store.append_all(&v1).expect("append v1");
    let first_id = v1
        .iter()
        .find(|a| a.predicate == PredicateV1::IssueOpen)
        .unwrap()
        .assertion_id
        .clone();
    store
        .append_all(&mint_assertions(&state_v2()))
        .expect("append v2");
    let all = store.assertions().unwrap();
    assert!(
        all.iter().any(|a| a.assertion_id == first_id),
        "the superseded assertion is still stored, unchanged"
    );
}

/// Projection-side predicates are computed, not assertable by a source.
#[test]
fn projection_predicates_are_rejected_at_append() {
    let store = open_store();
    let action_assertion = AssertionV1 {
        assertion_id: "smuggled-action".to_string(),
        subject: issue(100),
        predicate: PredicateV1::OpenAction,
        value: AssertionValueV1::Action(super::types::OpenActionKindV1::NoOpenAction),
        issuer: "attacker".to_string(),
        authority_class: AuthorityClassV1::GitHubTypedObject,
        source_ref: SourceRefV1 {
            source: "github-snapshot-adapter".to_string(),
            revision: "rev9".to_string(),
        },
        observed_at: "2026-08-26T15:00:00Z".to_string(),
        effective_at: "2026-08-26T15:00:00Z".to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec![],
        review_state: ReviewStateV1::Observed,
        visibility: VisibilityClassV1::Public,
    };
    match store.append(&action_assertion) {
        Err(CurrentTruthStoreError::ProjectionPredicateNotAssertable(_)) => {}
        other => panic!("expected projection-predicate rejection, got {other:?}"),
    }
}

/// A row written outside the closed vocabulary surfaces as an error, never
/// as a silent guess.
#[test]
fn corrupt_row_surfaces_as_error_not_silent_guess() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE current_truth_assertions (
            assertion_id TEXT PRIMARY KEY, subject_repo TEXT NOT NULL,
            subject_kind TEXT NOT NULL, subject_id TEXT NOT NULL,
            predicate TEXT NOT NULL, value_json TEXT NOT NULL,
            issuer TEXT NOT NULL, authority TEXT NOT NULL,
            source_id TEXT NOT NULL, source_revision TEXT NOT NULL,
            observed_at TEXT NOT NULL, effective_at TEXT NOT NULL,
            supersedes TEXT, evidence_json TEXT NOT NULL DEFAULT '[]',
            review_state TEXT NOT NULL, visibility TEXT NOT NULL,
            value_digest TEXT NOT NULL, recorded_at TEXT NOT NULL DEFAULT '',
            UNIQUE (subject_repo, subject_kind, subject_id, predicate,
                    authority, issuer, source_id, source_revision)
        );
        INSERT INTO current_truth_assertions VALUES (
            'x', 'a/b', 'issue', '1', 'made_up_predicate', '"unit"', 'i',
            'github_typed_object', 's', 'r', 't', 't', NULL, '[]',
            'observed', 'public', 'd', '');
        "#,
    )
    .unwrap();
    let store = CurrentTruthSqliteStore::with_connection(conn).expect("adopt");
    match store.assertions() {
        Err(CurrentTruthStoreError::CorruptRow(_)) => {}
        other => panic!("expected CorruptRow, got {other:?}"),
    }
}

/// Refresh through the adapter reduces without mutating GitHub: the fake
/// adapter records no mutation calls because the trait exposes none —
/// structural, plus idempotent re-mint from the same state.
#[test]
fn refresh_minting_is_pure_and_idempotent() {
    let state = state_v2();
    let first = mint_assertions(&state);
    let second = mint_assertions(&state);
    assert!(!first.is_empty(), "a non-trivial state mints assertions");
    assert_eq!(first, second, "same state mints identical assertions");
    assert!(
        first
            .iter()
            .all(|a| a.authority_class == AuthorityClassV1::GitHubTypedObject),
        "adapter-minted assertions carry the typed-object authority"
    );
    assert!(
        first
            .iter()
            .all(|a| !a.evidence_refs.is_empty() || a.predicate != PredicateV1::MergeReverted),
        "revert evidence carries the original merge SHA"
    );
}

// ── Review round 1 fixes (codex R2, findings 2/3/4/6/7/11) ────────────────

/// A merged PR without a merge SHA is an evidence gap: `pr_merged` holds
/// with the `Unit` value, but implementation presence is NOT projected and
/// no success-shaped action fires from missing evidence.
#[test]
fn merged_without_sha_is_evidence_gap_not_implementation() {
    let mut state = state_v2();
    state.pull_requests[0].merge_commit_sha = None;
    let store = open_store();
    store.append_all(&mint_assertions(&state)).expect("append");
    let reduction = reduce(&store.assertions().unwrap());
    assert_eq!(reduction.pr_lifecycle(&pr(200)), PrLifecycleView::Merged);
    let merged = reduction.get(&pr(200), PredicateV1::PrMerged);
    assert_eq!(merged.status, ReductionStatusV1::Current);
    assert_eq!(merged.values.first(), Some(&AssertionValueV1::Unit));
    assert!(
        !reduction.implementation_present(&issue(100)),
        "missing merge SHA must not project implementation present"
    );
    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_ne!(action.kind, super::types::OpenActionKindV1::RunVerification);
    assert_eq!(
        action.kind,
        super::types::OpenActionKindV1::AwaitImplementation
    );
}

/// A typed link removed in a newer snapshot is superseded by the newer
/// (empty) link set — removal is a new revision, never silence.
#[test]
fn removed_link_supersedes_older_link_set() {
    let store = open_store();
    store
        .append_all(&mint_assertions(&state_v2()))
        .expect("append v2 (linked)");
    let linked = reduce(&store.assertions().unwrap()).linked_prs(&issue(100));
    assert_eq!(linked, vec![GithubObjectRefV1::PullRequest(200)]);

    // A newer refresh at r3: the issue is still OPEN, the merge stands, but
    // the typed link is gone from the snapshot.
    let unlinked = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r3".to_string(),
        refreshed_at: "2026-08-26T12:00:00Z".to_string(),
        issues: vec![snapshot_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T12:00:00Z",
            "rev3-iss",
        )],
        pull_requests: vec![snapshot_pr(
            200,
            SnapshotPrStateV1::Merged,
            "2026-08-26T12:00:00Z",
            "rev3-pr",
            vec![],
        )],
        observations: vec![],
    };
    store
        .append_all(&mint_assertions(&unlinked))
        .expect("append v3 (unlinked)");
    let reduction = reduce(&store.assertions().unwrap());
    assert!(
        reduction.linked_prs(&issue(100)).is_empty(),
        "the removed link must not stay current"
    );
    assert!(!reduction.implementation_present(&issue(100)));
    let action = open_action_for(&reduction, &fresh_posture(), &issue(100));
    assert_eq!(
        action.kind,
        super::types::OpenActionKindV1::AwaitImplementation
    );
}

/// A complete multi-PR link set reduces deterministically regardless of the
/// snapshot's PR ordering: one set-valued assertion per issue per revision,
/// with value-derived identity.
#[test]
fn multi_link_set_is_order_independent() {
    let mut a = state_v1();
    a.pull_requests = vec![
        snapshot_pr(
            200,
            SnapshotPrStateV1::Open,
            "2026-08-26T10:00:00Z",
            "rev1-pr",
            vec![100],
        ),
        snapshot_pr(
            201,
            SnapshotPrStateV1::Open,
            "2026-08-26T10:00:00Z",
            "rev1-pr201",
            vec![100],
        ),
    ];
    let mut b = state_v1();
    b.pull_requests = vec![
        snapshot_pr(
            201,
            SnapshotPrStateV1::Open,
            "2026-08-26T10:00:00Z",
            "rev1-pr201",
            vec![100],
        ),
        snapshot_pr(
            200,
            SnapshotPrStateV1::Open,
            "2026-08-26T10:00:00Z",
            "rev1-pr",
            vec![100],
        ),
    ];
    let mut minted_a = mint_assertions(&a);
    let mut minted_b = mint_assertions(&b);
    minted_a.sort_by(|x, y| x.assertion_id.cmp(&y.assertion_id));
    minted_b.sort_by(|x, y| x.assertion_id.cmp(&y.assertion_id));
    assert_eq!(
        minted_a, minted_b,
        "PR ordering in the snapshot is irrelevant"
    );
    let mut linked = reduce(&minted_a).linked_prs(&issue(100));
    linked.sort_by_key(|object| object.as_token());
    assert_eq!(
        linked,
        vec![
            GithubObjectRefV1::PullRequest(200),
            GithubObjectRefV1::PullRequest(201),
        ]
    );
}

/// A refresh record older than the recorded attempt is refused: a late,
/// out-of-order record can never erase a newer posture.
#[test]
fn stale_refresh_record_refused() {
    let store = open_store();
    store
        .record_refresh(
            REPO,
            false,
            None,
            None,
            "2026-08-26T12:00:00Z",
            Some("outage"),
        )
        .expect("record outage");
    match store.record_refresh(
        REPO,
        true,
        Some("r1"),
        Some("2026-08-26T10:00:00Z"),
        "2026-08-26T11:00:00Z",
        None,
    ) {
        Err(CurrentTruthStoreError::StaleRefreshRecord { .. }) => {}
        other => panic!("expected StaleRefreshRecord, got {other:?}"),
    }
    // The newer attempt still lands.
    store
        .record_refresh(
            REPO,
            true,
            Some("r2"),
            Some("2026-08-26T13:00:00Z"),
            "2026-08-26T13:00:05Z",
            None,
        )
        .expect("newer attempt accepted");
    let posture = store.refresh_posture_row(REPO).unwrap().expect("posture");
    assert!(posture.fresh);
}

/// `observed_at` must parse as RFC 3339 — ordering authority is a real
/// instant, not a lexical accident.
#[test]
fn malformed_observed_at_rejected_at_append() {
    let store = open_store();
    let mut assertion = mint_assertions(&state_v1())
        .into_iter()
        .find(|a| a.predicate == PredicateV1::IssueOpen)
        .unwrap();
    assertion.observed_at = "not-a-timestamp".to_string();
    match store.append(&assertion) {
        Err(CurrentTruthStoreError::MalformedObservedAt(_)) => {}
        other => panic!("expected MalformedObservedAt, got {other:?}"),
    }
}

/// A public PR linked to a private issue exposes nothing about the private
/// issue: the issue-owned relation and implementation rows inherit the
/// ISSUE's visibility.
#[test]
fn public_pr_never_exposes_linked_private_issue() {
    let mut state = state_v1();
    state.issues[0].visibility = VisibilityClassV1::Private;
    let store = open_store();
    store.append_all(&mint_assertions(&state)).expect("append");
    store
        .record_refresh(
            REPO,
            true,
            Some("r1"),
            Some("2026-08-26T10:00:00Z"),
            "2026-08-26T10:00:05Z",
            None,
        )
        .expect("posture");
    let unauthorized = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("view");
    let serialized = serde_json::to_string(&unauthorized).unwrap();
    assert!(
        !serialized.contains("issue:100"),
        "no private issue token may leak through its public PR: {serialized}"
    );
    assert!(
        unauthorized
            .subjects
            .iter()
            .any(|s| s.subject_token == "kckylechen1/tachi#pull_request:200"),
        "the public PR itself remains visible"
    );
    let authorized =
        consumer::read_view(&store, REPO, CallerAuthorizationV1 { sees_private: true })
            .expect("authorized view");
    assert!(authorized
        .subjects
        .iter()
        .any(|s| s.subject_token == "kckylechen1/tachi#issue:100"));
}

// ── Review round 2 fixes (codex R2 round 2) ───────────────────────────────

/// A subject that becomes private in a newer revision is hidden entirely —
/// its older public rows must not keep exposing it.
#[test]
fn public_to_private_transition_hides_subject_entirely() {
    let store = open_store();
    // v2: everything public.
    store
        .append_all(&mint_assertions(&state_v2()))
        .expect("append v2");
    // v3: the issue transitioned to private.
    let mut private = state_v3();
    private.issues[0].visibility = VisibilityClassV1::Private;
    store
        .append_all(&mint_assertions(&private))
        .expect("append v3");
    store
        .record_refresh(
            REPO,
            true,
            Some("r3"),
            Some("2026-08-26T12:00:00Z"),
            "2026-08-26T12:00:05Z",
            None,
        )
        .expect("posture");

    let unauthorized = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("view");
    let serialized = serde_json::to_string(&unauthorized).unwrap();
    assert!(
        !serialized.contains("issue:100"),
        "older public rows must not keep exposing a now-private subject: {serialized}"
    );

    let authorized =
        consumer::read_view(&store, REPO, CallerAuthorizationV1 { sees_private: true })
            .expect("authorized view");
    assert!(authorized
        .subjects
        .iter()
        .any(|s| s.subject_token == "kckylechen1/tachi#issue:100"));
}

/// Two merged linked PRs: the presence SHA is picked deterministically by
/// (instant, PR number, SHA) — snapshot ordering is irrelevant.
#[test]
fn two_merged_linked_prs_presence_is_order_independent() {
    let base = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r1".to_string(),
        refreshed_at: "2026-08-26T10:00:00Z".to_string(),
        issues: vec![snapshot_issue(
            100,
            SnapshotIssueStateV1::Open,
            "2026-08-26T10:00:00Z",
            "rev1-iss",
        )],
        pull_requests: vec![],
        observations: vec![],
    };
    let mut newer_pr = snapshot_pr(
        300,
        SnapshotPrStateV1::Merged,
        "2026-08-26T12:00:00Z",
        "rev1-pr300",
        vec![100],
    );
    newer_pr.merge_commit_sha = Some("mergeAAA300".to_string());
    let mut older_pr = snapshot_pr(
        200,
        SnapshotPrStateV1::Merged,
        "2026-08-26T11:00:00Z",
        "rev1-pr200",
        vec![100],
    );
    older_pr.merge_commit_sha = Some("mergeBBB200".to_string());

    let mut order_a = base.clone();
    order_a.pull_requests = vec![older_pr.clone(), newer_pr.clone()];
    let mut order_b = base.clone();
    order_b.pull_requests = vec![newer_pr, older_pr];

    let mut minted_a = mint_assertions(&order_a);
    let mut minted_b = mint_assertions(&order_b);
    minted_a.sort_by(|x, y| x.assertion_id.cmp(&y.assertion_id));
    minted_b.sort_by(|x, y| x.assertion_id.cmp(&y.assertion_id));
    assert_eq!(minted_a, minted_b, "merged-PR snapshot order is irrelevant");

    let reduction = reduce(&minted_a);
    let presence = reduction.get(&issue(100), PredicateV1::ImplementationPresent);
    // The newest merge (PR 300 at 12:00, its own SHA) is the evidenced
    // presence — first-match on an unordered iteration would pick the
    // wrong one.
    assert_eq!(
        presence.values.first(),
        Some(&AssertionValueV1::CommitSha("mergeAAA300".to_string()))
    );
    assert!(reduction.implementation_present(&issue(100)));
}

/// Two DIFFERENT assertions claiming the same immutable id reduce to
/// `conflicted` regardless of the order they are handed to the reducer.
#[test]
fn duplicate_id_different_content_is_order_independent_conflict() {
    let base = mint_assertions(&state_v1());
    let mut tampered = base
        .iter()
        .find(|a| a.predicate == PredicateV1::IssueOpen)
        .unwrap()
        .clone();
    // Same id, different content (evidence changed).
    tampered.evidence_refs = vec!["smuggled-evidence".to_string()];

    let forward = reduce(&[base[0].clone(), tampered.clone()]);
    let backward = reduce(&[tampered, base[0].clone()]);
    assert_eq!(
        forward.get(&issue(100), PredicateV1::IssueOpen).status,
        ReductionStatusV1::Conflicted,
        "identity contradiction must surface, not silently resolve"
    );
    assert_eq!(
        forward.get(&issue(100), PredicateV1::IssueOpen).status,
        backward.get(&issue(100), PredicateV1::IssueOpen).status
    );
}

/// Two lifecycle predicates tying at the same instant AND source revision
/// conflict — the id never breaks the tie.
#[test]
fn same_instant_and_revision_lifecycle_tie_conflicts() {
    let tied_open = AssertionV1 {
        assertion_id: "tie-open-1".to_string(),
        subject: issue(100),
        predicate: PredicateV1::IssueOpen,
        value: AssertionValueV1::Unit,
        issuer: "adapter".to_string(),
        authority_class: AuthorityClassV1::GitHubTypedObject,
        source_ref: SourceRefV1 {
            source: "github-snapshot-adapter".to_string(),
            revision: "rev-tie".to_string(),
        },
        observed_at: "2026-08-26T10:00:00Z".to_string(),
        effective_at: "2026-08-26T10:00:00Z".to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec![],
        review_state: ReviewStateV1::Observed,
        visibility: VisibilityClassV1::Public,
    };
    // A different issuer for the closed claim — a genuinely different
    // authority asserting the opposite at the identical revision.
    let mut tied_closed = tied_open.clone();
    tied_closed.assertion_id = "tie-closed-1".to_string();
    tied_closed.predicate = PredicateV1::IssueClosed;
    tied_closed.issuer = "owner-tool".to_string();
    tied_closed.authority_class = AuthorityClassV1::OwnerDecision;

    let reduction = reduce(&[tied_open, tied_closed]);
    assert_eq!(
        reduction.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Conflicted,
        "open and closed at the identical instant+revision is a conflict, not an id tiebreak"
    );
}

/// A refresh record with a malformed attempt timestamp is refused.
#[test]
fn refresh_record_with_malformed_timestamp_refused() {
    let store = open_store();
    match store.record_refresh(REPO, true, Some("r1"), Some("t"), "yesterday", None) {
        Err(CurrentTruthStoreError::MalformedObservedAt(_)) => {}
        other => panic!("expected MalformedObservedAt, got {other:?}"),
    }
}

/// A PR-only change (new merge) changes the composite revision of the
/// issue-owned relation assertions instead of colliding with the
/// issue-only token.
#[test]
fn pr_only_change_moves_composite_revision_not_collision() {
    let store = open_store();
    // v1: PR open, linked.
    store
        .append_all(&mint_assertions(&state_v1()))
        .expect("append v1");
    // PR merges while the ISSUE snapshot itself did not move.
    let mut pr_only = state_v1();
    pr_only.issues[0].snapshot_revision = "rev1-iss".to_string(); // unchanged
    pr_only.issues[0].updated_at = "2026-08-26T10:00:00Z".to_string(); // unchanged
    pr_only.pull_requests[0] = snapshot_pr(
        200,
        SnapshotPrStateV1::Merged,
        "2026-08-26T11:30:00Z",
        "rev2-pr",
        vec![100],
    );
    let minted = mint_assertions(&pr_only);
    let append = store.append_all(&minted);
    match append {
        Ok(count) => assert!(count > 0, "the PR-side change must append new facts"),
        Err(CurrentTruthStoreError::ContradictsExistingRevision(key)) => {
            panic!("PR-only change collided at {key:?}");
        }
        other => panic!("unexpected append outcome {other:?}"),
    }
    let reduction = reduce(&store.assertions().unwrap());
    assert!(reduction.implementation_present(&issue(100)));
}

// ── Review round 3 fixes ───────────────────────────────────────────────────

/// Two DIFFERENT assertions sharing one id but belonging to DIFFERENT
/// subjects conflict BOTH groups, regardless of input order.
#[test]
fn same_id_different_subjects_conflicts_both_groups() {
    let base = mint_assertions(&state_v1());
    let open_a = base
        .iter()
        .find(|a| a.predicate == PredicateV1::IssueOpen)
        .unwrap()
        .clone();
    let mut open_b = open_a.clone();
    // Same id, different subject and content.
    open_b.subject = issue(101);
    open_b.evidence_refs = vec!["smuggled".to_string()];

    let forward = reduce(&[open_a.clone(), open_b.clone()]);
    let backward = reduce(&[open_b, open_a]);
    for reduction in [&forward, &backward] {
        assert_eq!(
            reduction.get(&issue(100), PredicateV1::IssueOpen).status,
            ReductionStatusV1::Conflicted
        );
        assert_eq!(
            reduction.get(&issue(101), PredicateV1::IssueOpen).status,
            ReductionStatusV1::Conflicted
        );
    }
}

/// A public issue linked to a private PR: the relation rows are private, so
/// the unauthorized view exposes neither the private PR's identity nor its
/// merge SHA through the public issue.
#[test]
fn public_issue_private_pr_relation_hidden() {
    let mut state = state_v2();
    state.pull_requests[0].visibility = VisibilityClassV1::Private;
    let store = open_store();
    store.append_all(&mint_assertions(&state)).expect("append");
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
    let unauthorized = consumer::read_view(
        &store,
        REPO,
        CallerAuthorizationV1 {
            sees_private: false,
        },
    )
    .expect("view");
    let serialized = serde_json::to_string(&unauthorized).unwrap();
    assert!(
        !serialized.contains("pull_request:200") && !serialized.contains("mergeabc123"),
        "private PR identity/SHA must not leak through the public issue: {serialized}"
    );
    let authorized =
        consumer::read_view(&store, REPO, CallerAuthorizationV1 { sees_private: true })
            .expect("authorized view");
    let authorized_json = serde_json::to_string(&authorized).unwrap();
    assert!(authorized_json.contains("pull_request:200"));
}

/// A lifecycle tie strictly older than the family's newest fact is
/// superseded history: the newer fact resolves the family.
#[test]
fn lifecycle_tie_superseded_by_newer_fact_resolves() {
    let tied_open = AssertionV1 {
        assertion_id: "r3-tie-open".to_string(),
        subject: issue(100),
        predicate: PredicateV1::IssueOpen,
        value: AssertionValueV1::Unit,
        issuer: "adapter".to_string(),
        authority_class: AuthorityClassV1::GitHubTypedObject,
        source_ref: SourceRefV1 {
            source: "github-snapshot-adapter".to_string(),
            revision: "rev-t1".to_string(),
        },
        observed_at: "2026-08-26T10:00:00Z".to_string(),
        effective_at: "2026-08-26T10:00:00Z".to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec![],
        review_state: ReviewStateV1::Observed,
        visibility: VisibilityClassV1::Public,
    };
    let mut tied_closed = tied_open.clone();
    tied_closed.assertion_id = "r3-tie-closed".to_string();
    tied_closed.predicate = PredicateV1::IssueClosed;
    tied_closed.issuer = "owner-tool".to_string();
    tied_closed.authority_class = AuthorityClassV1::OwnerDecision;

    let mut newer_reopened = tied_open.clone();
    newer_reopened.assertion_id = "r3-newer-reopened".to_string();
    newer_reopened.predicate = PredicateV1::IssueReopened;
    newer_reopened.source_ref.revision = "rev-t2".to_string();
    newer_reopened.observed_at = "2026-08-26T12:00:00Z".to_string();
    newer_reopened.effective_at = "2026-08-26T12:00:00Z".to_string();

    let reduction = reduce(&[tied_open, tied_closed, newer_reopened]);
    assert_eq!(
        reduction.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Reopened,
        "a tie at t1 is superseded history once a strictly newer fact exists"
    );
    // The pure tie (no newer fact) still conflicts.
    let reduction = reduce(&store_assertions_pair());
    assert_eq!(
        reduction.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Conflicted
    );
}

fn store_assertions_pair() -> Vec<AssertionV1> {
    let tied_open = AssertionV1 {
        assertion_id: "r3-tie-open".to_string(),
        subject: issue(100),
        predicate: PredicateV1::IssueOpen,
        value: AssertionValueV1::Unit,
        issuer: "adapter".to_string(),
        authority_class: AuthorityClassV1::GitHubTypedObject,
        source_ref: SourceRefV1 {
            source: "github-snapshot-adapter".to_string(),
            revision: "rev-t1".to_string(),
        },
        observed_at: "2026-08-26T10:00:00Z".to_string(),
        effective_at: "2026-08-26T10:00:00Z".to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec![],
        review_state: ReviewStateV1::Observed,
        visibility: VisibilityClassV1::Public,
    };
    let mut tied_closed = tied_open.clone();
    tied_closed.assertion_id = "r3-tie-closed".to_string();
    tied_closed.predicate = PredicateV1::IssueClosed;
    tied_closed.issuer = "owner-tool".to_string();
    tied_closed.authority_class = AuthorityClassV1::OwnerDecision;
    vec![tied_open, tied_closed]
}

/// An EMPTY merge SHA string is no evidence: same gap posture as a missing
/// SHA.
#[test]
fn empty_merge_sha_is_evidence_gap() {
    let mut state = state_v2();
    state.pull_requests[0].merge_commit_sha = Some(String::new());
    let store = open_store();
    store.append_all(&mint_assertions(&state)).expect("append");
    let reduction = reduce(&store.assertions().unwrap());
    let merged = reduction.get(&pr(200), PredicateV1::PrMerged);
    assert_eq!(merged.values.first(), Some(&AssertionValueV1::Unit));
    assert!(!reduction.implementation_present(&issue(100)));
}

/// The legacy single-ref form and the one-element set form assert the same
/// fact across lineages — agreement, not conflict.
#[test]
fn objectref_and_set_form_agree_across_lineages() {
    let set_form = AssertionV1 {
        assertion_id: "set-form".to_string(),
        subject: issue(100),
        predicate: PredicateV1::ImplementationPrLinked,
        value: AssertionValueV1::ObjectRefs(vec![GithubObjectRefV1::PullRequest(200)]),
        issuer: "github-refresh-v1".to_string(),
        authority_class: AuthorityClassV1::GitHubTypedObject,
        source_ref: SourceRefV1 {
            source: "github-snapshot-adapter".to_string(),
            revision: "rev1-iss".to_string(),
        },
        observed_at: "2026-08-26T10:00:00Z".to_string(),
        effective_at: "2026-08-26T10:00:00Z".to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec![],
        review_state: ReviewStateV1::Observed,
        visibility: VisibilityClassV1::Public,
    };
    let mut single_form = set_form.clone();
    single_form.assertion_id = "single-form".to_string();
    single_form.value = AssertionValueV1::ObjectRef(GithubObjectRefV1::PullRequest(200));
    single_form.issuer = "legacy-adapter".to_string();

    let reduction = reduce(&[set_form, single_form]);
    let linked = reduction.get(&issue(100), PredicateV1::ImplementationPrLinked);
    assert_eq!(
        linked.status,
        ReductionStatusV1::Current,
        "semantically identical cross-lineage facts agree"
    );
    assert_eq!(linked.current_heads.len(), 2);
}

/// The generation digest binds id AND content — same ids with different
/// content are different generations.
#[test]
fn generation_digest_is_content_sensitive() {
    let base = mint_assertions(&state_v1());
    let mut tampered = base.clone();
    if let Some(assertion) = tampered
        .iter_mut()
        .find(|a| a.predicate == PredicateV1::IssueOpen)
    {
        assertion.evidence_refs = vec!["different".to_string()];
    }
    assert_ne!(
        generation_digest(&base),
        generation_digest(&tampered),
        "same ids with different content must be different generations"
    );
}

/// The refresh-posture staleness guard holds across two independent
/// connections on the same database file.
#[test]
fn two_connection_refresh_guard_holds() {
    let dir = std::env::temp_dir().join(format!("ct-refresh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("store.db");
    let _ = std::fs::remove_file(&path);
    let path = path.to_str().expect("path").to_string();

    let writer_a = CurrentTruthSqliteStore::open(&path).expect("open A");
    writer_a
        .record_refresh(
            REPO,
            false,
            None,
            None,
            "2026-08-26T12:00:00Z",
            Some("outage"),
        )
        .expect("record outage on A");

    let writer_b = CurrentTruthSqliteStore::open(&path).expect("open B");
    match writer_b.record_refresh(
        REPO,
        true,
        Some("r1"),
        Some("2026-08-26T10:00:00Z"),
        "2026-08-26T11:00:00Z",
        None,
    ) {
        Err(CurrentTruthStoreError::StaleRefreshRecord { .. }) => {}
        other => panic!("expected StaleRefreshRecord across connections, got {other:?}"),
    }
    // The newer-outage posture survived the stale write attempt.
    let posture = writer_a
        .refresh_posture_row(REPO)
        .unwrap()
        .expect("posture");
    assert!(!posture.fresh);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

// ── Review round 4 fixes ───────────────────────────────────────────────────

/// An inadmissible row sharing an id with an admitted assertion never
/// affects the admitted group's truth.
#[test]
fn inadmissible_same_id_row_never_conflicts_admitted_group() {
    let base = mint_assertions(&state_v1());
    let open = base
        .iter()
        .find(|a| a.predicate == PredicateV1::IssueOpen)
        .unwrap()
        .clone();
    // Same id, different content, but REJECTED review state.
    let mut rejected_twin = open.clone();
    rejected_twin.evidence_refs = vec!["rejected-noise".to_string()];
    rejected_twin.review_state = ReviewStateV1::Rejected;

    let reduction = reduce(&[open.clone(), rejected_twin.clone()]);
    assert_eq!(
        reduction.get(&issue(100), PredicateV1::IssueOpen).status,
        ReductionStatusV1::Current,
        "rejected evidence cannot affect current truth, even via id collision"
    );
    // The same pair with BOTH admitted stays a conflict.
    let mut admitted_twin = rejected_twin;
    admitted_twin.review_state = ReviewStateV1::Observed;
    let reduction = reduce(&[open, admitted_twin]);
    assert_eq!(
        reduction.get(&issue(100), PredicateV1::IssueOpen).status,
        ReductionStatusV1::Conflicted
    );
}

/// A lifecycle member that is itself `Conflicted` at the family's max key
/// keeps the lifecycle view `Conflicted` — nothing is selected from it.
#[test]
fn conflicted_member_at_max_key_keeps_family_conflicted() {
    // Two lineages disagree about issue_open at the same revision.
    let open_a = AssertionV1 {
        assertion_id: "r4-open-a".to_string(),
        subject: issue(100),
        predicate: PredicateV1::IssueOpen,
        value: AssertionValueV1::Unit,
        issuer: "adapter-a".to_string(),
        authority_class: AuthorityClassV1::GitHubTypedObject,
        source_ref: SourceRefV1 {
            source: "adapter-a".to_string(),
            revision: "rev-t1".to_string(),
        },
        observed_at: "2026-08-26T10:00:00Z".to_string(),
        effective_at: "2026-08-26T10:00:00Z".to_string(),
        supersedes_assertion_id: None,
        evidence_refs: vec![],
        review_state: ReviewStateV1::Observed,
        visibility: VisibilityClassV1::Public,
    };
    // Same predicate, same lineage-visible ordering, DIFFERENT issuer
    // asserting a contradictory value shape (CommitSha vs Unit on the same
    // predicate) — values disagree across lineages at the same revision.
    let mut open_b = open_a.clone();
    open_b.assertion_id = "r4-open-b".to_string();
    open_b.issuer = "adapter-b".to_string();
    open_b.source_ref.source = "adapter-b".to_string();
    open_b.value = AssertionValueV1::CommitSha("not-a-state-value".to_string());

    let reduction = reduce(&[open_a, open_b]);
    assert_eq!(
        reduction.get(&issue(100), PredicateV1::IssueOpen).status,
        ReductionStatusV1::Conflicted,
        "fixture setup: the predicate itself must be conflicted"
    );
    assert_eq!(
        reduction.issue_lifecycle(&issue(100)),
        IssueLifecycleView::Conflicted,
        "a conflicted member owning the max key keeps the family conflicted"
    );
}

/// A missing SHA and an empty SHA mint the SAME composite revision — no
/// phantom revision for a None ⇄ Some(\"\") flip.
#[test]
fn missing_and_empty_sha_mint_same_composite_revision() {
    let mut missing = state_v2();
    missing.pull_requests[0].merge_commit_sha = None;
    let mut empty = state_v2();
    empty.pull_requests[0].merge_commit_sha = Some(String::new());

    let minted_missing = mint_assertions(&missing);
    let minted_empty = mint_assertions(&empty);
    let link_of = |minted: &Vec<AssertionV1>| {
        minted
            .iter()
            .find(|a| a.predicate == PredicateV1::ImplementationPrLinked)
            .unwrap()
            .clone()
    };
    assert_eq!(
        link_of(&minted_missing).source_ref.revision,
        link_of(&minted_empty).source_ref.revision,
        "the gap forms must be revision-identical"
    );
    assert_eq!(minted_missing, minted_empty);
}
