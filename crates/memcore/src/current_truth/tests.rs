//! Discrimination fixtures for the #1297 fold.
//!
//! These are written as *discrimination* tests, not coverage: each one names a
//! wrong answer that a plausible implementation would give, and fails on it.
//! The wrong answers being excluded, in order of how badly they would hurt:
//!
//! - `owner_closed = unknown` rendering as "done" because the PR merged.
//! - A model's confident claim moving a truth value, or inventing a subject.
//! - A conflict being resolved by recency, by authority ladder, or by id order.
//! - `open → closed → reopened` reading as a conflict.
//! - A revert being handled by rewriting the merge event.
//! - Arrival order changing the output.

use serde_json::json;

use crate::types::{AuthorityLevel, EffectScope, TachiEventRecord};

use super::action_queue::{derive_action_queue, ActionKindV1};
use super::admission::{
    build_truth_assertion_event, classify_assertion, decode_truth_assertion,
    truth_assertion_event_id, TruthAssertionInputV1, AGENT_AUTHORITY_CAP,
};
use super::reduce::{reduce_current_truth, CurrentTruthFold};
use super::types::{
    AssertionRelationV1, CandidateReasonV1, CurrentTruthError, CurrentTruthProjectionV1,
    TruthIssuerV1, TruthPredicate, TruthStateV1, TRUTH_ASSERTION_DOMAIN,
    TRUTH_ASSERTION_EVENT_TYPE, VALUE_ISSUE_CLOSED, VALUE_ISSUE_OPEN, VALUE_NO, VALUE_YES,
};

const PROJECT: &str = "tachi";
const AS_OF: &str = "2026-07-26T00:00:00Z";
const ISSUE: &str = "github:kckylechen1/tachi#1297";

// ── fixture builders ────────────────────────────────────────────────────────

fn base_input(
    subject: &str,
    predicate: TruthPredicate,
    value: Option<&str>,
    issuer: TruthIssuerV1,
    observed_at: &str,
    revision: &str,
) -> TruthAssertionInputV1 {
    TruthAssertionInputV1 {
        project: PROJECT.to_string(),
        source_repo: PROJECT.to_string(),
        adapter: String::new(),
        session_id: String::new(),
        actor: "test".to_string(),
        subject_ref: subject.to_string(),
        predicate,
        value: value.map(str::to_string),
        issuer,
        source_revision: revision.to_string(),
        observed_at: observed_at.to_string(),
        evidence_ref: format!("{subject}@{revision}"),
        relation: AssertionRelationV1::Standalone,
        authority: AuthorityLevel::RawFact,
    }
}

fn build(input: &TruthAssertionInputV1) -> TachiEventRecord {
    build_truth_assertion_event(input)
        .unwrap_or_else(|e| panic!("fixture failed to build: {e}"))
        .event
}

/// A GitHub-style source snapshot. The default admitted shape.
fn snapshot(
    subject: &str,
    predicate: TruthPredicate,
    value: &str,
    observed_at: &str,
    revision: &str,
) -> TachiEventRecord {
    build(&base_input(
        subject,
        predicate,
        Some(value),
        TruthIssuerV1::SourceSnapshot {
            system: "github".to_string(),
            snapshot_digest: format!("sha256:{revision}"),
        },
        observed_at,
        revision,
    ))
}

/// A named owner's decision. The only admitted issuer for `accepted` and
/// `owner_closed`.
fn owner_decision(
    subject: &str,
    predicate: TruthPredicate,
    value: &str,
    login: &str,
    observed_at: &str,
    revision: &str,
) -> TachiEventRecord {
    let mut input = base_input(
        subject,
        predicate,
        Some(value),
        TruthIssuerV1::OwnerDecision {
            owner_login: login.to_string(),
        },
        observed_at,
        revision,
    );
    input.authority = AuthorityLevel::ExecutionGate;
    build(&input)
}

/// An agent-authored claim, however confident. Never admitted.
fn agent_claim(
    subject: &str,
    predicate: TruthPredicate,
    value: &str,
    agent: &str,
    observed_at: &str,
    revision: &str,
) -> TachiEventRecord {
    let mut input = base_input(
        subject,
        predicate,
        Some(value),
        TruthIssuerV1::Agent {
            agent: agent.to_string(),
        },
        observed_at,
        revision,
    );
    // Ask for the maximum. The cap must hold anyway.
    input.authority = AuthorityLevel::ExecutionGate;
    build(&input)
}

fn project(events: &[TachiEventRecord]) -> CurrentTruthProjectionV1 {
    reduce_current_truth(events, AS_OF).expect("projection")
}

fn state_of(
    projection: &CurrentTruthProjectionV1,
    subject: &str,
    predicate: TruthPredicate,
) -> TruthStateV1 {
    projection
        .subject(subject)
        .unwrap_or_else(|| panic!("subject {subject} missing from projection"))
        .state(predicate)
        .unwrap_or_else(|| panic!("predicate {predicate} missing from subject {subject}"))
        .clone()
}

fn current_str(
    projection: &CurrentTruthProjectionV1,
    subject: &str,
    predicate: TruthPredicate,
) -> String {
    state_of(projection, subject, predicate)
        .current_value()
        .unwrap_or_else(|| panic!("{predicate} on {subject} is not `current`"))
        .as_str()
        .to_string()
}

/// The merged-PR / open-issue world the issue's headline equation describes.
fn merged_but_open_fixture() -> Vec<TachiEventRecord> {
    vec![
        snapshot(
            ISSUE,
            TruthPredicate::IssueState,
            VALUE_ISSUE_OPEN,
            "2026-07-20T10:00:00Z",
            "rev-a",
        ),
        snapshot(
            ISSUE,
            TruthPredicate::OwnerProtected,
            VALUE_YES,
            "2026-07-20T10:00:00Z",
            "rev-a",
        ),
        snapshot(
            ISSUE,
            TruthPredicate::ImplementedBy,
            "github:kckylechen1/tachi#1444",
            "2026-07-21T09:00:00Z",
            "rev-b",
        ),
        snapshot(
            ISSUE,
            TruthPredicate::Merged,
            "27499b706",
            "2026-07-21T09:30:00Z",
            "rev-b",
        ),
    ]
}

// ── acceptance: merged PR, open issue ───────────────────────────────────────

#[test]
fn merged_pr_with_open_issue_leaves_owner_closed_unknown() {
    let projection = project(&merged_but_open_fixture());

    assert_eq!(
        current_str(&projection, ISSUE, TruthPredicate::ImplementedBy),
        "github:kckylechen1/tachi#1444"
    );
    assert_eq!(
        current_str(&projection, ISSUE, TruthPredicate::Merged),
        "27499b706"
    );
    assert_eq!(
        current_str(&projection, ISSUE, TruthPredicate::IssueState),
        VALUE_ISSUE_OPEN
    );

    // The whole point. Merged is not closed, and closed is not owner-closed.
    assert!(
        state_of(&projection, ISSUE, TruthPredicate::OwnerClosed).is_unknown(),
        "a merged PR must not manufacture an owner close"
    );
    assert!(state_of(&projection, ISSUE, TruthPredicate::Accepted).is_unknown());
    assert!(state_of(&projection, ISSUE, TruthPredicate::Deployed).is_unknown());
    assert_eq!(projection.stats.unknown_owner_closed_subjects, 1);
    assert_eq!(projection.stats.subjects, 1);
    assert_eq!(projection.stats.admitted_assertions, 4);
    assert_eq!(projection.stats.conflicted_predicates, 0);
}

#[test]
fn every_subject_emits_every_predicate_so_unknown_is_never_a_missing_key() {
    let projection = project(&merged_but_open_fixture());
    let subject = projection.subject(ISSUE).expect("subject");
    assert_eq!(subject.predicates.len(), TruthPredicate::ALL.len());
    let order: Vec<TruthPredicate> = subject.predicates.iter().map(|p| p.predicate).collect();
    assert_eq!(order, TruthPredicate::ALL);
}

// ── acceptance: revert ──────────────────────────────────────────────────────

#[test]
fn revert_changes_truth_without_rewriting_the_merge_event() {
    let merge = snapshot(
        ISSUE,
        TruthPredicate::Merged,
        "27499b706",
        "2026-07-21T09:30:00Z",
        "rev-b",
    );
    let revert = snapshot(
        ISSUE,
        TruthPredicate::Merged,
        VALUE_NO,
        "2026-07-22T11:00:00Z",
        "rev-c",
    );
    let merge_before = merge.clone();

    let projection = project(&[merge.clone(), revert.clone()]);

    assert_eq!(
        current_str(&projection, ISSUE, TruthPredicate::Merged),
        VALUE_NO
    );

    // The displaced merge observation is still emitted as history, and the
    // ledger event behind it is byte-identical to what was appended.
    let merged_entry = projection
        .subject(ISSUE)
        .unwrap()
        .predicate(TruthPredicate::Merged)
        .unwrap();
    assert_eq!(merged_entry.displaced.len(), 1);
    assert_eq!(merged_entry.displaced[0].assertion_id, merge.id);
    assert_eq!(
        merged_entry.displaced[0].value.as_ref().unwrap().as_str(),
        "27499b706"
    );
    assert!(merged_entry.superseded.is_empty());
    assert!(merged_entry.retracted.is_empty());

    assert_eq!(merge, merge_before, "the merge event must not be rewritten");
    assert_eq!(
        decode_truth_assertion(&merge)
            .unwrap()
            .value
            .unwrap()
            .as_str(),
        "27499b706"
    );
}

// ── acceptance: an agent asserts a wrong relation ───────────────────────────

#[test]
fn agent_assertion_stays_candidate_and_leaves_the_truth_surface_byte_identical() {
    let clean = merged_but_open_fixture();
    let before = project(&clean).canonical_subjects_json().unwrap();

    let mut poisoned = clean.clone();
    // A confident, well-cited, wrong claim: the issue is owner-closed and the
    // merge is a different SHA.
    poisoned.push(agent_claim(
        ISSUE,
        TruthPredicate::Merged,
        "deadbeef",
        "some-model",
        "2026-07-25T12:00:00Z",
        "rev-z",
    ));
    poisoned.push(agent_claim(
        ISSUE,
        TruthPredicate::OwnerClosed,
        VALUE_YES,
        "some-model",
        "2026-07-25T12:00:01Z",
        "rev-z",
    ));

    let after_projection = project(&poisoned);
    let after = after_projection.canonical_subjects_json().unwrap();

    assert_eq!(before, after, "an agent claim moved the truth surface");
    assert!(state_of(&after_projection, ISSUE, TruthPredicate::OwnerClosed).is_unknown());
    assert_eq!(after_projection.diagnostics.candidates.len(), 2);
    for candidate in &after_projection.diagnostics.candidates {
        assert_eq!(candidate.reason, CandidateReasonV1::AgentAuthored);
        assert_eq!(candidate.reason.as_str(), "agent_authored");
    }
}

#[test]
fn an_agent_cannot_even_introduce_a_subject() {
    let only_agent = [agent_claim(
        "github:kckylechen1/tachi#9999",
        TruthPredicate::Merged,
        "cafebabe",
        "some-model",
        "2026-07-25T12:00:00Z",
        "rev-z",
    )];
    let projection = project(&only_agent);
    assert!(
        projection.subjects.is_empty(),
        "an agent-authored event introduced a subject into the truth surface"
    );
    assert_eq!(projection.stats.candidate_assertions, 1);
    assert_eq!(projection.stats.admitted_assertions, 0);
}

#[test]
fn agent_authority_is_capped_at_build_time() {
    let event = agent_claim(
        ISSUE,
        TruthPredicate::Merged,
        "deadbeef",
        "some-model",
        "2026-07-25T12:00:00Z",
        "rev-z",
    );
    assert_eq!(event.authority, AGENT_AUTHORITY_CAP);
    assert!(!event.authority.is_decision_eligible());
}

#[test]
fn a_forged_decision_authority_on_an_agent_row_is_still_a_candidate() {
    // Simulate a row that reached the ledger by some other path with the
    // authority column set to the maximum. Redline 3 must not depend on the
    // writer having been well behaved.
    let mut event = agent_claim(
        ISSUE,
        TruthPredicate::Merged,
        "deadbeef",
        "some-model",
        "2026-07-25T12:00:00Z",
        "rev-z",
    );
    event.authority = AuthorityLevel::ExecutionGate;
    assert!(event.authority.is_decision_eligible());

    let assertion = decode_truth_assertion(&event).unwrap();
    assert_eq!(
        classify_assertion(&assertion, AS_OF).unwrap(),
        Some(CandidateReasonV1::AgentAuthored)
    );

    let projection = project(&[event]);
    assert!(projection.subjects.is_empty());
}

// ── acceptance: determinism ─────────────────────────────────────────────────

#[test]
fn shuffled_insertion_order_yields_byte_identical_canonical_json() {
    let mut events = merged_but_open_fixture();
    events.push(owner_decision(
        ISSUE,
        TruthPredicate::Accepted,
        VALUE_YES,
        "kckylechen1",
        "2026-07-23T08:00:00Z",
        "rev-d",
    ));
    events.push(agent_claim(
        ISSUE,
        TruthPredicate::OwnerClosed,
        VALUE_YES,
        "some-model",
        "2026-07-24T08:00:00Z",
        "rev-e",
    ));
    events.push(snapshot(
        "github:kckylechen1/tachi#1444",
        TruthPredicate::IssueState,
        VALUE_ISSUE_CLOSED,
        "2026-07-21T09:31:00Z",
        "rev-f",
    ));

    let baseline = project(&events).canonical_json().unwrap();

    let mut reversed = events.clone();
    reversed.reverse();
    assert_eq!(baseline, project(&reversed).canonical_json().unwrap());

    for rotation in 1..events.len() {
        let mut rotated = events.clone();
        rotated.rotate_left(rotation);
        assert_eq!(
            baseline,
            project(&rotated).canonical_json().unwrap(),
            "rotation by {rotation} changed the projection"
        );
    }
}

#[test]
fn fold_from_checkpoint_equals_a_clean_fold_even_when_windows_overlap() {
    let mut events = merged_but_open_fixture();
    events.push(owner_decision(
        ISSUE,
        TruthPredicate::Accepted,
        VALUE_YES,
        "kckylechen1",
        "2026-07-23T08:00:00Z",
        "rev-d",
    ));

    let clean = reduce_current_truth(&events, AS_OF)
        .unwrap()
        .canonical_json()
        .unwrap();

    let mut incremental = CurrentTruthFold::new();
    incremental.ingest(&events[0..3]);
    // Deliberately overlapping second window, the way a checkpoint replay
    // re-reads the tail of the previous page.
    incremental.ingest(&events[2..]);
    let staged = incremental
        .project(AS_OF)
        .unwrap()
        .canonical_json()
        .unwrap();

    assert_eq!(clean, staged);
    assert_eq!(incremental.len(), events.len());
    assert!(!incremental.is_empty());
}

// ── acceptance: conflict ────────────────────────────────────────────────────

#[test]
fn two_admitted_equal_authority_assertions_conflict_with_no_winner() {
    let events = [
        owner_decision(
            ISSUE,
            TruthPredicate::OwnerClosed,
            VALUE_YES,
            "alice",
            "2026-07-23T08:00:00Z",
            "rev-d",
        ),
        owner_decision(
            ISSUE,
            TruthPredicate::OwnerClosed,
            VALUE_NO,
            "bob",
            "2026-07-24T08:00:00Z",
            "rev-d",
        ),
    ];
    let projection = project(&events);
    let state = state_of(&projection, ISSUE, TruthPredicate::OwnerClosed);

    let TruthStateV1::Conflicted { members } = &state else {
        panic!("expected conflicted, got {state:?}");
    };
    assert_eq!(members.len(), 2, "both members must be emitted");
    assert!(state.current_value().is_none());

    // Later-in-time did not win. Recency is not a tiebreak for decisions.
    let values: Vec<&str> = members
        .iter()
        .map(|m| m.value.as_ref().unwrap().as_str())
        .collect();
    assert!(values.contains(&VALUE_YES) && values.contains(&VALUE_NO));

    let serialized = serde_json::to_string(&state).unwrap();
    assert!(
        !serialized.contains("winner"),
        "conflicted state must have no winner field: {serialized}"
    );
    assert_eq!(projection.stats.conflicted_predicates, 1);
}

#[test]
fn distinct_issuers_at_the_same_revision_both_reach_the_ledger() {
    // If the append-if-absent key collapsed on issuer, the second observation
    // would vanish at insert and the conflict above would silently disappear.
    let alice = truth_assertion_event_id(
        ISSUE,
        TruthPredicate::OwnerClosed,
        "rev-d",
        &AssertionRelationV1::Standalone,
        &TruthIssuerV1::OwnerDecision {
            owner_login: "alice".to_string(),
        },
    );
    let bob = truth_assertion_event_id(
        ISSUE,
        TruthPredicate::OwnerClosed,
        "rev-d",
        &AssertionRelationV1::Standalone,
        &TruthIssuerV1::OwnerDecision {
            owner_login: "bob".to_string(),
        },
    );
    assert_ne!(alice, bob);
    // And the same observation twice is the same event, so a re-fetch appends
    // nothing.
    assert_eq!(
        alice,
        truth_assertion_event_id(
            ISSUE,
            TruthPredicate::OwnerClosed,
            "rev-d",
            &AssertionRelationV1::Standalone,
            &TruthIssuerV1::OwnerDecision {
                owner_login: "alice".to_string(),
            },
        )
    );
}

// ── acceptance: open → closed → reopened ────────────────────────────────────

#[test]
fn open_closed_reopened_is_a_transition_not_a_conflict() {
    // GitHub's `state` vocabulary is OPEN | CLOSED; a reopen is observed as a
    // later `open`. The discrimination is transition-vs-conflict, not the
    // spelling of the third value.
    let events = [
        snapshot(
            ISSUE,
            TruthPredicate::IssueState,
            VALUE_ISSUE_OPEN,
            "2026-07-20T10:00:00Z",
            "rev-a",
        ),
        snapshot(
            ISSUE,
            TruthPredicate::IssueState,
            VALUE_ISSUE_CLOSED,
            "2026-07-21T10:00:00Z",
            "rev-b",
        ),
        snapshot(
            ISSUE,
            TruthPredicate::IssueState,
            VALUE_ISSUE_OPEN,
            "2026-07-22T10:00:00Z",
            "rev-c",
        ),
    ];
    let projection = project(&events);
    let state = state_of(&projection, ISSUE, TruthPredicate::IssueState);

    assert!(
        !state.is_conflicted(),
        "a lifecycle transition must not read as a conflict"
    );
    assert_eq!(state.current_value().unwrap().as_str(), VALUE_ISSUE_OPEN);

    let entry = projection
        .subject(ISSUE)
        .unwrap()
        .predicate(TruthPredicate::IssueState)
        .unwrap();
    assert_eq!(entry.displaced.len(), 2);
    assert_eq!(projection.stats.conflicted_predicates, 0);
}

#[test]
fn decisions_do_not_transition_on_recency() {
    // The mirror image of the test above: `owner_closed` is a decision, so a
    // later decision does not quietly out-rank an earlier one.
    let events = [
        owner_decision(
            ISSUE,
            TruthPredicate::OwnerClosed,
            VALUE_NO,
            "alice",
            "2026-07-20T10:00:00Z",
            "rev-a",
        ),
        owner_decision(
            ISSUE,
            TruthPredicate::OwnerClosed,
            VALUE_YES,
            "alice",
            "2026-07-25T10:00:00Z",
            "rev-b",
        ),
    ];
    assert!(state_of(&project(&events), ISSUE, TruthPredicate::OwnerClosed).is_conflicted());
}

// ── relations ───────────────────────────────────────────────────────────────

#[test]
fn a_retraction_removes_its_target_and_history_keeps_it() {
    let accepted = owner_decision(
        ISSUE,
        TruthPredicate::Accepted,
        VALUE_YES,
        "alice",
        "2026-07-20T10:00:00Z",
        "rev-a",
    );
    let mut retraction = base_input(
        ISSUE,
        TruthPredicate::Accepted,
        None,
        TruthIssuerV1::OwnerDecision {
            owner_login: "alice".to_string(),
        },
        "2026-07-21T10:00:00Z",
        "rev-b",
    );
    retraction.authority = AuthorityLevel::ExecutionGate;
    retraction.relation = AssertionRelationV1::Retracts {
        target_assertion_id: accepted.id.clone(),
    };
    let retraction = build(&retraction);

    let projection = project(&[accepted.clone(), retraction]);
    let entry = projection
        .subject(ISSUE)
        .unwrap()
        .predicate(TruthPredicate::Accepted)
        .unwrap();

    assert!(entry.state.is_unknown(), "a retracted value must not stand");
    assert_eq!(entry.retracted.len(), 1);
    assert_eq!(entry.retracted[0].assertion_id, accepted.id);
    assert!(entry.superseded.is_empty());
}

#[test]
fn a_supersession_displaces_its_target_and_asserts_the_replacement() {
    let first = owner_decision(
        ISSUE,
        TruthPredicate::Accepted,
        VALUE_YES,
        "alice",
        "2026-07-20T10:00:00Z",
        "rev-a",
    );
    let mut correction = base_input(
        ISSUE,
        TruthPredicate::Accepted,
        Some(VALUE_NO),
        TruthIssuerV1::OwnerDecision {
            owner_login: "alice".to_string(),
        },
        "2026-07-21T10:00:00Z",
        "rev-b",
    );
    correction.authority = AuthorityLevel::ExecutionGate;
    correction.relation = AssertionRelationV1::Supersedes {
        target_assertion_id: first.id.clone(),
    };
    let correction = build(&correction);

    let projection = project(&[first.clone(), correction]);
    let entry = projection
        .subject(ISSUE)
        .unwrap()
        .predicate(TruthPredicate::Accepted)
        .unwrap();

    assert_eq!(entry.state.current_value().unwrap().as_str(), VALUE_NO);
    assert_eq!(entry.superseded.len(), 1);
    assert_eq!(entry.superseded[0].assertion_id, first.id);
    assert!(entry.retracted.is_empty());
}

#[test]
fn a_retraction_carrying_a_value_is_refused_at_build() {
    let mut input = base_input(
        ISSUE,
        TruthPredicate::Accepted,
        Some(VALUE_YES),
        TruthIssuerV1::OwnerDecision {
            owner_login: "alice".to_string(),
        },
        "2026-07-21T10:00:00Z",
        "rev-b",
    );
    input.relation = AssertionRelationV1::Retracts {
        target_assertion_id: "truth-assertion-whatever".to_string(),
    };
    assert!(matches!(
        build_truth_assertion_event(&input),
        Err(CurrentTruthError::InvalidAssertion(_))
    ));
}

// ── admission ───────────────────────────────────────────────────────────────

#[test]
fn an_owner_decision_cannot_assert_a_merge_sha() {
    let mut input = base_input(
        ISSUE,
        TruthPredicate::Merged,
        Some("deadbeef"),
        TruthIssuerV1::OwnerDecision {
            owner_login: "alice".to_string(),
        },
        "2026-07-21T10:00:00Z",
        "rev-b",
    );
    input.authority = AuthorityLevel::ExecutionGate;
    let event = build(&input);
    let assertion = decode_truth_assertion(&event).unwrap();
    assert_eq!(
        classify_assertion(&assertion, AS_OF).unwrap(),
        Some(CandidateReasonV1::IssuerNotAuthoritativeForPredicate)
    );
    assert_eq!(
        CandidateReasonV1::IssuerNotAuthoritativeForPredicate.as_str(),
        "issuer_not_authoritative_for_predicate"
    );
}

#[test]
fn a_non_decision_eligible_authority_column_is_a_candidate() {
    let mut input = base_input(
        ISSUE,
        TruthPredicate::Merged,
        Some("deadbeef"),
        TruthIssuerV1::SourceSnapshot {
            system: "github".to_string(),
            snapshot_digest: "sha256:rev-b".to_string(),
        },
        "2026-07-21T10:00:00Z",
        "rev-b",
    );
    input.authority = AuthorityLevel::Advisory;
    let event = build(&input);
    let assertion = decode_truth_assertion(&event).unwrap();
    assert_eq!(
        classify_assertion(&assertion, AS_OF).unwrap(),
        Some(CandidateReasonV1::AuthorityNotDecisionEligible)
    );
}

#[test]
fn an_observation_after_as_of_is_a_candidate_not_a_truth() {
    let events = [
        snapshot(
            ISSUE,
            TruthPredicate::IssueState,
            VALUE_ISSUE_OPEN,
            "2026-07-20T10:00:00Z",
            "rev-a",
        ),
        snapshot(
            ISSUE,
            TruthPredicate::Merged,
            "deadbeef",
            "2026-08-01T10:00:00Z",
            "rev-b",
        ),
    ];
    let projection = project(&events);
    assert!(state_of(&projection, ISSUE, TruthPredicate::Merged).is_unknown());
    assert_eq!(projection.diagnostics.candidates.len(), 1);
    assert_eq!(
        projection.diagnostics.candidates[0].reason,
        CandidateReasonV1::ObservedAfterAsOf
    );

    // Move `as_of` forward and the same ledger yields the merge.
    let later = reduce_current_truth(&events, "2026-08-02T00:00:00Z").unwrap();
    assert_eq!(
        current_str(&later, ISSUE, TruthPredicate::Merged),
        "deadbeef"
    );
}

#[test]
fn an_unparseable_as_of_refuses_the_whole_projection() {
    let err = reduce_current_truth(&merged_but_open_fixture(), "yesterday").unwrap_err();
    assert!(matches!(err, CurrentTruthError::InvalidAsOf(_)));
    assert!(err.to_string().contains("yesterday"));
}

// ── decode, refusal, and the row/payload split ──────────────────────────────

#[test]
fn assertion_id_and_authority_come_from_the_row_never_from_the_payload() {
    let mut event = snapshot(
        ISSUE,
        TruthPredicate::Merged,
        "27499b706",
        "2026-07-21T09:30:00Z",
        "rev-b",
    );
    event.authority = AuthorityLevel::Advisory;
    if let serde_json::Value::Object(map) = &mut event.payload {
        map.insert("assertion_id".to_string(), json!("forged-id"));
        map.insert("authority".to_string(), json!("execution_gate"));
    } else {
        panic!("payload is not an object");
    }

    let assertion = decode_truth_assertion(&event).unwrap();
    assert_eq!(assertion.assertion_id, event.id);
    assert_ne!(assertion.assertion_id, "forged-id");
    assert_eq!(assertion.authority, AuthorityLevel::Advisory);
}

#[test]
fn a_malformed_payload_is_rejected_with_a_reason_never_swallowed() {
    let mut event = snapshot(
        ISSUE,
        TruthPredicate::Merged,
        "27499b706",
        "2026-07-21T09:30:00Z",
        "rev-b",
    );
    event.payload = json!({ "payload_version": "truth_assertion_v1" });

    let projection = project(&[event.clone()]);
    assert!(projection.subjects.is_empty());
    assert_eq!(projection.diagnostics.rejected.len(), 1);
    assert_eq!(projection.diagnostics.rejected[0].event_id, event.id);
    assert!(!projection.diagnostics.rejected[0].reason.is_empty());
    assert_eq!(projection.stats.rejected_assertions, 1);
}

#[test]
fn a_wrong_payload_version_is_refused_rather_than_best_effort_parsed() {
    let mut event = snapshot(
        ISSUE,
        TruthPredicate::Merged,
        "27499b706",
        "2026-07-21T09:30:00Z",
        "rev-b",
    );
    if let serde_json::Value::Object(map) = &mut event.payload {
        map.insert("payload_version".to_string(), json!("truth_assertion_v2"));
    }
    let err = decode_truth_assertion(&event).unwrap_err();
    assert!(err.contains("payload_version"), "{err}");
}

#[test]
fn other_ledger_event_types_are_ignored_by_id_not_dropped() {
    let mut foreign = snapshot(
        ISSUE,
        TruthPredicate::Merged,
        "27499b706",
        "2026-07-21T09:30:00Z",
        "rev-b",
    );
    foreign.id = "wiki-saved-1".to_string();
    foreign.event_type = "wiki.saved".to_string();

    let mut events = merged_but_open_fixture();
    events.push(foreign);
    let projection = project(&events);

    assert_eq!(projection.diagnostics.ignored_event_ids, ["wiki-saved-1"]);
    assert_eq!(projection.stats.ignored_events, 1);
    assert_eq!(projection.stats.admitted_assertions, 4);
}

#[test]
fn a_closed_value_vocabulary_is_enforced() {
    let mut input = base_input(
        ISSUE,
        TruthPredicate::IssueState,
        Some("reopened"),
        TruthIssuerV1::SourceSnapshot {
            system: "github".to_string(),
            snapshot_digest: "sha256:rev-a".to_string(),
        },
        "2026-07-20T10:00:00Z",
        "rev-a",
    );
    assert!(matches!(
        build_truth_assertion_event(&input),
        Err(CurrentTruthError::InvalidAssertion(_))
    ));

    // And an assertion that cannot name its evidence is refused outright.
    input.value = Some(VALUE_ISSUE_OPEN.to_string());
    input.evidence_ref = "   ".to_string();
    assert!(matches!(
        build_truth_assertion_event(&input),
        Err(CurrentTruthError::InvalidAssertion(_))
    ));
}

// ── redline 1: nothing here can reach `memories` ────────────────────────────

#[test]
fn built_events_carry_no_projection_hints_and_no_behavioural_effects() {
    for event in merged_but_open_fixture() {
        assert_eq!(event.event_type, TRUTH_ASSERTION_EVENT_TYPE);
        assert_eq!(event.domain, TRUTH_ASSERTION_DOMAIN);
        assert_eq!(event.effects, [EffectScope::None]);
        assert!(
            event.projection_hints.is_empty(),
            "a projection hint would let the continuity sweep write this event into `memories`"
        );
        // The event type must also match no prefix in the sweep's inference
        // table, whose entries all end in `.` or are exact matches.
        assert!(!event.event_type.starts_with("pattern."));
        assert!(!event.event_type.starts_with("timeline."));
        assert!(!event.event_type.starts_with("outcome."));
        assert!(!event.event_type.starts_with("project_cycle."));
        assert!(!event.event_type.starts_with("evidence_gate."));
    }
}

#[test]
fn created_at_is_the_source_instant_so_a_refetch_is_byte_identical() {
    let once = snapshot(
        ISSUE,
        TruthPredicate::Merged,
        "27499b706",
        "2026-07-21T09:30:00+00:00",
        "rev-b",
    );
    // Same observation, spelled with a different but equivalent offset.
    let twice = snapshot(
        ISSUE,
        TruthPredicate::Merged,
        "27499b706",
        "2026-07-21T10:30:00+01:00",
        "rev-b",
    );
    assert_eq!(
        once, twice,
        "a re-fetch must produce a byte-identical event"
    );
    assert_eq!(once.created_at, "2026-07-21T09:30:00.000000000Z");
}

// ── action queue ────────────────────────────────────────────────────────────

#[test]
fn merged_and_owner_protected_with_no_owner_close_is_awaiting_owner() {
    let projection = project(&merged_but_open_fixture());
    let queue = derive_action_queue(&projection);

    assert_eq!(queue.len(), 1, "{queue:?}");
    assert_eq!(queue[0].kind, ActionKindV1::AwaitingOwner);
    assert_eq!(queue[0].kind.as_str(), "awaiting_owner");
    assert_eq!(queue[0].subject_ref, ISSUE);
    assert_eq!(queue[0].predicate, Some(TruthPredicate::OwnerClosed));
    assert!(!queue[0].evidence.is_empty());
}

#[test]
fn an_owner_close_drains_the_awaiting_owner_item() {
    let mut events = merged_but_open_fixture();
    events.push(owner_decision(
        ISSUE,
        TruthPredicate::OwnerClosed,
        VALUE_YES,
        "kckylechen1",
        "2026-07-24T08:00:00Z",
        "rev-g",
    ));
    let queue = derive_action_queue(&project(&events));
    assert!(queue.is_empty(), "{queue:?}");
}

#[test]
fn an_unprotected_subject_does_not_manufacture_an_owner_gate() {
    // `owner_protected` unknown must not be read as protected.
    let events: Vec<TachiEventRecord> = merged_but_open_fixture()
        .into_iter()
        .filter(|event| {
            decode_truth_assertion(event).unwrap().predicate != TruthPredicate::OwnerProtected
        })
        .collect();
    assert!(derive_action_queue(&project(&events)).is_empty());
}

#[test]
fn a_conflicted_predicate_produces_a_resolve_item() {
    let events = [
        owner_decision(
            ISSUE,
            TruthPredicate::OwnerClosed,
            VALUE_YES,
            "alice",
            "2026-07-23T08:00:00Z",
            "rev-d",
        ),
        owner_decision(
            ISSUE,
            TruthPredicate::OwnerClosed,
            VALUE_NO,
            "bob",
            "2026-07-24T08:00:00Z",
            "rev-d",
        ),
    ];
    let queue = derive_action_queue(&project(&events));
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].kind, ActionKindV1::ResolveConflict);
    assert_eq!(queue[0].predicate, Some(TruthPredicate::OwnerClosed));
    assert_eq!(queue[0].evidence.len(), 2);
}

#[test]
fn a_handoff_pinned_behind_the_reconciled_state_asks_for_re_verification() {
    let mut events = merged_but_open_fixture();
    events.push(snapshot(
        ISSUE,
        TruthPredicate::HandoffEvidenceHead,
        "rev-a",
        // Pinned before the merge observation at 2026-07-21T09:30:00Z.
        "2026-07-20T10:00:00Z",
        "rev-handoff",
    ));
    let queue = derive_action_queue(&project(&events));
    assert!(
        queue
            .iter()
            .any(|item| item.kind == ActionKindV1::ReVerifyHandoff),
        "{queue:?}"
    );

    // A handoff pinned at or after the latest reconciled observation is not
    // stale, and must not generate noise.
    let mut fresh = merged_but_open_fixture();
    fresh.push(snapshot(
        ISSUE,
        TruthPredicate::HandoffEvidenceHead,
        "rev-b",
        "2026-07-21T09:30:00Z",
        "rev-handoff-2",
    ));
    assert!(!derive_action_queue(&project(&fresh))
        .iter()
        .any(|item| item.kind == ActionKindV1::ReVerifyHandoff));
}

#[test]
fn the_action_queue_is_a_pure_function_of_the_projection() {
    let events = merged_but_open_fixture();
    let projection = project(&events);
    let first = derive_action_queue(&projection);
    let second = derive_action_queue(&projection);
    assert_eq!(first, second);
    // And identical for a projection built from a shuffled ledger.
    let mut shuffled = events;
    shuffled.reverse();
    assert_eq!(first, derive_action_queue(&project(&shuffled)));
}

#[test]
fn an_empty_ledger_projects_to_an_empty_surface_not_an_error() {
    let projection = project(&[]);
    assert!(projection.subjects.is_empty());
    assert_eq!(projection.stats, Default::default());
    assert_eq!(projection.as_of, "2026-07-26T00:00:00.000000000Z");
    assert!(derive_action_queue(&projection).is_empty());
}
