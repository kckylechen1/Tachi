//! The action queue — derived, never stored.
//!
//! Every item here is a pure function of a [`CurrentTruthProjectionV1`]. There
//! is no table, no file, no `Connection`, and no id that outlives a call:
//!
//! - **Fill** is reduction. An item exists because the projection says so.
//! - **Drain** is the truth changing on the next reduction. Nothing is
//!   "completed"; the condition simply stops holding.
//! - **Bound** is `O(open subjects)`, because the rules only fire on predicates
//!   an open subject can have.
//!
//! Storing these would create a second inbox that has to be reconciled with the
//! ledger, needs its own GC, and resurrects stale todos when the truth moves
//! but the row does not. That is the failure mode this issue exists to kill, so
//! the queue is deliberately unstorable: [`ActionItemV1`] carries no identity of
//! its own, only the projection coordinates that produced it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{
    parse_instant, AssertionRefV1, CurrentTruthProjectionV1, SubjectTruthV1, TruthPredicate,
    TruthStateV1, VALUE_NO, VALUE_YES,
};

/// The three conditions the adjudicated design names, and no others.
///
/// Declaration order is the canonical emission order within one subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKindV1 {
    /// A predicate is [`TruthStateV1::Conflicted`]. Somebody has to look; the
    /// reducer will not pick.
    ResolveConflict,
    /// Merged, owner-protected, and not owner-closed. The work is done and the
    /// close is not the agent's to make.
    AwaitingOwner,
    /// A handoff pinned its evidence to a revision older than the subject's
    /// latest reconciled observation. The handoff may still be right; it is no
    /// longer *known* to be.
    ReVerifyHandoff,
}

impl ActionKindV1 {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ResolveConflict => "resolve_conflict",
            Self::AwaitingOwner => "awaiting_owner",
            Self::ReVerifyHandoff => "re_verify_handoff",
        }
    }
}

impl std::fmt::Display for ActionKindV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One derived item. Carries no id, no timestamp of its own, and no state — it
/// is a *view* of the projection coordinates that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItemV1 {
    pub kind: ActionKindV1,
    pub subject_ref: String,
    /// The predicate that triggered the item, where one did.
    pub predicate: Option<TruthPredicate>,
    /// Deterministic, code-generated. Never model prose.
    pub detail: String,
    /// The evidence a human needs to act, carried through from the projection
    /// so acting on an item never requires a second lookup.
    pub evidence: Vec<AssertionRefV1>,
}

/// Derive the queue. Deterministic: same projection ⇒ same `Vec`, same order.
pub fn derive_action_queue(projection: &CurrentTruthProjectionV1) -> Vec<ActionItemV1> {
    let mut items: Vec<ActionItemV1> = Vec::new();
    // `projection.subjects` is already sorted by `subject_ref`, and
    // `TruthPredicate::ALL` fixes the inner order, so emission order is fixed
    // by construction rather than by a sort that could tie.
    for subject in &projection.subjects {
        push_conflicts(subject, &mut items);
        push_awaiting_owner(subject, &mut items);
        push_stale_handoff(subject, &mut items);
    }
    items
}

/// `conflicted ⇒ resolve`, one item per conflicted predicate.
fn push_conflicts(subject: &SubjectTruthV1, items: &mut Vec<ActionItemV1>) {
    for predicate in TruthPredicate::ALL {
        let Some(entry) = subject.predicate(predicate) else {
            continue;
        };
        let TruthStateV1::Conflicted { members } = &entry.state else {
            continue;
        };
        items.push(ActionItemV1 {
            kind: ActionKindV1::ResolveConflict,
            subject_ref: subject.subject_ref.clone(),
            predicate: Some(predicate),
            detail: format!(
                "{} admitted assertions disagree on {predicate}; the reducer picks no winner",
                members.len()
            ),
            evidence: members.clone(),
        });
    }
}

/// `merged ∧ ¬owner_closed ∧ owner-protected ⇒ awaiting-owner`.
///
/// Read precisely:
///
/// - **merged** = `merged` is `current` with a value other than [`VALUE_NO`]
///   (the value is normally a merge SHA, so the test is "not explicitly not
///   merged", never "some string is present").
/// - **owner-protected** = `owner_protected` is `current(yes)`. `unknown` does
///   **not** count: this rule must not manufacture an owner gate on a subject
///   nobody observed a protection label for.
/// - **¬owner_closed** = `owner_closed` is `unknown` or `current(no)`. A
///   *conflicted* `owner_closed` is excluded on purpose — that subject already
///   emits a [`ActionKindV1::ResolveConflict`] item, and telling someone to
///   wait for an owner while the ledger disagrees about whether the owner
///   already acted is worse than saying nothing.
fn push_awaiting_owner(subject: &SubjectTruthV1, items: &mut Vec<ActionItemV1>) {
    let Some(merged) = subject.state(TruthPredicate::Merged) else {
        return;
    };
    let Some(merged_value) = merged.current_value() else {
        return;
    };
    if merged_value.as_str() == VALUE_NO {
        return;
    }

    let protected = subject
        .state(TruthPredicate::OwnerProtected)
        .and_then(|state| state.current_value())
        .is_some_and(|value| value.as_str() == VALUE_YES);
    if !protected {
        return;
    }

    let owner_closed = subject.state(TruthPredicate::OwnerClosed);
    let awaiting = match owner_closed {
        None => true,
        Some(TruthStateV1::Unknown) => true,
        Some(TruthStateV1::Current { value, .. }) => value.as_str() == VALUE_NO,
        Some(TruthStateV1::Conflicted { .. }) => false,
    };
    if !awaiting {
        return;
    }

    let mut evidence = state_refs(merged).to_vec();
    if let Some(state) = subject.state(TruthPredicate::OwnerProtected) {
        evidence.extend_from_slice(state_refs(state));
    }
    if let Some(state) = owner_closed {
        evidence.extend_from_slice(state_refs(state));
    }

    items.push(ActionItemV1 {
        kind: ActionKindV1::AwaitingOwner,
        subject_ref: subject.subject_ref.clone(),
        predicate: Some(TruthPredicate::OwnerClosed),
        detail: format!(
            "merged as {merged_value} and owner-protected, with owner_closed {}",
            match owner_closed {
                Some(TruthStateV1::Current { value, .. }) => format!("current({value})"),
                _ => "unknown".to_string(),
            }
        ),
        evidence,
    });
}

/// A handoff whose evidence head predates the reconciled state ⇒ re-verify.
///
/// "Predates" is evaluated as an **instant comparison**, not a string compare:
/// the handoff's own `handoff_evidence_head` observation is parsed, so is the
/// latest observation backing any *other* predicate of the same subject, and
/// the item fires only on a strict `<`. Equal instants are not stale.
///
/// Fail direction: if either side will not parse, no item is emitted. A
/// re-verify prompt built on an unreadable timestamp is noise, and noise in
/// this queue is how a queue stops being read.
fn push_stale_handoff(subject: &SubjectTruthV1, items: &mut Vec<ActionItemV1>) {
    let Some(head) = subject.state(TruthPredicate::HandoffEvidenceHead) else {
        return;
    };
    if head.current_value().is_none() {
        return;
    }
    let Some(head_at) = latest_instant(head) else {
        return;
    };

    let mut reconciled_at: Option<DateTime<Utc>> = None;
    for predicate in TruthPredicate::ALL {
        if predicate == TruthPredicate::HandoffEvidenceHead {
            continue;
        }
        let Some(state) = subject.state(predicate) else {
            continue;
        };
        if let Some(instant) = latest_instant(state) {
            reconciled_at = Some(reconciled_at.map_or(instant, |current| current.max(instant)));
        }
    }
    let Some(reconciled_at) = reconciled_at else {
        return;
    };
    if head_at >= reconciled_at {
        return;
    }

    items.push(ActionItemV1 {
        kind: ActionKindV1::ReVerifyHandoff,
        subject_ref: subject.subject_ref.clone(),
        predicate: Some(TruthPredicate::HandoffEvidenceHead),
        detail: "handoff evidence head predates the subject's latest reconciled observation"
            .to_string(),
        evidence: state_refs(head).to_vec(),
    });
}

/// The assertions backing a state, whatever the variant.
fn state_refs(state: &TruthStateV1) -> &[AssertionRefV1] {
    match state {
        TruthStateV1::Unknown => &[],
        TruthStateV1::Current { evidence, .. } => evidence,
        TruthStateV1::Conflicted { members } => members,
    }
}

/// Latest instant among a state's backing assertions.
///
/// Deliberately re-parses rather than taking the lexicographic max of
/// `observed_at`: RFC3339 text only sorts as time when offset and sub-second
/// precision agree, and a comparison that is *usually* right is the exact shape
/// of the ordering bug this module's timestamps are normalised to avoid.
fn latest_instant(state: &TruthStateV1) -> Option<DateTime<Utc>> {
    state_refs(state)
        .iter()
        .filter_map(|r| parse_instant(&r.observed_at))
        .max()
}
