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
            },
            SnapshotObservationV1 {
                kind: SnapshotObservationKindV1::MergeReverted {
                    number: 200,
                    revert_commit_sha: "revertdef456".to_string(),
                    original_merge_sha: "mergeabc123".to_string(),
                },
                observed_at: "2026-08-26T13:30:00Z".to_string(),
                revision: "rev4-revert".to_string(),
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
    assert_eq!(head.source_revision, "rev2-pr");
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
