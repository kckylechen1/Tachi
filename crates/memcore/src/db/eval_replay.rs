//! Full replay of the append-only decision-fact ledger (tachi#1675 PR3,
//! design D6's "full replay ≡ incremental projection" and D4's replay
//! ordering clause).
//!
//! [`super::eval_projection`] is the INCREMENTAL read: it asks SQL for each
//! subject's current state — a joined row plus a per-subject judgment lookup
//! that keeps the last event. This module is the REPLAY read: it streams the
//! whole append-only judgment log in one pass and FOLDS it forward, and it
//! binds route facts in Rust from batched reads instead of through a SQL
//! `LEFT JOIN`. Same ledger, two mechanisms, one required answer — the
//! equivalence tests below and in the server's projection layer are what make
//! "the ledger is replayable" a checked claim rather than a slogan.
//!
//! What is deliberately NOT re-implemented: the base column mapping, the
//! attribution ladder, the candidate-array extractor and the published total
//! order, all of which are imported from `eval_projection`. Duplicating them
//! would make the equivalence fixture grade a copy of the mapping instead of
//! the fold it exists to test, and the copy would drift.
//!
//! Four properties this module is required to hold, each with a test:
//!
//! 1. **Ordering is `insertion_seq`, never a timestamp**
//!    ([`REPLAY_ORDERING_BASIS`]). Both adjudication tables carry a durable
//!    per-subject counter assigned inside the append transaction, with
//!    `UNIQUE (subject, insertion_seq)` — a strict total order per subject.
//!    `created_at` is NOT that order: a correction written by a clock-skewed
//!    or backdating caller can carry an EARLIER timestamp than the event it
//!    supersedes, and a timestamp-ordered replay would then resurrect the
//!    superseded judgment.
//! 2. **Corrections append; the ledger never rewrites.** A new disposition
//!    row changes what the replay RESOLVES TO while leaving every earlier row
//!    byte-identical.
//! 3. **Recorded policy revision governs interpretation.** Each row's
//!    `policy_source_revision` is the content-bearing route-policy snapshot
//!    hash stamped when the recommendation was made. This function has NO
//!    access to the mutable route-policy `state` table and never consults it:
//!    a replay of old rows cannot be re-scored under rules written after
//!    them. The recorded revisions are censused in [`EvalReplay`] so a
//!    consumer can see which policy states its evidence spans.
//! 4. **`occurred_at` basis is carried, not invented.** Legacy execution rows
//!    have no `occurred_at` column, so both paths map `occurred_at :=
//!    created_at` and mark it (design D4: no backfill, because backfilling
//!    fabricates event times).
//!
//! Strictly READ-ONLY, exactly like `eval_projection`: no `INSERT`/`UPDATE`/
//! `DELETE` and no write accessor.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::MemoryError;

use super::eval_projection::{
    candidate_profiles_from_value, dispatch_observation, list_dispatch_subject_rows,
    list_mirror_subject_rows, mirror_observation, sort_observations_newest_first,
    EvalAdjudicationFacts, EvalObservation, EvalRouteFacts, EvalSpine,
};
use super::route_eval::{
    get_route_recommendation, list_eval_rubric_scores, list_route_decisions, EvalRubricScoreRow,
    RouteDecisionRow, RouteRecommendationRow,
};

/// The ONLY ordering a replay may fold on. Named as a constant so it appears
/// verbatim in the replay's own output: a reader can check what the fold
/// claims to have ordered by, and a future edit to a timestamp order has to
/// change a published value to do it.
pub const REPLAY_ORDERING_BASIS: &str = "insertion_seq";

/// Result of a full replay: the reconstructed observations plus the ledger
/// provenance a consumer needs in order to trust them.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalReplay {
    /// Same shape, same order as [`super::eval_projection::list_eval_observations`]
    /// over the same window — that identity is the point of this module.
    pub observations: Vec<EvalObservation>,
    /// Recorded `policy_source_revision` → how many replayed observations
    /// were interpreted under it. The census is of what the LEDGER recorded;
    /// the live route-policy state is never read here, so a rule written
    /// after these rows cannot appear in this map.
    pub policy_revisions: BTreeMap<String, usize>,
    /// Observations with no recorded policy revision — an `unadvised`
    /// acceptance, a decision whose recommendation row is gone, or a mirror
    /// row (which structurally has neither). Counted, never guessed at.
    pub rows_without_policy_revision: usize,
    /// Judgment events folded, across every subject in scope. Larger than
    /// `observations.len()` exactly when some subject was corrected.
    pub judgment_events_applied: usize,
    /// Subjects whose current judgment supersedes at least one earlier one.
    pub corrected_subjects: usize,
    /// Always [`REPLAY_ORDERING_BASIS`].
    pub ordering_basis: &'static str,
}

impl EvalReplay {
    /// Canonical value of the whole replay — the comparison artifact the
    /// equivalence fixtures diff.
    pub fn canonical_json(&self) -> Value {
        canonicalize(&json!({
            "observations": canonical_eval_observations(&self.observations),
            "policy_revisions": self.policy_revisions,
            "rows_without_policy_revision": self.rows_without_policy_revision,
            "judgment_events_applied": self.judgment_events_applied,
            "corrected_subjects": self.corrected_subjects,
            "ordering_basis": self.ordering_basis,
        }))
    }

    /// SHA-256 of [`Self::canonical_json`]'s serialization. A one-line
    /// equality assertion for "these two replays are the same replay"; the
    /// canonical value itself is what to diff when it fails.
    pub fn digest(&self) -> String {
        digest_of(&self.canonical_json())
    }
}

/// Replay the ledger for the observations whose base row falls in
/// `[since, until)`, newest first, capped at `limit` overall — the same
/// window contract [`super::eval_projection::list_eval_observations`] takes,
/// so the two can be compared directly. A FULL replay is this function with
/// an empty `since`, no `until`, and a limit at or above the row count.
pub fn replay_eval_observations(
    conn: &Connection,
    since: &str,
    until: Option<&str>,
    limit: usize,
) -> Result<EvalReplay, MemoryError> {
    let dispatch_judgments = fold_judgment_stream(read_dispatch_judgment_events(conn)?);
    let mirror_judgments = fold_judgment_stream(read_mirror_judgment_events(conn)?);
    let dispatch_rubrics = rubrics_by_adjudication(conn, EvalSpine::Dispatch)?;
    let mirror_rubrics = rubrics_by_adjudication(conn, EvalSpine::Mirror)?;
    let decisions = decisions_by_dispatch_id(conn)?;
    let recommendations = recommendations_for(conn, &decisions)?;

    let mut observations = Vec::new();
    let mut judgment_events_applied = 0usize;
    let mut corrected_subjects = 0usize;

    for subject in list_dispatch_subject_rows(conn, since, until, limit)? {
        // The LEFT JOIN's null side, reproduced as a map miss: no decision row
        // is the honest "no acceptance fact was recorded" state (design D2),
        // never a fabricated one.
        let decision = decisions.get(&subject.dispatch_id);
        let recommendation = decision
            .and_then(|decision| decision.recommendation_id.as_deref())
            .and_then(|id| recommendations.get(id));
        let route = decision.map(|decision| route_facts(decision, recommendation));
        let selected_profile = decision.and_then(|decision| decision.selected_profile.clone());
        let judgment = dispatch_judgments.get(&subject.outcome_id);
        let rubric = judgment.and_then(|judgment| {
            dispatch_rubrics
                .get(judgment.adjudication_id.as_str())
                .cloned()
        });
        if let Some(judgment) = judgment {
            judgment_events_applied += judgment.event_count;
            if judgment.is_overturn() {
                corrected_subjects += 1;
            }
        }
        observations.push(dispatch_observation(
            subject,
            selected_profile,
            route,
            judgment.cloned(),
            rubric,
        ));
    }

    for subject in list_mirror_subject_rows(conn, since, until, limit)? {
        let judgment = mirror_judgments.get(&subject.eval_run_id);
        let rubric = judgment.and_then(|judgment| {
            mirror_rubrics
                .get(judgment.adjudication_id.as_str())
                .cloned()
        });
        if let Some(judgment) = judgment {
            judgment_events_applied += judgment.event_count;
            if judgment.is_overturn() {
                corrected_subjects += 1;
            }
        }
        observations.push(mirror_observation(subject, judgment.cloned(), rubric));
    }

    sort_observations_newest_first(&mut observations, limit);

    let mut policy_revisions: BTreeMap<String, usize> = BTreeMap::new();
    let mut rows_without_policy_revision = 0usize;
    for observation in &observations {
        match observation
            .route
            .as_ref()
            .and_then(|route| route.policy_source_revision.as_deref())
        {
            Some(revision) => *policy_revisions.entry(revision.to_string()).or_insert(0) += 1,
            None => rows_without_policy_revision += 1,
        }
    }

    Ok(EvalReplay {
        observations,
        policy_revisions,
        rows_without_policy_revision,
        judgment_events_applied,
        corrected_subjects,
        ordering_basis: REPLAY_ORDERING_BASIS,
    })
}

// ─── judgment fold ─────────────────────────────────────────────────────────

/// One appended judgment event, spine-normalized (the dispatch spine's
/// free-text `verdict` and the mirror spine's `usefulness` land in the same
/// slot, exactly as the incremental resolver normalizes them).
struct JudgmentEvent {
    subject_id: String,
    adjudication_id: String,
    actor: String,
    verdict: Option<String>,
    created_at: String,
    insertion_seq: i64,
}

/// Collapse the append-only judgment stream into one authoritative judgment
/// per subject.
///
/// The authoritative event is the one with the HIGHEST `insertion_seq`, and
/// `event_count` is how many events the subject has. Both are computed
/// without depending on the order the events arrive in: the fold is a
/// function of the event SET, so a batched read, a per-subject read and a
/// vacuum-reordered table all land on the same answer. `created_at` is never
/// compared — a backdated correction still supersedes what it corrects.
fn fold_judgment_stream(events: Vec<JudgmentEvent>) -> BTreeMap<String, EvalAdjudicationFacts> {
    let mut folded: BTreeMap<String, EvalAdjudicationFacts> = BTreeMap::new();
    for event in events {
        match folded.get_mut(&event.subject_id) {
            Some(current) => {
                let event_count = current.event_count + 1;
                if event.insertion_seq >= current.insertion_seq {
                    *current = facts_of(event, event_count);
                } else {
                    current.event_count = event_count;
                }
            }
            None => {
                let subject_id = event.subject_id.clone();
                folded.insert(subject_id, facts_of(event, 1));
            }
        }
    }
    folded
}

fn facts_of(event: JudgmentEvent, event_count: usize) -> EvalAdjudicationFacts {
    EvalAdjudicationFacts {
        adjudication_id: event.adjudication_id,
        actor: event.actor,
        verdict: event.verdict,
        created_at: event.created_at,
        insertion_seq: event.insertion_seq,
        event_count,
    }
}

/// The whole dispatch-spine judgment log in ONE pass, oldest first per
/// subject. Deliberately not the incremental resolver's per-subject
/// `list_adjudications_for_outcome` (which re-reads each event by id, plus
/// its signatures): a replay reads the log, not one subject's slice of it.
fn read_dispatch_judgment_events(conn: &Connection) -> Result<Vec<JudgmentEvent>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT outcome_id, adjudication_id, actor, verdict, created_at, insertion_seq \
         FROM dispatch_adjudications ORDER BY outcome_id, insertion_seq",
    )?;
    let events = statement
        .query_map([], |row| {
            Ok(JudgmentEvent {
                subject_id: row.get(0)?,
                adjudication_id: row.get(1)?,
                actor: row.get(2)?,
                verdict: row.get(3)?,
                created_at: row.get(4)?,
                insertion_seq: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(events)
}

/// The mirror-spine judgment log in one pass. `usefulness` is this spine's
/// judgment slot — the same normalization the incremental resolver applies.
fn read_mirror_judgment_events(conn: &Connection) -> Result<Vec<JudgmentEvent>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT eval_run_id, adjudication_id, actor, usefulness, created_at, insertion_seq \
         FROM mirror_eval_adjudications ORDER BY eval_run_id, insertion_seq",
    )?;
    let events = statement
        .query_map([], |row| {
            Ok(JudgmentEvent {
                subject_id: row.get(0)?,
                adjudication_id: row.get(1)?,
                actor: row.get(2)?,
                verdict: Some(row.get::<_, String>(3)?),
                created_at: row.get(4)?,
                insertion_seq: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(events)
}

// ─── companion facts ───────────────────────────────────────────────────────

fn rubrics_by_adjudication(
    conn: &Connection,
    spine: EvalSpine,
) -> Result<BTreeMap<String, EvalRubricScoreRow>, MemoryError> {
    Ok(list_eval_rubric_scores(conn, spine.subject_kind())?
        .into_iter()
        .map(|row| (row.adjudication_id.clone(), row))
        .collect())
}

fn decisions_by_dispatch_id(
    conn: &Connection,
) -> Result<BTreeMap<String, RouteDecisionRow>, MemoryError> {
    Ok(list_route_decisions(conn)?
        .into_iter()
        .map(|row| (row.dispatch_id.clone(), row))
        .collect())
}

/// Only the recommendation rows the in-scope decisions actually reference.
/// `route_recommendations` is never deduplicated (every consult is its own
/// fact), so it is the one table here that grows without bound — loading it
/// whole to answer a bounded replay would be the wrong trade.
///
/// A referenced-but-absent recommendation is simply a miss, matching the
/// incremental resolver's `LEFT JOIN` null side exactly: no candidate set, no
/// recorded policy revision, never a reconstructed one.
fn recommendations_for(
    conn: &Connection,
    decisions: &BTreeMap<String, RouteDecisionRow>,
) -> Result<BTreeMap<String, RouteRecommendationRow>, MemoryError> {
    let referenced = decisions
        .values()
        .filter_map(|decision| decision.recommendation_id.clone())
        .collect::<BTreeSet<_>>();
    let mut out = BTreeMap::new();
    for recommendation_id in referenced {
        if let Some(row) = get_route_recommendation(conn, &recommendation_id)? {
            out.insert(recommendation_id, row);
        }
    }
    Ok(out)
}

/// Bind one acceptance-moment decision (plus the recommendation it cites, if
/// that row still exists) into the read-side route facts.
///
/// The candidate set and the policy revision come from the RECOMMENDATION
/// row — the recorded decision-time facts — never from live route policy.
fn route_facts(
    decision: &RouteDecisionRow,
    recommendation: Option<&RouteRecommendationRow>,
) -> EvalRouteFacts {
    EvalRouteFacts {
        route_decision_id: decision.route_decision_id.clone(),
        assignment_mode: decision.assignment_mode.clone(),
        override_flag: decision.override_flag,
        recommendation_id: decision.recommendation_id.clone(),
        recommended_profile: recommendation.and_then(|row| row.recommended_profile.clone()),
        candidate_profiles: recommendation
            .map(|row| candidate_profiles_from_value(&row.candidates))
            .unwrap_or_default(),
        policy_source_revision: recommendation.and_then(|row| row.policy_source_revision.clone()),
    }
}

// ─── canonical serialization ───────────────────────────────────────────────

/// Deterministic value for a row set: field names fixed here (not derived),
/// object keys sorted by [`canonicalize`].
///
/// Hand-written rather than `#[derive(Serialize)]` on purpose — this is a
/// comparison artifact two independent read paths are graded against, so a
/// new field must be added here consciously rather than appear in the
/// fixture the moment somebody widens a struct.
pub fn canonical_eval_observations(rows: &[EvalObservation]) -> Value {
    Value::Array(rows.iter().map(canonical_eval_observation).collect())
}

/// SHA-256 of the canonical row-set serialization.
pub fn eval_observations_digest(rows: &[EvalObservation]) -> String {
    digest_of(&canonical_eval_observations(rows))
}

fn canonical_eval_observation(row: &EvalObservation) -> Value {
    canonicalize(&json!({
        "spine": row.spine.as_str(),
        "subject_id": row.subject_id,
        "dispatch_id": row.dispatch_id,
        "profile": row.profile,
        "profile_attribution_basis": row.profile_attribution_basis.as_str(),
        "model": row.model,
        "vendor": row.vendor,
        "task_type": row.task_type,
        "terminal_outcome": row.terminal_outcome,
        "identity_attribution_basis": row.identity_attribution_basis,
        "cost_tokens": row.cost_tokens,
        "cost_usd": row.cost_usd,
        "duration_ms": row.duration_ms,
        "occurred_at": row.occurred_at,
        "occurred_at_basis": row.occurred_at_basis,
        "adjudication": row.adjudication.as_ref().map(|adjudication| json!({
            "adjudication_id": adjudication.adjudication_id,
            "actor": adjudication.actor,
            "verdict": adjudication.verdict,
            "created_at": adjudication.created_at,
            "insertion_seq": adjudication.insertion_seq,
            "event_count": adjudication.event_count,
            "is_overturn": adjudication.is_overturn(),
        })),
        "rubric": row.rubric.as_ref().map(|rubric| json!({
            "rubric_score_id": rubric.rubric_score_id,
            "adjudication_id": rubric.adjudication_id,
            "subject_kind": rubric.subject_kind,
            "rubric_hash": rubric.rubric_hash,
            "contract_correctness": rubric.contract_correctness,
            "evidence_quality": rubric.evidence_quality,
            "safety": rubric.safety,
            "scope_discipline": rubric.scope_discipline,
            "intervention_burden": rubric.intervention_burden,
            "completion_integrity": rubric.completion_integrity,
            "adjudication_confidence": rubric.adjudication_confidence,
            "adjudicator_actor": rubric.adjudicator_actor,
            "adjudicator_vendor": rubric.adjudicator_vendor,
            "independence_basis": rubric.independence_basis,
            "occurred_at": rubric.occurred_at,
            "recorded_at": rubric.recorded_at,
        })),
        "route": row.route.as_ref().map(|route| json!({
            "route_decision_id": route.route_decision_id,
            "assignment_mode": route.assignment_mode,
            "override_flag": route.override_flag,
            "recommendation_id": route.recommendation_id,
            "recommended_profile": route.recommended_profile,
            "candidate_profiles": route.candidate_profiles,
            "policy_source_revision": route.policy_source_revision,
        })),
    }))
}

/// Sort every object key, recursively. `serde_json`'s map happens to be
/// BTreeMap-backed in this workspace, but a canonical artifact must not
/// depend on a cargo feature staying off — this makes the ordering a
/// property of the code.
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        scalar => scalar.clone(),
    }
}

fn digest_of(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

#[cfg(test)]
mod tests;
