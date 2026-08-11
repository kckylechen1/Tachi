//! Decision-fact ledger + structured rubric companion (tachi#1675 PR1).
//!
//! Three append-only tables, deliberately carrying **zero terminal/
//! execution-state columns** (design D1) — terminal truth remains solely
//! `status.json` + `dispatch_outcomes.execution_outcome`. See
//! `crates::db::schema::ddl::BASE_SCHEMA_CHUNKS` for the DDL and doc
//! comments; this module owns the Rust structs and access functions only.
//!
//! - [`route_recommendations`] — the recommendation moment (Seam A):
//!   `insert_route_recommendation` is a plain append (never deduplicated —
//!   every consult is a new fact).
//! - [`route_decisions`] — the acceptance moment (Seam B):
//!   `insert_route_decision_idempotent` is keyed UNIQUE on `dispatch_id`; a
//!   replay is a zero-write no-op that returns the existing row.
//! - [`eval_rubric_scores`] — the structured adjudication companion (D3):
//!   `insert_eval_rubric_score` is append-only, keyed UNIQUE on
//!   `(subject_kind, adjudication_id)`; there is deliberately no UPDATE/
//!   DELETE accessor — a corrected judgment is a NEW adjudication event
//!   (new `adjudication_id`) carrying its own new rubric row.
//!
//! None of these three access surfaces ever touch `session_claims`,
//! `agent_identities`, or any other permission/identity table.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

// ─── route_recommendations ─────────────────────────────────────────────────

/// Fields for one `route_recommendations` row. `occurred_at` is
/// caller-supplied (normalized) per the #1432 temporal contract (D4);
/// `recorded_at` is stamped by [`insert_route_recommendation`] itself.
#[derive(Debug, Clone)]
pub struct NewRouteRecommendation {
    pub recommendation_id: String,
    pub task_type: Option<String>,
    pub risk: String,
    /// Full scored candidate array (profile/agent/score/reasons/...)
    /// verbatim, serialized as JSON.
    pub candidates: Value,
    pub recommended_profile: Option<String>,
    /// Content-bearing route-policy snapshot hash
    /// (`route_policy_source_revision`), not a bare counter.
    pub policy_source_revision: Option<String>,
    pub rows_considered: u64,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteRecommendationRow {
    pub recommendation_id: String,
    pub task_type: Option<String>,
    pub risk: String,
    pub candidates: Value,
    pub recommended_profile: Option<String>,
    pub policy_source_revision: Option<String>,
    pub rows_considered: u64,
    pub occurred_at: String,
    pub recorded_at: String,
}

const ROUTE_RECOMMENDATION_COLUMNS: &str = "recommendation_id, task_type, risk, candidates, \
     recommended_profile, policy_source_revision, rows_considered, occurred_at, recorded_at";

fn row_to_recommendation(
    row: &rusqlite::Row<'_>,
) -> Result<RouteRecommendationRow, rusqlite::Error> {
    let candidates_raw: String = row.get(3)?;
    let candidates =
        serde_json::from_str(&candidates_raw).unwrap_or_else(|_| Value::Array(Vec::new()));
    let rows_considered: i64 = row.get(6)?;
    Ok(RouteRecommendationRow {
        recommendation_id: row.get(0)?,
        task_type: row.get(1)?,
        risk: row.get(2)?,
        candidates,
        recommended_profile: row.get(4)?,
        policy_source_revision: row.get(5)?,
        rows_considered: rows_considered.max(0) as u64,
        occurred_at: row.get(7)?,
        recorded_at: row.get(8)?,
    })
}

/// Append a `route_recommendations` row. Never deduplicated — Seam A calls
/// this every time `handle_dispatch_recommendation` runs, including replays
/// with identical content: each consult is its own fact, not a state
/// transition.
pub fn insert_route_recommendation(
    conn: &Connection,
    new: &NewRouteRecommendation,
) -> Result<RouteRecommendationRow, MemoryError> {
    let recorded_at = normalize_utc_iso_or_now("");
    let candidates_json =
        serde_json::to_string(&new.candidates).unwrap_or_else(|_| "[]".to_string());
    let rows_considered = new.rows_considered as i64;
    conn.execute(
        "INSERT INTO route_recommendations
         (recommendation_id, task_type, risk, candidates, recommended_profile,
          policy_source_revision, rows_considered, occurred_at, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            new.recommendation_id,
            new.task_type,
            new.risk,
            candidates_json,
            new.recommended_profile,
            new.policy_source_revision,
            rows_considered,
            new.occurred_at,
            recorded_at,
        ],
    )?;
    get_route_recommendation(conn, &new.recommendation_id)?.ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "route_recommendations row {} vanished immediately after insert",
            new.recommendation_id
        ))
    })
}

pub fn get_route_recommendation(
    conn: &Connection,
    recommendation_id: &str,
) -> Result<Option<RouteRecommendationRow>, MemoryError> {
    let sql =
        format!("SELECT {ROUTE_RECOMMENDATION_COLUMNS} FROM route_recommendations WHERE recommendation_id = ?1");
    Ok(conn
        .query_row(&sql, params![recommendation_id], row_to_recommendation)
        .optional()?)
}

// ─── route_decisions ────────────────────────────────────────────────────────

/// Closed vocabulary for `assignment_mode` (design D2/D1). PR1's Seam B only
/// ever writes `advised`/`unadvised`; `user_forced`/`experiment` are reserved
/// for a future explicit-override caller.
pub const ASSIGNMENT_MODES: &[&str] = &["advised", "unadvised", "user_forced", "experiment"];

#[derive(Debug, Clone)]
pub struct NewRouteDecision {
    pub route_decision_id: String,
    pub dispatch_id: String,
    pub recommendation_id: Option<String>,
    pub selected_profile: Option<String>,
    pub selected_model: Option<String>,
    pub assignment_mode: String,
    pub override_flag: bool,
    pub contract_hash: Option<String>,
    pub env_id: Option<String>,
    pub host_profile: Option<String>,
    /// Nullable pending #1239's v21 claim wiring (spec correction 1).
    pub work_claim_id: Option<String>,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteDecisionRow {
    pub route_decision_id: String,
    pub dispatch_id: String,
    pub recommendation_id: Option<String>,
    pub selected_profile: Option<String>,
    pub selected_model: Option<String>,
    pub assignment_mode: String,
    pub override_flag: bool,
    pub contract_hash: Option<String>,
    pub env_id: Option<String>,
    pub host_profile: Option<String>,
    pub work_claim_id: Option<String>,
    pub occurred_at: String,
    pub recorded_at: String,
}

const ROUTE_DECISION_COLUMNS: &str = "route_decision_id, dispatch_id, recommendation_id, \
     selected_profile, selected_model, assignment_mode, override_flag, contract_hash, \
     env_id, host_profile, work_claim_id, occurred_at, recorded_at";

fn row_to_decision(row: &rusqlite::Row<'_>) -> Result<RouteDecisionRow, rusqlite::Error> {
    Ok(RouteDecisionRow {
        route_decision_id: row.get(0)?,
        dispatch_id: row.get(1)?,
        recommendation_id: row.get(2)?,
        selected_profile: row.get(3)?,
        selected_model: row.get(4)?,
        assignment_mode: row.get(5)?,
        override_flag: row.get::<_, i64>(6)? != 0,
        contract_hash: row.get(7)?,
        env_id: row.get(8)?,
        host_profile: row.get(9)?,
        work_claim_id: row.get(10)?,
        occurred_at: row.get(11)?,
        recorded_at: row.get(12)?,
    })
}

/// Insert the acceptance-moment decision row, idempotent on `dispatch_id`
/// (UNIQUE): a replayed acceptance for the same `dispatch_id` is a
/// **zero-write** no-op that returns the existing row unchanged — this is
/// append-only-with-idempotency, never an update (design D2's "acknowledged
/// dual-write, not a transaction" framing: status.json is written first and
/// this insert follows in the same code path; a crash in between leaves no
/// row, and readers must treat a missing row as `assignment_mode =
/// 'unadvised'`, never fabricate one here).
pub fn insert_route_decision_idempotent(
    conn: &Connection,
    new: &NewRouteDecision,
) -> Result<RouteDecisionRow, MemoryError> {
    if !ASSIGNMENT_MODES.contains(&new.assignment_mode.as_str()) {
        return Err(MemoryError::InvalidArg(format!(
            "assignment_mode '{}' is not in the closed set {:?}",
            new.assignment_mode, ASSIGNMENT_MODES
        )));
    }
    let recorded_at = normalize_utc_iso_or_now("");
    conn.execute(
        "INSERT INTO route_decisions
         (route_decision_id, dispatch_id, recommendation_id, selected_profile,
          selected_model, assignment_mode, override_flag, contract_hash,
          env_id, host_profile, work_claim_id, occurred_at, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(dispatch_id) DO NOTHING",
        params![
            new.route_decision_id,
            new.dispatch_id,
            new.recommendation_id,
            new.selected_profile,
            new.selected_model,
            new.assignment_mode,
            new.override_flag as i64,
            new.contract_hash,
            new.env_id,
            new.host_profile,
            new.work_claim_id,
            new.occurred_at,
            recorded_at,
        ],
    )?;
    get_route_decision_by_dispatch_id(conn, &new.dispatch_id)?.ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "route_decisions row for dispatch_id {} vanished immediately after insert",
            new.dispatch_id
        ))
    })
}

pub fn get_route_decision_by_dispatch_id(
    conn: &Connection,
    dispatch_id: &str,
) -> Result<Option<RouteDecisionRow>, MemoryError> {
    let sql =
        format!("SELECT {ROUTE_DECISION_COLUMNS} FROM route_decisions WHERE dispatch_id = ?1");
    Ok(conn
        .query_row(&sql, params![dispatch_id], row_to_decision)
        .optional()?)
}

/// Every acceptance-moment decision row, ordered by `dispatch_id` for a
/// deterministic read.
///
/// Added for the replay reader (tachi#1675 PR3), which binds route facts in
/// Rust from a batched read instead of through the incremental resolver's
/// SQL `LEFT JOIN`. Unbounded by design: this table holds at most one row per
/// dispatch that ever accepted (`UNIQUE(dispatch_id)`), which is the same
/// order of magnitude the joined path already scans, and a full replay is
/// defined over the whole ledger rather than a window of it.
pub fn list_route_decisions(conn: &Connection) -> Result<Vec<RouteDecisionRow>, MemoryError> {
    let sql = format!("SELECT {ROUTE_DECISION_COLUMNS} FROM route_decisions ORDER BY dispatch_id");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map([], row_to_decision)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─── eval_rubric_scores ─────────────────────────────────────────────────────

pub const RUBRIC_DIMENSION_VALUES: &[&str] = &["pass", "concern", "fail", "not_assessed"];
pub const RUBRIC_CONFIDENCE_VALUES: &[&str] = &["low", "medium", "high"];
pub const RUBRIC_SUBJECT_KINDS: &[&str] = &["dispatch", "mirror"];
/// `identity_bound` is included in the DDL CHECK (a reserved future value,
/// design D5) but [`insert_eval_rubric_score`] refuses to write it — no
/// caller can reach `identity_bound` in this phase.
pub const RUBRIC_INDEPENDENCE_BASIS_VALUES: &[&str] = &[
    "structural_cross_vendor",
    "declared_only",
    "self",
    "identity_bound",
];

#[derive(Debug, Clone)]
pub struct NewEvalRubricScore {
    pub rubric_score_id: String,
    pub adjudication_id: String,
    pub subject_kind: String,
    pub rubric_hash: String,
    pub contract_correctness: String,
    pub evidence_quality: String,
    pub safety: String,
    pub scope_discipline: String,
    pub intervention_burden: String,
    pub completion_integrity: String,
    pub adjudication_confidence: String,
    pub adjudicator_actor: String,
    pub adjudicator_vendor: String,
    pub independence_basis: String,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvalRubricScoreRow {
    pub rubric_score_id: String,
    pub adjudication_id: String,
    pub subject_kind: String,
    pub rubric_hash: String,
    pub contract_correctness: String,
    pub evidence_quality: String,
    pub safety: String,
    pub scope_discipline: String,
    pub intervention_burden: String,
    pub completion_integrity: String,
    pub adjudication_confidence: String,
    pub adjudicator_actor: String,
    pub adjudicator_vendor: String,
    pub independence_basis: String,
    pub occurred_at: String,
    pub recorded_at: String,
}

const EVAL_RUBRIC_SCORE_COLUMNS: &str = "rubric_score_id, adjudication_id, subject_kind, \
     rubric_hash, contract_correctness, evidence_quality, safety, scope_discipline, \
     intervention_burden, completion_integrity, adjudication_confidence, adjudicator_actor, \
     adjudicator_vendor, independence_basis, occurred_at, recorded_at";

fn row_to_rubric_score(row: &rusqlite::Row<'_>) -> Result<EvalRubricScoreRow, rusqlite::Error> {
    Ok(EvalRubricScoreRow {
        rubric_score_id: row.get(0)?,
        adjudication_id: row.get(1)?,
        subject_kind: row.get(2)?,
        rubric_hash: row.get(3)?,
        contract_correctness: row.get(4)?,
        evidence_quality: row.get(5)?,
        safety: row.get(6)?,
        scope_discipline: row.get(7)?,
        intervention_burden: row.get(8)?,
        completion_integrity: row.get(9)?,
        adjudication_confidence: row.get(10)?,
        adjudicator_actor: row.get(11)?,
        adjudicator_vendor: row.get(12)?,
        independence_basis: row.get(13)?,
        occurred_at: row.get(14)?,
        recorded_at: row.get(15)?,
    })
}

/// Append a structured rubric row. UNIQUE(subject_kind, adjudication_id) at
/// the schema level means a second attempt to write a rubric row for the
/// SAME adjudication event fails (surfaces as a rusqlite constraint error,
/// never silently ignored or overwritten) — an overturn/correction must mint
/// a NEW adjudication event (new `adjudication_id` on the underlying
/// `dispatch_adjudications`/`mirror_eval_adjudications` append-only table)
/// and carry its OWN new rubric row. There is deliberately no update/delete
/// accessor in this module.
pub fn insert_eval_rubric_score(
    conn: &Connection,
    new: &NewEvalRubricScore,
) -> Result<EvalRubricScoreRow, MemoryError> {
    for (label, value, allowed) in [
        (
            "subject_kind",
            new.subject_kind.as_str(),
            RUBRIC_SUBJECT_KINDS,
        ),
        (
            "contract_correctness",
            new.contract_correctness.as_str(),
            RUBRIC_DIMENSION_VALUES,
        ),
        (
            "evidence_quality",
            new.evidence_quality.as_str(),
            RUBRIC_DIMENSION_VALUES,
        ),
        ("safety", new.safety.as_str(), RUBRIC_DIMENSION_VALUES),
        (
            "scope_discipline",
            new.scope_discipline.as_str(),
            RUBRIC_DIMENSION_VALUES,
        ),
        (
            "intervention_burden",
            new.intervention_burden.as_str(),
            RUBRIC_DIMENSION_VALUES,
        ),
        (
            "completion_integrity",
            new.completion_integrity.as_str(),
            RUBRIC_DIMENSION_VALUES,
        ),
        (
            "adjudication_confidence",
            new.adjudication_confidence.as_str(),
            RUBRIC_CONFIDENCE_VALUES,
        ),
        (
            "independence_basis",
            new.independence_basis.as_str(),
            RUBRIC_INDEPENDENCE_BASIS_VALUES,
        ),
    ] {
        if !allowed.contains(&value) {
            return Err(MemoryError::InvalidArg(format!(
                "{label} '{value}' is not in the closed set {:?}",
                allowed
            )));
        }
    }
    // D5: `identity_bound` is a reserved enum value that activates only once
    // #1239 wires v21 claims through adjudication identity — no writer in
    // this phase may produce it, even though the DDL CHECK allows it for
    // forward compatibility.
    if new.independence_basis == "identity_bound" {
        return Err(MemoryError::InvalidArg(
            "independence_basis 'identity_bound' is reserved and cannot be written in this phase"
                .to_string(),
        ));
    }
    if new.adjudicator_actor.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "adjudicator_actor must not be empty".to_string(),
        ));
    }

    let recorded_at = normalize_utc_iso_or_now("");
    conn.execute(
        "INSERT INTO eval_rubric_scores
         (rubric_score_id, adjudication_id, subject_kind, rubric_hash,
          contract_correctness, evidence_quality, safety, scope_discipline,
          intervention_burden, completion_integrity, adjudication_confidence,
          adjudicator_actor, adjudicator_vendor, independence_basis, occurred_at, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            new.rubric_score_id,
            new.adjudication_id,
            new.subject_kind,
            new.rubric_hash,
            new.contract_correctness,
            new.evidence_quality,
            new.safety,
            new.scope_discipline,
            new.intervention_burden,
            new.completion_integrity,
            new.adjudication_confidence,
            new.adjudicator_actor,
            new.adjudicator_vendor,
            new.independence_basis,
            new.occurred_at,
            recorded_at,
        ],
    )?;
    get_eval_rubric_score(conn, &new.subject_kind, &new.adjudication_id)?.ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "eval_rubric_scores row for adjudication_id {} vanished immediately after insert",
            new.adjudication_id
        ))
    })
}

pub fn get_eval_rubric_score(
    conn: &Connection,
    subject_kind: &str,
    adjudication_id: &str,
) -> Result<Option<EvalRubricScoreRow>, MemoryError> {
    let sql = format!(
        "SELECT {EVAL_RUBRIC_SCORE_COLUMNS} FROM eval_rubric_scores \
         WHERE subject_kind = ?1 AND adjudication_id = ?2"
    );
    Ok(conn
        .query_row(
            &sql,
            params![subject_kind, adjudication_id],
            row_to_rubric_score,
        )
        .optional()?)
}

/// Every rubric row for one spine, ordered by `adjudication_id` for a
/// deterministic read.
///
/// Added for the replay reader (tachi#1675 PR3): the incremental resolver
/// looks a rubric row up per authoritative adjudication event, while replay
/// folds the whole judgment stream and needs the companion rows in one pass.
/// `UNIQUE(subject_kind, adjudication_id)` means the returned rows are keyed
/// one-to-one by `adjudication_id` within a spine.
pub fn list_eval_rubric_scores(
    conn: &Connection,
    subject_kind: &str,
) -> Result<Vec<EvalRubricScoreRow>, MemoryError> {
    let sql = format!(
        "SELECT {EVAL_RUBRIC_SCORE_COLUMNS} FROM eval_rubric_scores \
         WHERE subject_kind = ?1 ORDER BY adjudication_id"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params![subject_kind], row_to_rubric_score)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn new_recommendation(id: &str) -> NewRouteRecommendation {
        NewRouteRecommendation {
            recommendation_id: id.to_string(),
            task_type: Some("fix_request".to_string()),
            risk: "low".to_string(),
            candidates: serde_json::json!([
                {"profile": "wizard_sonnet", "score": 0.9, "reasons": ["live_eval_weighted"]},
                {"profile": "codex_55_review", "score": 0.4, "reasons": ["fallback"]},
            ]),
            recommended_profile: Some("wizard_sonnet".to_string()),
            policy_source_revision: Some("policyrev-abc".to_string()),
            rows_considered: 12,
            occurred_at: normalize_utc_iso_or_now(""),
        }
    }

    fn new_decision(dispatch_id: &str) -> NewRouteDecision {
        NewRouteDecision {
            route_decision_id: format!("rd-{dispatch_id}"),
            dispatch_id: dispatch_id.to_string(),
            recommendation_id: None,
            selected_profile: Some("wizard_sonnet".to_string()),
            selected_model: Some("claude-sonnet-5".to_string()),
            assignment_mode: "unadvised".to_string(),
            override_flag: false,
            contract_hash: Some("contracthash-1".to_string()),
            env_id: Some("env-1".to_string()),
            host_profile: Some("dev".to_string()),
            work_claim_id: None,
            occurred_at: normalize_utc_iso_or_now(""),
        }
    }

    fn new_rubric(adjudication_id: &str, subject_kind: &str) -> NewEvalRubricScore {
        NewEvalRubricScore {
            rubric_score_id: format!("rs-{adjudication_id}"),
            adjudication_id: adjudication_id.to_string(),
            subject_kind: subject_kind.to_string(),
            rubric_hash: "rubric-v1-hash".to_string(),
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
            occurred_at: normalize_utc_iso_or_now(""),
        }
    }

    #[test]
    fn route_recommendation_insert_and_get_roundtrip() {
        let conn = open_conn();
        let inserted = insert_route_recommendation(&conn, &new_recommendation("rec-1")).unwrap();
        assert_eq!(inserted.recommendation_id, "rec-1");
        assert_eq!(inserted.risk, "low");
        assert_eq!(inserted.rows_considered, 12);
        assert_eq!(
            inserted.recommended_profile.as_deref(),
            Some("wizard_sonnet")
        );
        assert!(!inserted.recorded_at.is_empty());

        let got = get_route_recommendation(&conn, "rec-1")
            .unwrap()
            .expect("row present");
        assert_eq!(got, inserted);
    }

    /// Every consult is a new fact: two recommendation calls with byte-identical
    /// content produce TWO independent rows, never deduplicated.
    #[test]
    fn route_recommendation_is_never_deduplicated() {
        let conn = open_conn();
        insert_route_recommendation(&conn, &new_recommendation("rec-a")).unwrap();
        let mut second = new_recommendation("rec-a-2");
        second.candidates = new_recommendation("rec-a").candidates;
        insert_route_recommendation(&conn, &second).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM route_recommendations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 2, "each consult is an independent, un-deduped fact");
    }

    #[test]
    fn route_decision_insert_and_get_roundtrip() {
        let conn = open_conn();
        let inserted = insert_route_decision_idempotent(&conn, &new_decision("d-1")).unwrap();
        assert_eq!(inserted.dispatch_id, "d-1");
        assert_eq!(inserted.assignment_mode, "unadvised");
        assert!(!inserted.override_flag);
        assert!(!inserted.recorded_at.is_empty());

        let got = get_route_decision_by_dispatch_id(&conn, "d-1")
            .unwrap()
            .expect("row present");
        assert_eq!(got, inserted);
    }

    /// Idempotency: a replayed acceptance for the SAME dispatch_id is a
    /// zero-write no-op — the SECOND call's differing fields are discarded,
    /// the original row (and row count) is untouched.
    #[test]
    fn route_decision_replay_is_zero_write_idempotent() {
        let conn = open_conn();
        let first = insert_route_decision_idempotent(&conn, &new_decision("d-2")).unwrap();

        let mut replay = new_decision("d-2");
        replay.route_decision_id = "rd-different-id".to_string();
        replay.selected_profile = Some("some_other_profile".to_string());
        replay.assignment_mode = "advised".to_string();
        let second = insert_route_decision_idempotent(&conn, &replay).unwrap();

        assert_eq!(
            second, first,
            "replay must return the EXISTING row, not the differing replay payload"
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM route_decisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "replay writes zero additional rows");
    }

    #[test]
    fn route_decision_rejects_assignment_mode_outside_closed_set() {
        let conn = open_conn();
        let mut bad = new_decision("d-3");
        bad.assignment_mode = "auto_yolo".to_string();
        let err = insert_route_decision_idempotent(&conn, &bad).unwrap_err();
        assert!(err.to_string().contains("assignment_mode"));

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM route_decisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "no row written for a rejected assignment_mode");
    }

    /// NULL-recommendation is legal: `staff_start` does not always consult
    /// `recommend` first (design D2).
    #[test]
    fn route_decision_allows_null_recommendation_id() {
        let conn = open_conn();
        let decision = new_decision("d-null-rec");
        assert!(decision.recommendation_id.is_none());
        let row = insert_route_decision_idempotent(&conn, &decision).unwrap();
        assert!(row.recommendation_id.is_none());
    }

    #[test]
    fn eval_rubric_score_insert_and_get_roundtrip() {
        let conn = open_conn();
        let inserted = insert_eval_rubric_score(&conn, &new_rubric("adj-1", "mirror")).unwrap();
        assert_eq!(inserted.adjudication_id, "adj-1");
        assert_eq!(inserted.subject_kind, "mirror");
        assert_eq!(inserted.independence_basis, "structural_cross_vendor");
        assert!(!inserted.recorded_at.is_empty());

        let got = get_eval_rubric_score(&conn, "mirror", "adj-1")
            .unwrap()
            .expect("row present");
        assert_eq!(got, inserted);
    }

    /// A second rubric row for the SAME (subject_kind, adjudication_id) is
    /// rejected outright (UNIQUE constraint) — an overturn must use a NEW
    /// adjudication_id, never overwrite the old rubric row.
    #[test]
    fn eval_rubric_score_rejects_second_row_for_same_adjudication() {
        let conn = open_conn();
        insert_eval_rubric_score(&conn, &new_rubric("adj-2", "mirror")).unwrap();
        let mut second = new_rubric("adj-2", "mirror");
        second.rubric_score_id = "rs-adj-2-second".to_string();
        second.contract_correctness = "fail".to_string();
        let err = insert_eval_rubric_score(&conn, &second).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("unique")
                || err.to_string().to_lowercase().contains("constraint"),
            "expected a UNIQUE constraint violation, got: {err}"
        );

        let existing = get_eval_rubric_score(&conn, "mirror", "adj-2")
            .unwrap()
            .expect("original row still present");
        assert_eq!(
            existing.contract_correctness, "pass",
            "original row must be unchanged by the rejected second write"
        );
    }

    /// Overturn: a genuinely NEW adjudication event (a different
    /// adjudication_id) appends its OWN rubric row without touching the
    /// prior one.
    #[test]
    fn eval_rubric_score_overturn_appends_new_row_without_rewriting_old() {
        let conn = open_conn();
        let original = insert_eval_rubric_score(&conn, &new_rubric("adj-3", "dispatch")).unwrap();

        let mut overturn = new_rubric("adj-3-correction", "dispatch");
        overturn.contract_correctness = "fail".to_string();
        let corrected = insert_eval_rubric_score(&conn, &overturn).unwrap();

        let still_original = get_eval_rubric_score(&conn, "dispatch", "adj-3")
            .unwrap()
            .expect("original row untouched");
        assert_eq!(still_original, original);
        assert_eq!(still_original.contract_correctness, "pass");
        assert_eq!(corrected.contract_correctness, "fail");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM eval_rubric_scores", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "overturn appends, does not overwrite");
    }

    /// The four `independence_basis` values each construct a valid row
    /// except the reserved `identity_bound`, which is refused in this phase.
    #[test]
    fn eval_rubric_score_independence_basis_four_values() {
        let conn = open_conn();
        for (i, basis) in ["structural_cross_vendor", "declared_only", "self"]
            .iter()
            .enumerate()
        {
            let mut row = new_rubric(&format!("adj-basis-{i}"), "mirror");
            row.independence_basis = basis.to_string();
            let inserted = insert_eval_rubric_score(&conn, &row).unwrap();
            assert_eq!(&inserted.independence_basis, basis);
        }

        let mut reserved = new_rubric("adj-basis-reserved", "mirror");
        reserved.independence_basis = "identity_bound".to_string();
        let err = insert_eval_rubric_score(&conn, &reserved).unwrap_err();
        assert!(err.to_string().contains("identity_bound"));
    }

    #[test]
    fn eval_rubric_score_rejects_dimension_outside_closed_set() {
        let conn = open_conn();
        let mut bad = new_rubric("adj-bad-dim", "mirror");
        bad.safety = "great".to_string();
        let err = insert_eval_rubric_score(&conn, &bad).unwrap_err();
        assert!(err.to_string().contains("safety"));

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM eval_rubric_scores", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "no row written for a rejected dimension value");
    }

    /// Negative test: none of the three write paths touch `session_claims`
    /// or `agent_identities` — those row counts are unchanged by driving all
    /// three inserts.
    #[test]
    fn writes_never_touch_session_or_identity_tables() {
        let conn = open_conn();
        let before_claims: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_claims", [], |r| r.get(0))
            .unwrap();
        let before_identities: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_identities", [], |r| r.get(0))
            .unwrap();

        insert_route_recommendation(&conn, &new_recommendation("rec-neg")).unwrap();
        insert_route_decision_idempotent(&conn, &new_decision("d-neg")).unwrap();
        insert_eval_rubric_score(&conn, &new_rubric("adj-neg", "mirror")).unwrap();

        let after_claims: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_claims", [], |r| r.get(0))
            .unwrap();
        let after_identities: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_identities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before_claims, after_claims);
        assert_eq!(before_identities, after_identities);
    }
}
