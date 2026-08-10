//! Read-side spine unification for the decision-fact ledger (tachi#1675 PR2,
//! design D1's closing clause: "Spine unification is read-side").
//!
//! ONE Rust row type — [`EvalObservation`] — tagged with the source spine it
//! was resolved from:
//!
//! - [`EvalSpine::Dispatch`]: `dispatch_outcomes` ⋈ `dispatch_adjudications`
//!   ⋈ `eval_rubric_scores` ⋈ `route_decisions` (⋈ `route_recommendations`
//!   for the decision-time candidate set).
//! - [`EvalSpine::Mirror`]: `mirror_eval_runs` ⋈ `mirror_eval_observations`
//!   ⋈ `mirror_eval_adjudications` ⋈ `eval_rubric_scores`.
//!
//! **Mirror rows are permanently off-policy for routing** (design D1 +
//! spec correction 7): the mirror spine has no route decision and no
//! candidate set — not "missing", *structurally absent* — so
//! [`EvalObservation::route`] is always `None` for them and no code path in
//! this module may ever synthesize one. They carry per-candidate QUALITY
//! evidence only, and only where a profile identity is attributable at all.
//!
//! This module is strictly READ-ONLY: it contains no `INSERT`/`UPDATE`/
//! `DELETE` and no write accessor. Decision rules (usability, abstain,
//! lexicographic tiers) deliberately live OUTSIDE it, in the server's
//! projection layer — memcore owns the join, not the policy.
//!
//! Two honest gaps are encoded in the types rather than papered over:
//! - `duration_ms` is `None` for every dispatch-spine row: `dispatch_outcomes`
//!   has no duration column (design D3 / codex finding 3). Only mirror
//!   observations carry one. Adding the column is a separate, explicit ALTER
//!   decision — this resolver must not fabricate a latency.
//! - `occurred_at` on both spines is mapped from the legacy `created_at`
//!   (design D4: existing tables are NOT backfilled, because backfilling
//!   fabricates event times), and every row says so via
//!   [`EvalObservation::occurred_at_basis`].

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::error::MemoryError;

use super::dispatch_adjudications::list_adjudications_for_outcome;
use super::mirror_eval::list_adjudications_for_run;
use super::route_eval::{get_eval_rubric_score, EvalRubricScoreRow};

/// `occurred_at` was read off a legacy `created_at` column, not a real
/// caller-supplied event time (design D4: no backfill).
pub const OCCURRED_AT_BASIS_LEGACY_CREATED_AT: &str = "legacy_created_at";

/// Which ledger spine a row was resolved from. The `(spine, id)` pair — not
/// any single id space — is the achievable unification (spec correction 2:
/// the dispatch spine keys on `dispatch_id`, the mirror spine on its own run
/// id, and there is no shared id space between them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalSpine {
    Dispatch,
    Mirror,
}

impl EvalSpine {
    pub fn as_str(self) -> &'static str {
        match self {
            EvalSpine::Dispatch => "dispatch",
            EvalSpine::Mirror => "mirror",
        }
    }

    /// `eval_rubric_scores.subject_kind` for this spine.
    pub fn subject_kind(self) -> &'static str {
        self.as_str()
    }
}

/// How a row's candidate (profile) identity was attributed. A reader must be
/// able to tell an acceptance-moment fact from a planned intent — the #1065
/// pattern applied to profile attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileAttributionBasis {
    /// `route_decisions.selected_profile`: the acceptance-moment fact.
    RouteDecision,
    /// Carrier-OBSERVED profile from the frozen identity receipt.
    ObservedReceipt,
    /// PLANNED (unconfirmed) profile from the frozen identity receipt.
    PlannedReceipt,
    /// Mirror spine `requested_profile`: register-time REQUESTED intent, never
    /// carrier-confirmed. Usable for quality attribution only — mirror rows
    /// are off-policy for routing regardless.
    MirrorRequested,
    /// No profile identity anywhere on the row.
    Unattributed,
}

impl ProfileAttributionBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileAttributionBasis::RouteDecision => "route_decision",
            ProfileAttributionBasis::ObservedReceipt => "observed_receipt",
            ProfileAttributionBasis::PlannedReceipt => "planned_receipt",
            ProfileAttributionBasis::MirrorRequested => "mirror_requested",
            ProfileAttributionBasis::Unattributed => "unattributed",
        }
    }
}

/// The authoritative (last-appended) adjudication event for a subject, plus
/// how many events precede it. `event_count > 1` means the current judgment
/// is an OVERTURN of an earlier one — the settling-window input of the
/// abstain rule (design D6).
#[derive(Debug, Clone, PartialEq)]
pub struct EvalAdjudicationFacts {
    pub adjudication_id: String,
    pub actor: String,
    /// Dispatch spine: free-text `verdict`. Mirror spine: `usefulness`.
    pub verdict: Option<String>,
    pub created_at: String,
    pub insertion_seq: i64,
    /// Total adjudication events for this subject; `> 1` ⇒ this one is an
    /// overturn/correction.
    pub event_count: usize,
}

impl EvalAdjudicationFacts {
    pub fn is_overturn(&self) -> bool {
        self.event_count > 1
    }
}

/// Routing facts bound to a dispatch-spine row. Structurally absent on the
/// mirror spine — never synthesized.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalRouteFacts {
    pub route_decision_id: String,
    pub assignment_mode: String,
    pub override_flag: bool,
    pub recommendation_id: Option<String>,
    pub recommended_profile: Option<String>,
    /// Profile names from the decision-time candidate array. EMPTY when no
    /// recommendation row is linked (`assignment_mode = 'unadvised'`): there
    /// was no candidate set, which is a fact, not a gap to fill.
    pub candidate_profiles: Vec<String>,
    pub policy_source_revision: Option<String>,
}

impl EvalRouteFacts {
    /// Whether `profile` was in the decision-time candidate set. `false` when
    /// no candidate set was recorded at all.
    pub fn candidate_set_contains(&self, profile: &str) -> bool {
        self.candidate_profiles.iter().any(|p| p == profile)
    }
}

/// One unified evidence row, spine-tagged.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalObservation {
    pub spine: EvalSpine,
    /// `(spine, subject_id)` is the unified key: `outcome_id` on the dispatch
    /// spine, `eval_run_id` on the mirror spine.
    pub subject_id: String,
    /// Dispatch spine only.
    pub dispatch_id: Option<String>,
    /// Candidate identity for routing/quality attribution.
    pub profile: Option<String>,
    pub profile_attribution_basis: ProfileAttributionBasis,
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub task_type: Option<String>,
    /// Dispatch: `execution_outcome` (machine-resolved). Mirror:
    /// `terminal_outcome` from the (at most one) observation. `None` = no
    /// terminal state recorded at all.
    pub terminal_outcome: Option<String>,
    /// Dispatch spine only (#1065 option D).
    pub identity_attribution_basis: Option<String>,
    pub cost_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    /// ALWAYS `None` on the dispatch spine — no such column exists there
    /// (design D3 / codex finding 3).
    pub duration_ms: Option<u64>,
    pub occurred_at: String,
    pub occurred_at_basis: &'static str,
    pub adjudication: Option<EvalAdjudicationFacts>,
    pub rubric: Option<EvalRubricScoreRow>,
    /// `None` for every mirror row, by construction.
    pub route: Option<EvalRouteFacts>,
}

impl EvalObservation {
    /// Whether this row can carry candidate-set-level (on-policy) routing
    /// evidence at all. Mirror rows are permanently `false`.
    pub fn binds_decision_candidate_set(&self) -> bool {
        match (&self.profile, &self.route) {
            (Some(profile), Some(route)) => route.candidate_set_contains(profile),
            _ => false,
        }
    }
}

fn candidate_profiles_from_json(raw: Option<String>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    candidate_profiles_from_value(&value)
}

/// Profile names out of a recorded candidate array. Anything that is not an
/// array of objects carrying a string `profile` yields an EMPTY set — a
/// malformed or absent candidate array means "no candidate set was recorded",
/// never a reconstructed guess.
///
/// `pub(crate)` so the replay reader (`super::eval_replay`), which holds the
/// already-parsed `route_recommendations.candidates` value rather than its raw
/// column text, extracts candidates through the SAME rule the incremental
/// resolver uses. Two extractors would be an equivalence bug waiting to
/// happen.
pub(crate) fn candidate_profiles_from_value(candidates: &Value) -> Vec<String> {
    let Value::Array(items) = candidates else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            item.get("profile")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn receipt_profile(receipt: Option<&Value>, pointer: &str) -> Option<String> {
    receipt
        .and_then(|value| value.pointer(pointer))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "unknown")
        .map(str::to_string)
}

/// One adjudication event, spine-normalized (the dispatch spine's free-text
/// `verdict` and the mirror spine's `usefulness` land in the same slot).
struct AdjudicationEvent {
    adjudication_id: String,
    actor: String,
    verdict: Option<String>,
    created_at: String,
    insertion_seq: i64,
}

/// Attach the authoritative adjudication + its rubric companion to a subject.
fn resolve_judgment(
    conn: &Connection,
    spine: EvalSpine,
    events: Vec<AdjudicationEvent>,
) -> Result<(Option<EvalAdjudicationFacts>, Option<EvalRubricScoreRow>), MemoryError> {
    let event_count = events.len();
    // The LAST appended event is the current judgment (the #1035/#1066
    // append-only convention); an earlier event's rubric row must never stand
    // in for a correction that carries none — that is exactly the
    // `unstructured_verdict` case the projection is required to exclude.
    let Some(AdjudicationEvent {
        adjudication_id,
        actor,
        verdict,
        created_at,
        insertion_seq,
    }) = events.into_iter().next_back()
    else {
        return Ok((None, None));
    };
    let rubric = get_eval_rubric_score(conn, spine.subject_kind(), &adjudication_id)?;
    Ok((
        Some(EvalAdjudicationFacts {
            adjudication_id,
            actor,
            verdict,
            created_at,
            insertion_seq,
            event_count,
        }),
        rubric,
    ))
}

/// Base execution facts for ONE dispatch-spine subject, before any judgment
/// or route binding is attached.
///
/// Shared by the incremental resolver below and the replay reader in
/// [`super::eval_replay`] (design D6's "full replay ≡ incremental
/// projection"). The two paths differ ONLY where an append-only stream is
/// collapsed into current state — the judgment fold and the route binding.
/// The base column mapping is deliberately SHARED, not duplicated: a second
/// copy of it would drift, and an equivalence test written over two copies
/// grades the copy instead of the fold it is supposed to be testing.
#[derive(Debug, Clone)]
pub(crate) struct DispatchSubjectRow {
    pub(crate) outcome_id: String,
    pub(crate) dispatch_id: String,
    pub(crate) model: Option<String>,
    pub(crate) vendor: Option<String>,
    pub(crate) task_type: Option<String>,
    pub(crate) execution_outcome: String,
    pub(crate) identity_attribution_basis: String,
    pub(crate) cost_tokens: Option<u64>,
    pub(crate) cost_usd: Option<f64>,
    pub(crate) identity_receipt: Option<Value>,
    pub(crate) created_at: String,
}

/// The dispatch-spine base columns, in the exact order
/// [`dispatch_subject_from_row`] reads them (indices 0..=10). ONE column list
/// behind both the incremental resolver's joined query and the replay
/// reader's unjoined one.
const DISPATCH_SUBJECT_COLUMNS: &str = "o.outcome_id, o.dispatch_id, o.model, o.vendor, \
     o.task_type, o.execution_outcome, o.identity_attribution_basis, o.cost_tokens, o.cost_usd, \
     o.identity_receipt, o.created_at";

/// The window/order/limit tail, identical on both paths so a replay and an
/// incremental read over the same window select the SAME subject rows in the
/// SAME order before either of them attaches a judgment.
const DISPATCH_WINDOW_TAIL: &str =
    "WHERE o.created_at >= ?1 AND (?2 IS NULL OR o.created_at < ?2) \
     ORDER BY o.created_at DESC, o.outcome_id DESC LIMIT ?3";

fn dispatch_subject_from_row(
    row: &rusqlite::Row<'_>,
) -> Result<DispatchSubjectRow, rusqlite::Error> {
    let cost_tokens: Option<i64> = row.get(7)?;
    let identity_receipt = row
        .get::<_, Option<String>>(9)?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    Ok(DispatchSubjectRow {
        outcome_id: row.get(0)?,
        dispatch_id: row.get(1)?,
        model: row.get(2)?,
        vendor: row.get(3)?,
        task_type: row.get(4)?,
        execution_outcome: row.get(5)?,
        identity_attribution_basis: row.get(6)?,
        cost_tokens: cost_tokens.map(|v| v.max(0) as u64),
        cost_usd: row.get(8)?,
        identity_receipt,
        created_at: row.get(10)?,
    })
}

/// Dispatch-spine base rows in `[since, until)`, newest first — no judgment,
/// no route binding. The replay reader's entry point into the same subject
/// set the incremental resolver sees.
pub(crate) fn list_dispatch_subject_rows(
    conn: &Connection,
    since: &str,
    until: Option<&str>,
    limit: usize,
) -> Result<Vec<DispatchSubjectRow>, MemoryError> {
    let sql = format!(
        "SELECT {DISPATCH_SUBJECT_COLUMNS} FROM dispatch_outcomes o {DISPATCH_WINDOW_TAIL}"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(
            params![since, until, limit as i64],
            dispatch_subject_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Assemble one dispatch-spine [`EvalObservation`] from its base facts plus
/// whatever judgment and route binding the caller resolved. Shared by both
/// read paths — including the attribution ladder, which is the one place
/// allowed to decide what a row's candidate identity IS.
pub(crate) fn dispatch_observation(
    subject: DispatchSubjectRow,
    selected_profile: Option<String>,
    route: Option<EvalRouteFacts>,
    adjudication: Option<EvalAdjudicationFacts>,
    rubric: Option<EvalRubricScoreRow>,
) -> EvalObservation {
    // Attribution ladder, strongest first: the acceptance-moment decision
    // fact, then the carrier-OBSERVED receipt identity, then the PLANNED one.
    // Never invent a profile from a mutable profile definition.
    let (profile, profile_attribution_basis) =
        if let Some(profile) = selected_profile.filter(|profile| !profile.trim().is_empty()) {
            (Some(profile), ProfileAttributionBasis::RouteDecision)
        } else if let Some(profile) = receipt_profile(
            subject.identity_receipt.as_ref(),
            "/observed/effective/profile",
        ) {
            (Some(profile), ProfileAttributionBasis::ObservedReceipt)
        } else if let Some(profile) =
            receipt_profile(subject.identity_receipt.as_ref(), "/planned/profile")
        {
            (Some(profile), ProfileAttributionBasis::PlannedReceipt)
        } else {
            (None, ProfileAttributionBasis::Unattributed)
        };

    EvalObservation {
        spine: EvalSpine::Dispatch,
        subject_id: subject.outcome_id,
        dispatch_id: Some(subject.dispatch_id),
        profile,
        profile_attribution_basis,
        model: subject.model,
        vendor: subject.vendor,
        task_type: subject.task_type,
        terminal_outcome: Some(subject.execution_outcome)
            .filter(|outcome| !outcome.trim().is_empty()),
        identity_attribution_basis: Some(subject.identity_attribution_basis),
        cost_tokens: subject.cost_tokens,
        cost_usd: subject.cost_usd,
        // Structurally absent on this spine — never fabricated.
        duration_ms: None,
        occurred_at: subject.created_at,
        occurred_at_basis: OCCURRED_AT_BASIS_LEGACY_CREATED_AT,
        adjudication,
        rubric,
        route,
    }
}

/// Dispatch-spine observations whose `created_at` falls in `[since, until)`,
/// newest first. `until = None` means unbounded upper end.
pub fn list_dispatch_eval_observations(
    conn: &Connection,
    since: &str,
    until: Option<&str>,
    limit: usize,
) -> Result<Vec<EvalObservation>, MemoryError> {
    struct Raw {
        subject: DispatchSubjectRow,
        selected_profile: Option<String>,
        route: Option<EvalRouteFacts>,
    }

    let sql = format!(
        "SELECT {DISPATCH_SUBJECT_COLUMNS}, \
         d.route_decision_id, d.recommendation_id, d.selected_profile, d.selected_model, \
         d.assignment_mode, d.override_flag, \
         r.candidates, r.recommended_profile, r.policy_source_revision \
         FROM dispatch_outcomes o \
         LEFT JOIN route_decisions d ON d.dispatch_id = o.dispatch_id \
         LEFT JOIN route_recommendations r ON r.recommendation_id = d.recommendation_id \
         {DISPATCH_WINDOW_TAIL}"
    );
    let mut statement = conn.prepare(&sql)?;
    let raws = statement
        .query_map(params![since, until, limit as i64], |row| {
            let route_decision_id: Option<String> = row.get(11)?;
            let recommendation_id: Option<String> = row.get(12)?;
            let selected_profile: Option<String> = row.get(13)?;
            let assignment_mode: Option<String> = row.get(15)?;
            let override_flag: Option<i64> = row.get(16)?;
            let candidates: Option<String> = row.get(17)?;
            let recommended_profile: Option<String> = row.get(18)?;
            let policy_source_revision: Option<String> = row.get(19)?;
            // The LEFT JOIN's null side is the honest "no route decision was
            // recorded" state (design D2: a crash between the FS-atomic
            // status.json write and this insert leaves no row, and the
            // reader's rule for a missing row is `unadvised` — never a
            // fabricated decision).
            let route = route_decision_id.map(|route_decision_id| EvalRouteFacts {
                route_decision_id,
                assignment_mode: assignment_mode.unwrap_or_else(|| "unadvised".to_string()),
                override_flag: override_flag.unwrap_or(0) != 0,
                recommendation_id,
                recommended_profile,
                candidate_profiles: candidate_profiles_from_json(candidates),
                policy_source_revision,
            });
            Ok(Raw {
                subject: dispatch_subject_from_row(row)?,
                selected_profile,
                route,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(raws.len());
    for raw in raws {
        let events = dispatch_adjudication_events(conn, &raw.subject.outcome_id)?;
        let (adjudication, rubric) = resolve_judgment(conn, EvalSpine::Dispatch, events)?;
        out.push(dispatch_observation(
            raw.subject,
            raw.selected_profile,
            raw.route,
            adjudication,
            rubric,
        ));
    }
    Ok(out)
}

fn dispatch_adjudication_events(
    conn: &Connection,
    outcome_id: &str,
) -> Result<Vec<AdjudicationEvent>, MemoryError> {
    Ok(list_adjudications_for_outcome(conn, outcome_id)?
        .into_iter()
        .map(|event| AdjudicationEvent {
            adjudication_id: event.adjudication_id,
            actor: event.actor,
            verdict: event.verdict,
            created_at: event.created_at,
            insertion_seq: event.insertion_seq,
        })
        .collect())
}

const MIRROR_PROJECTION_SQL: &str = "SELECT \
     r.eval_run_id, r.requested_profile, r.requested_model, r.harness, r.created_at, \
     o.terminal_outcome, o.duration_ms, o.cost_tokens, o.cost_usd, o.effective_model, \
     o.effective_backend \
     FROM mirror_eval_runs r \
     LEFT JOIN mirror_eval_observations o ON o.eval_run_id = r.eval_run_id \
     WHERE r.created_at >= ?1 AND (?2 IS NULL OR r.created_at < ?2) \
     ORDER BY r.created_at DESC, r.eval_run_id DESC \
     LIMIT ?3";

/// Base facts for ONE mirror-spine subject (its run row plus the at-most-one
/// observation row), before any judgment is attached. Shared with
/// [`super::eval_replay`] for the same reason as [`DispatchSubjectRow`].
#[derive(Debug, Clone)]
pub(crate) struct MirrorSubjectRow {
    pub(crate) eval_run_id: String,
    pub(crate) requested_profile: Option<String>,
    pub(crate) requested_model: Option<String>,
    pub(crate) harness: Option<String>,
    pub(crate) created_at: String,
    pub(crate) terminal_outcome: Option<String>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) cost_tokens: Option<u64>,
    pub(crate) cost_usd: Option<f64>,
    pub(crate) effective_model: Option<String>,
}

/// Mirror-spine base rows in `[since, until)`, newest first — no judgment
/// attached. `mirror_eval_observations.eval_run_id` is UNIQUE, so the LEFT
/// JOIN is at most one-to-one and this row set is a function of the window
/// alone.
pub(crate) fn list_mirror_subject_rows(
    conn: &Connection,
    since: &str,
    until: Option<&str>,
    limit: usize,
) -> Result<Vec<MirrorSubjectRow>, MemoryError> {
    let mut statement = conn.prepare(MIRROR_PROJECTION_SQL)?;
    let rows = statement
        .query_map(params![since, until, limit as i64], |row| {
            let duration_ms: Option<i64> = row.get(6)?;
            let cost_tokens: Option<i64> = row.get(7)?;
            Ok(MirrorSubjectRow {
                eval_run_id: row.get(0)?,
                requested_profile: row.get(1)?,
                requested_model: row.get(2)?,
                harness: row.get(3)?,
                created_at: row.get(4)?,
                terminal_outcome: row
                    .get::<_, Option<String>>(5)?
                    .filter(|outcome| !outcome.trim().is_empty()),
                duration_ms: duration_ms.map(|v| v.max(0) as u64),
                cost_tokens: cost_tokens.map(|v| v.max(0) as u64),
                cost_usd: row.get(8)?,
                effective_model: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Assemble one mirror-spine [`EvalObservation`]. `route` is not a parameter
/// at all: the mirror spine has no route decision and no candidate set, and
/// giving this function somewhere to put one is exactly how a future caller
/// would end up fabricating one (design D1 / spec correction 7).
pub(crate) fn mirror_observation(
    subject: MirrorSubjectRow,
    adjudication: Option<EvalAdjudicationFacts>,
    rubric: Option<EvalRubricScoreRow>,
) -> EvalObservation {
    let profile = subject
        .requested_profile
        .filter(|profile| !profile.trim().is_empty());
    let profile_attribution_basis = if profile.is_some() {
        ProfileAttributionBasis::MirrorRequested
    } else {
        ProfileAttributionBasis::Unattributed
    };
    EvalObservation {
        spine: EvalSpine::Mirror,
        subject_id: subject.eval_run_id,
        dispatch_id: None,
        profile,
        profile_attribution_basis,
        // The carrier-OBSERVED effective model when one exists; the
        // register-time requested value is never laundered into the observed
        // slot (the #1066 gating precedent).
        model: subject.effective_model.or(subject.requested_model),
        vendor: subject.harness,
        task_type: None,
        terminal_outcome: subject.terminal_outcome,
        identity_attribution_basis: None,
        cost_tokens: subject.cost_tokens,
        cost_usd: subject.cost_usd,
        duration_ms: subject.duration_ms,
        occurred_at: subject.created_at,
        occurred_at_basis: OCCURRED_AT_BASIS_LEGACY_CREATED_AT,
        adjudication,
        rubric,
        // Structural, not missing: there is no mirror candidate set.
        route: None,
    }
}

/// Mirror-spine observations in `[since, until)`, newest first.
///
/// Every returned row has `route: None` — the mirror spine has no route
/// decision and no candidate set to bind (design D1 / spec correction 7).
pub fn list_mirror_eval_observations(
    conn: &Connection,
    since: &str,
    until: Option<&str>,
    limit: usize,
) -> Result<Vec<EvalObservation>, MemoryError> {
    let subjects = list_mirror_subject_rows(conn, since, until, limit)?;
    let mut out = Vec::with_capacity(subjects.len());
    for subject in subjects {
        let events = list_adjudications_for_run(conn, &subject.eval_run_id)?
            .into_iter()
            .map(|event| AdjudicationEvent {
                adjudication_id: event.adjudication_id,
                actor: event.actor,
                // The mirror spine's judgment slot is `usefulness`.
                verdict: Some(event.usefulness),
                created_at: event.created_at,
                insertion_seq: event.insertion_seq,
            })
            .collect::<Vec<_>>();
        let (adjudication, rubric) = resolve_judgment(conn, EvalSpine::Mirror, events)?;
        out.push(mirror_observation(subject, adjudication, rubric));
    }
    Ok(out)
}

/// Which recorded route-policy revisions a set of rows was interpreted under
/// (tachi#1675 PR3 / spec correction 5).
///
/// Route-policy rules live in a MUTABLE key-value table, so "this evidence is
/// replayable" is only true if each row carries the content-bearing policy
/// snapshot hash that was live when its recommendation was made. This census
/// is how a reader sees which policy states its evidence actually spans —
/// and, next to the LIVE revision, whether the rules have moved underneath it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyRevisionCensus {
    /// Recorded `policy_source_revision` → number of rows carrying it.
    pub revisions: std::collections::BTreeMap<String, usize>,
    /// Rows with no recorded revision at all: an `unadvised` acceptance, a
    /// decision whose recommendation row is gone, or a mirror row (which
    /// structurally has neither). Counted, never defaulted to the live one.
    pub rows_without_revision: usize,
}

/// Census the recorded policy revisions of a row set. A pure function of the
/// rows — deliberately no store handle, so it can never reach for the live
/// route-policy state to fill a gap.
pub fn policy_revision_census(rows: &[EvalObservation]) -> PolicyRevisionCensus {
    let mut census = PolicyRevisionCensus::default();
    for row in rows {
        match row
            .route
            .as_ref()
            .and_then(|route| route.policy_source_revision.as_deref())
        {
            Some(revision) => *census.revisions.entry(revision.to_string()).or_insert(0) += 1,
            None => census.rows_without_revision += 1,
        }
    }
    census
}

/// The ONE total order both read paths publish rows in: newest first, ties
/// broken by `(spine, subject_id)` so the same data always yields the same
/// row sequence, then capped at `limit` overall.
///
/// Shared with [`super::eval_replay`] deliberately: a replay that ordered its
/// output differently would fail the equivalence fixture for a reason that
/// has nothing to do with the fold under test.
pub(crate) fn sort_observations_newest_first(rows: &mut Vec<EvalObservation>, limit: usize) {
    rows.sort_by(|a, b| {
        b.occurred_at
            .cmp(&a.occurred_at)
            .then_with(|| a.spine.as_str().cmp(b.spine.as_str()))
            .then_with(|| a.subject_id.cmp(&b.subject_id))
    });
    rows.truncate(limit);
}

/// Both spines unified, newest first, capped at `limit` rows overall.
pub fn list_eval_observations(
    conn: &Connection,
    since: &str,
    until: Option<&str>,
    limit: usize,
) -> Result<Vec<EvalObservation>, MemoryError> {
    let mut rows = list_dispatch_eval_observations(conn, since, until, limit)?;
    rows.extend(list_mirror_eval_observations(conn, since, until, limit)?);
    sort_observations_newest_first(&mut rows, limit);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::mirror_eval::{
        append_mirror_eval_adjudication, record_mirror_eval_observation, register_mirror_eval_run,
        NewMirrorEvalAdjudication, NewMirrorEvalObservation, NewMirrorEvalRun,
    };
    use crate::db::route_eval::{
        insert_eval_rubric_score, insert_route_decision_idempotent, insert_route_recommendation,
        NewEvalRubricScore, NewRouteDecision, NewRouteRecommendation,
    };

    fn open_conn() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn seed_outcome(conn: &Connection, outcome_id: &str, dispatch_id: &str, created_at: &str) {
        conn.execute(
            "INSERT INTO dispatch_outcomes \
             (outcome_id, dispatch_id, vendor, task_type, execution_outcome, \
              identity_attribution_basis, cost_tokens, cost_usd, idempotency_key, created_at, updated_at) \
             VALUES (?1, ?2, 'claude', 'fix_request', 'completed', 'observed', 1000, 0.5, ?1, ?3, ?3)",
            params![outcome_id, dispatch_id, created_at],
        )
        .unwrap();
    }

    fn seed_adjudication(conn: &Connection, adjudication_id: &str, outcome_id: &str, seq: i64) {
        conn.execute(
            "INSERT INTO dispatch_adjudications \
             (adjudication_id, outcome_id, event_key, verdict, actor, evidence_ref, created_at, insertion_seq) \
             VALUES (?1, ?2, ?1, 'APPROVED', 'leader', 'evidence://x', '2026-08-01T00:00:00.000Z', ?3)",
            params![adjudication_id, outcome_id, seq],
        )
        .unwrap();
    }

    fn rubric(adjudication_id: &str, subject_kind: &str) -> NewEvalRubricScore {
        NewEvalRubricScore {
            rubric_score_id: format!("rs-{adjudication_id}"),
            adjudication_id: adjudication_id.to_string(),
            subject_kind: subject_kind.to_string(),
            rubric_hash: "rubric-v1".to_string(),
            contract_correctness: "pass".to_string(),
            evidence_quality: "pass".to_string(),
            safety: "pass".to_string(),
            scope_discipline: "pass".to_string(),
            intervention_burden: "not_assessed".to_string(),
            completion_integrity: "pass".to_string(),
            adjudication_confidence: "high".to_string(),
            adjudicator_actor: "leader".to_string(),
            adjudicator_vendor: "codex".to_string(),
            independence_basis: "structural_cross_vendor".to_string(),
            occurred_at: "2026-08-01T00:00:00.000Z".to_string(),
        }
    }

    /// The dispatch spine resolves outcomes ⋈ adjudications ⋈ rubric ⋈
    /// route_decisions ⋈ route_recommendations into ONE row, and the
    /// decision-time candidate set comes back with it.
    #[test]
    fn dispatch_spine_join_carries_route_and_rubric() {
        let conn = open_conn();
        seed_outcome(&conn, "out-1", "disp-1", "2026-08-01T00:00:00.000Z");
        insert_route_recommendation(
            &conn,
            &NewRouteRecommendation {
                recommendation_id: "rec-1".to_string(),
                task_type: Some("fix_request".to_string()),
                risk: "medium".to_string(),
                candidates: serde_json::json!([
                    {"profile": "wizard_sonnet", "score": 9.0},
                    {"profile": "codex_55_review", "score": 4.0},
                ]),
                recommended_profile: Some("wizard_sonnet".to_string()),
                policy_source_revision: Some("rev-1".to_string()),
                rows_considered: 3,
                occurred_at: "2026-08-01T00:00:00.000Z".to_string(),
            },
        )
        .unwrap();
        insert_route_decision_idempotent(
            &conn,
            &NewRouteDecision {
                route_decision_id: "rd-1".to_string(),
                dispatch_id: "disp-1".to_string(),
                recommendation_id: Some("rec-1".to_string()),
                selected_profile: Some("wizard_sonnet".to_string()),
                selected_model: Some("anthropic/claude-sonnet".to_string()),
                assignment_mode: "advised".to_string(),
                override_flag: false,
                contract_hash: None,
                env_id: None,
                host_profile: None,
                work_claim_id: None,
                occurred_at: "2026-08-01T00:00:00.000Z".to_string(),
            },
        )
        .unwrap();
        seed_adjudication(&conn, "adj-1", "out-1", 1);
        insert_eval_rubric_score(&conn, &rubric("adj-1", "dispatch")).unwrap();

        let rows =
            list_dispatch_eval_observations(&conn, "2026-07-01T00:00:00.000Z", None, 50).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.spine, EvalSpine::Dispatch);
        assert_eq!(row.profile.as_deref(), Some("wizard_sonnet"));
        assert_eq!(
            row.profile_attribution_basis,
            ProfileAttributionBasis::RouteDecision
        );
        assert_eq!(row.terminal_outcome.as_deref(), Some("completed"));
        assert!(row.rubric.is_some());
        assert!(row.binds_decision_candidate_set());
        let route = row.route.as_ref().expect("route facts");
        assert_eq!(route.assignment_mode, "advised");
        assert!(route.candidate_set_contains("codex_55_review"));
        // codex finding 3: no duration column on this spine, ever.
        assert_eq!(row.duration_ms, None);
        assert_eq!(row.occurred_at_basis, OCCURRED_AT_BASIS_LEGACY_CREATED_AT);
    }

    /// An overturn: the LATEST adjudication is authoritative, and an earlier
    /// event's rubric row never stands in for a correction that carries none.
    #[test]
    fn latest_adjudication_wins_and_its_missing_rubric_is_not_backfilled() {
        let conn = open_conn();
        seed_outcome(&conn, "out-2", "disp-2", "2026-08-01T00:00:00.000Z");
        seed_adjudication(&conn, "adj-first", "out-2", 1);
        insert_eval_rubric_score(&conn, &rubric("adj-first", "dispatch")).unwrap();
        seed_adjudication(&conn, "adj-overturn", "out-2", 2);

        let rows =
            list_dispatch_eval_observations(&conn, "2026-07-01T00:00:00.000Z", None, 50).unwrap();
        let row = &rows[0];
        let adjudication = row.adjudication.as_ref().expect("adjudication");
        assert_eq!(adjudication.adjudication_id, "adj-overturn");
        assert!(adjudication.is_overturn());
        assert!(
            row.rubric.is_none(),
            "the correction carries no rubric row; the superseded one must not be reused"
        );
    }

    /// Mirror rows never carry route facts — the negative that keeps a future
    /// implementer from fabricating a mirror candidate set.
    #[test]
    fn mirror_rows_never_carry_route_facts() {
        let conn = open_conn();
        let run = register_mirror_eval_run(
            &conn,
            &NewMirrorEvalRun {
                frozen_contract_ref: "kckylechen1/tachi#1675".to_string(),
                execution_origin: "host_native_subagent".to_string(),
                lifecycle_owner: "host".to_string(),
                harness: Some("claude_code_task_tool".to_string()),
                native_child_id: Some("child-1".to_string()),
                requested_profile: Some("wizard_sonnet".to_string()),
                requested_model: Some("anthropic/claude-sonnet".to_string()),
                requested_agent: Some("claude".to_string()),
            },
        )
        .unwrap();
        record_mirror_eval_observation(
            &conn,
            &NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "completed".to_string(),
                duration_ms: Some(4242),
                cost_tokens: Some(900),
                cost_usd: Some(0.2),
                effective_model: Some("anthropic/claude-sonnet".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let adjudication = append_mirror_eval_adjudication(
            &conn,
            &NewMirrorEvalAdjudication {
                adjudication_id: "madj-1".to_string(),
                eval_run_id: run.eval_run_id.clone(),
                event_key: "madj-1".to_string(),
                actor: "leader".to_string(),
                usefulness: "useful".to_string(),
                evidence_usable: true,
                evidence_ref: "evidence://mirror".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        insert_eval_rubric_score(&conn, &rubric(&adjudication.adjudication_id, "mirror")).unwrap();

        let rows =
            list_mirror_eval_observations(&conn, "2020-01-01T00:00:00.000Z", None, 50).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.spine, EvalSpine::Mirror);
        assert!(row.route.is_none(), "mirror spine has no route decision");
        assert!(!row.binds_decision_candidate_set());
        // The mirror spine is the ONLY one carrying a real latency fact.
        assert_eq!(row.duration_ms, Some(4242));
        assert!(row.rubric.is_some());
        assert_eq!(
            row.profile_attribution_basis,
            ProfileAttributionBasis::MirrorRequested
        );
    }

    /// The unified list is spine-tagged, deterministic, and window-bounded.
    #[test]
    fn unified_list_is_window_bounded_and_deterministic() {
        let conn = open_conn();
        seed_outcome(&conn, "out-old", "disp-old", "2026-01-01T00:00:00.000Z");
        seed_outcome(&conn, "out-new", "disp-new", "2026-08-05T00:00:00.000Z");

        let rows = list_eval_observations(&conn, "2026-08-01T00:00:00.000Z", None, 50).unwrap();
        assert_eq!(rows.len(), 1, "the out-of-window row is not returned");
        assert_eq!(rows[0].subject_id, "out-new");

        let again = list_eval_observations(&conn, "2026-08-01T00:00:00.000Z", None, 50).unwrap();
        assert_eq!(rows, again, "same input, same output row order");
    }
}
