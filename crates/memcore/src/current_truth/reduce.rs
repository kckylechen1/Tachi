//! The pure read-time fold.
//!
//! # The procedure, verbatim from the adjudicated rule
//!
//! Per `(subject_ref, predicate)`, over events in total order:
//!
//! 1. **Admit** by the predicate's authority policy — [`classify_assertion_at`].
//!    Agent-authored assertions become candidates and never enter reduction.
//! 2. **Retract** — an admitted retraction removes its target from the working
//!    set; history keeps it on [`PredicateTruthV1::retracted`].
//! 3. **Supersede/correct** — a targeted assertion is displaced and contributes
//!    [`PredicateTruthV1::superseded`].
//! 4. **Source-authoritative transitions** — for predicates where
//!    [`TruthPredicate::revises_by_observation_recency`] holds, only the latest
//!    observation stratum survives; the rest contribute
//!    [`PredicateTruthV1::displaced`]. `open → closed → reopened` is a
//!    transition, not a conflict.
//! 5. **Remainder** — empty ⇒ `unknown`; one distinct value ⇒ `current(value)`;
//!    ≥2 distinct values ⇒ `conflicted` with **all** members emitted.
//!
//! # Three places this file refuses to be clever
//!
//! **No winner is ever picked across equal authority.** The only ranking in the
//! whole fold is step 4's latest-observation stratum, and it is gated on a
//! per-predicate flag that is `false` for every decision predicate. Nothing
//! else breaks a tie: two admitted assertions with distinct values produce
//! [`TruthStateV1::Conflicted`], which has no winner field to populate.
//!
//! **Assertion ids never decide truth.** They are a sort key so emitted lists
//! are byte-stable, and nothing else. If two observations of a
//! recency-revised predicate share an instant and disagree, that is a
//! conflict — deciding it by hash order would be picking a winner with a coin.
//!
//! **Relations are evaluated as a set, not a sequence.** Every admitted
//! retraction and supersession contributes its target to a removal set, and the
//! sets are applied once. A sequential walk would make the answer depend on
//! where a relation sits relative to its target, which is arrival order wearing
//! a disguise. Two consequences, both deliberate: retracting a superseding
//! assertion does not resurrect what it superseded (history is monotone), and a
//! relation targeting an assertion outside its own `(subject, predicate)` group
//! is inert.
//!
//! # Authority is a gate, not a ladder
//!
//! Step 5 says "≥2 distinct values **at equal admitted authority**". In this
//! repo's vocabulary, admission *is* a boolean:
//! [`AuthorityLevel::is_decision_eligible`](crate::types::AuthorityLevel::is_decision_eligible)
//! (`types/continuity.rs:53-58`) partitions the enum into eligible and not, and
//! the enum derives no `Ord`. So every admitted assertion is at equal admitted
//! authority by construction, and the qualifier is always satisfied. Inventing
//! a precedence among `raw_fact` / `derived_evidence` / `blocker` /
//! `execution_gate` to break ties would be exactly the parallel vocabulary the
//! design forbids.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use crate::types::TachiEventRecord;

use super::admission::{classify_assertion_at, decode_truth_assertion};
use super::types::{
    normalize_instant, parse_instant, sort_refs, AssertionRefV1, AssertionRelationV1,
    CandidateRecordV1, CurrentTruthDiagnosticsV1, CurrentTruthError, CurrentTruthProjectionV1,
    CurrentTruthStatsV1, PredicateTruthV1, RejectedAssertionV1, SubjectTruthV1, TruthAssertionV1,
    TruthPredicate, TruthStateV1, TruthValue, CURRENT_TRUTH_PROJECTION_VERSION,
    TRUTH_ASSERTION_EVENT_TYPE,
};

/// Admitted assertions of one subject, bucketed by predicate.
type PredicateBuckets<'a> = BTreeMap<TruthPredicate, Vec<&'a TruthAssertionV1>>;

/// Accumulates ledger events; produces a projection on demand.
///
/// Ingestion is **classification-free**: admission depends on `as_of`, which is
/// not known until [`Self::project`] is called, so deciding a candidate at
/// ingest time would bake one instant's answer into the accumulator. What the
/// fold keeps is decodable facts, not verdicts.
///
/// Ingesting the same event id twice is idempotent, which is what makes
/// fold-from-checkpoint equal a clean fold even when the checkpoint window
/// overlaps.
#[derive(Debug, Clone, Default)]
pub struct CurrentTruthFold {
    assertions: Vec<TruthAssertionV1>,
    rejected: Vec<RejectedAssertionV1>,
    ignored_event_ids: Vec<String>,
    seen_event_ids: BTreeSet<String>,
}

impl CurrentTruthFold {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a batch. Order is irrelevant — [`Self::project`] sorts.
    pub fn ingest(&mut self, events: &[TachiEventRecord]) {
        for event in events {
            self.ingest_event(event);
        }
    }

    /// Feed one event.
    ///
    /// Nothing is ever dropped silently: an event of another type lands in
    /// `ignored_event_ids`, and an event of *this* type that will not decode
    /// lands in `rejected` with a reason. A silent drop would read downstream
    /// as "there was nothing to say about this subject", which is the exact
    /// misreading `unknown` exists to prevent.
    pub fn ingest_event(&mut self, event: &TachiEventRecord) {
        if !self.seen_event_ids.insert(event.id.clone()) {
            return;
        }
        if event.event_type.trim() != TRUTH_ASSERTION_EVENT_TYPE {
            self.ignored_event_ids.push(event.id.clone());
            return;
        }
        match decode_truth_assertion(event) {
            Ok(assertion) => self.assertions.push(assertion),
            Err(reason) => self.rejected.push(RejectedAssertionV1 {
                event_id: event.id.clone(),
                reason,
            }),
        }
    }

    /// Number of decodable assertions currently held. Diagnostic only.
    pub fn len(&self) -> usize {
        self.assertions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.assertions.is_empty()
    }

    /// Reduce to a projection true at `as_of`.
    ///
    /// The only clock in the module. An unparseable `as_of` refuses the whole
    /// projection rather than falling back to "now" or to no time filter — a
    /// reducer that quietly widened its own window would manufacture the
    /// staleness this issue exists to kill.
    pub fn project(&self, as_of: &str) -> Result<CurrentTruthProjectionV1, CurrentTruthError> {
        let as_of_ts =
            parse_instant(as_of).ok_or_else(|| CurrentTruthError::InvalidAsOf(as_of.to_string()))?;
        let as_of_norm = normalize_instant(as_of_ts);

        // ── Step 1: admission ────────────────────────────────────────────────
        let mut admitted: Vec<&TruthAssertionV1> = Vec::new();
        let mut candidates: Vec<CandidateRecordV1> = Vec::new();
        for assertion in &self.assertions {
            match classify_assertion_at(assertion, as_of_ts) {
                Some(reason) => candidates.push(CandidateRecordV1 {
                    assertion: assertion.clone(),
                    reason,
                }),
                None => admitted.push(assertion),
            }
        }
        candidates.sort_by(|a, b| {
            a.assertion
                .observed_at
                .cmp(&b.assertion.observed_at)
                .then_with(|| a.assertion.assertion_id.cmp(&b.assertion.assertion_id))
        });

        // Grouping happens over *admitted only*. This is what makes redline 3
        // structural rather than cosmetic: an agent-authored event cannot
        // change a value and cannot even introduce a subject into the truth
        // surface, because the subject set is built from this map.
        let mut grouped: BTreeMap<&str, PredicateBuckets<'_>> = BTreeMap::new();
        for assertion in admitted.iter().copied() {
            grouped
                .entry(assertion.subject_ref.as_str())
                .or_default()
                .entry(assertion.predicate)
                .or_default()
                .push(assertion);
        }

        let mut subjects: Vec<SubjectTruthV1> = Vec::with_capacity(grouped.len());
        let mut conflicted_predicates = 0usize;
        let mut unknown_owner_closed_subjects = 0usize;
        let empty_group: Vec<&TruthAssertionV1> = Vec::new();

        for (subject_ref, by_predicate) in &grouped {
            // Every subject emits every predicate, in canonical order, so
            // `unknown` is explicit rather than inferred from a missing key.
            let mut predicates: Vec<PredicateTruthV1> =
                Vec::with_capacity(TruthPredicate::ALL.len());
            for predicate in TruthPredicate::ALL {
                let group = by_predicate.get(&predicate).unwrap_or(&empty_group);
                let reduced = reduce_group(predicate, group);
                if reduced.state.is_conflicted() {
                    conflicted_predicates += 1;
                }
                if predicate == TruthPredicate::OwnerClosed && reduced.state.is_unknown() {
                    unknown_owner_closed_subjects += 1;
                }
                predicates.push(reduced);
            }
            subjects.push(SubjectTruthV1 {
                subject_ref: subject_ref.to_string(),
                predicates,
            });
        }

        let stats = CurrentTruthStatsV1 {
            subjects: subjects.len(),
            admitted_assertions: admitted.len(),
            candidate_assertions: candidates.len(),
            rejected_assertions: self.rejected.len(),
            ignored_events: self.ignored_event_ids.len(),
            conflicted_predicates,
            unknown_owner_closed_subjects,
        };

        let mut rejected = self.rejected.clone();
        rejected.sort_by(|a, b| a.event_id.cmp(&b.event_id));
        let mut ignored_event_ids = self.ignored_event_ids.clone();
        ignored_event_ids.sort();

        Ok(CurrentTruthProjectionV1 {
            projection_version: CURRENT_TRUTH_PROJECTION_VERSION.to_string(),
            as_of: as_of_norm,
            subjects,
            diagnostics: CurrentTruthDiagnosticsV1 {
                candidates,
                rejected,
                ignored_event_ids,
            },
            stats,
        })
    }
}

/// One-shot convenience: build a fold, feed it, project it.
pub fn reduce_current_truth(
    events: &[TachiEventRecord],
    as_of: &str,
) -> Result<CurrentTruthProjectionV1, CurrentTruthError> {
    let mut fold = CurrentTruthFold::new();
    fold.ingest(events);
    fold.project(as_of)
}

/// Steps 2-5 for one `(subject_ref, predicate)` group.
///
/// `group` holds admitted assertions only; admission already happened.
fn reduce_group(predicate: TruthPredicate, group: &[&TruthAssertionV1]) -> PredicateTruthV1 {
    // ── Step 2 + 3: relation removal sets ────────────────────────────────────
    let mut retracted_targets: BTreeSet<&str> = BTreeSet::new();
    let mut superseded_targets: BTreeSet<&str> = BTreeSet::new();
    for assertion in group.iter().copied() {
        match &assertion.relation {
            AssertionRelationV1::Standalone => {}
            AssertionRelationV1::Retracts {
                target_assertion_id,
            } => {
                retracted_targets.insert(target_assertion_id.as_str());
            }
            AssertionRelationV1::Supersedes {
                target_assertion_id,
            } => {
                superseded_targets.insert(target_assertion_id.as_str());
            }
        }
    }

    let mut retracted: Vec<AssertionRefV1> = Vec::new();
    let mut superseded: Vec<AssertionRefV1> = Vec::new();
    let mut working: Vec<&TruthAssertionV1> = Vec::new();

    for assertion in group.iter().copied() {
        // A retraction is a directive, not a member: it asserts no value, so it
        // can neither be `current` nor a conflict member.
        if matches!(assertion.relation, AssertionRelationV1::Retracts { .. }) {
            continue;
        }
        let id = assertion.assertion_id.as_str();
        // Retraction outranks supersession: the stronger withdrawal wins when
        // an assertion is targeted by both.
        if retracted_targets.contains(id) {
            retracted.push(assertion.as_ref_v1());
            continue;
        }
        if superseded_targets.contains(id) {
            superseded.push(assertion.as_ref_v1());
            continue;
        }
        working.push(assertion);
    }

    // ── Step 4: source-authoritative transitions ─────────────────────────────
    let mut displaced: Vec<AssertionRefV1> = Vec::new();
    if predicate.revises_by_observation_recency() && !working.is_empty() {
        let latest: Option<DateTime<Utc>> = working
            .iter()
            .filter_map(|a| parse_instant(&a.observed_at))
            .max();
        if let Some(latest) = latest {
            let mut survivors: Vec<&TruthAssertionV1> = Vec::new();
            for assertion in working {
                if parse_instant(&assertion.observed_at) == Some(latest) {
                    survivors.push(assertion);
                } else {
                    // The merge observation a later revert observation
                    // replaced, or the `open` a `closed` replaced. The ledger
                    // event behind it is untouched — this is a read-time
                    // disposition, not a rewrite.
                    displaced.push(assertion.as_ref_v1());
                }
            }
            working = survivors;
        }
    }

    // ── Step 5: remainder ────────────────────────────────────────────────────
    let mut distinct: BTreeSet<&TruthValue> = BTreeSet::new();
    let mut refs: Vec<AssertionRefV1> = Vec::new();
    for assertion in working.iter().copied() {
        // Defensive: decode guarantees a value on every non-retraction, so a
        // `None` here means the fold was fed something that skipped decode.
        // Skipping it is the conservative direction — it cannot invent truth.
        if let Some(value) = assertion.value.as_ref() {
            distinct.insert(value);
            refs.push(assertion.as_ref_v1());
        }
    }

    sort_refs(&mut refs);
    let state = match distinct.len() {
        0 => TruthStateV1::Unknown,
        1 => {
            let value = distinct
                .into_iter()
                .next()
                .cloned()
                .expect("a set of length 1 yields one element");
            TruthStateV1::Current {
                value,
                evidence: refs,
            }
        }
        _ => TruthStateV1::Conflicted { members: refs },
    };

    sort_refs(&mut superseded);
    sort_refs(&mut retracted);
    sort_refs(&mut displaced);

    PredicateTruthV1 {
        predicate,
        state,
        superseded,
        retracted,
        displaced,
    }
}
