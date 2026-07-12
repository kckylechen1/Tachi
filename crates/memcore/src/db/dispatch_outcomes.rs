//! Canonical dispatch outcome ledger (#773 v4 sol carve).
//!
//! One append-only row per dispatch completion, written FIRST by the
//! `tachi_complete` seam (`tachi-server::complete_ops`) before any derived
//! row (eval memory, signature evidence, precedent). This is the future
//! router's real read surface: `vendor` + `created_at` window queries and
//! `issue_ref` lookups are the two shapes the design doc calls out — see
//! [`list_outcomes_by_vendor_window`] and [`list_outcomes_by_issue_ref`].
//!
//! This table holds mutable execution facts only. Adjudication evidence
//! (verdict, adjudicator, error signature) lives in the append-only
//! `dispatch_adjudications` table (#1035) — S2, not built here; a replayed
//! `complete` may rewrite any column on this row.
//!
//! No facade action is exposed yet (kept intentionally small per #757
//! economics — internal fns are the deliverable, not a new tool surface).
//! Graph-edge projection (seat->performed->dispatch, dispatch->produced->
//! outcome, etc.) is the S4 seat's job, stacked after this one; every ref a
//! row carries (issue_ref/pr_ref/flow_id/dispatch_id/eval_memory_id) is
//! present on the row so those edges can be derived later without a
//! re-migration.
//!
//! ## Idempotency
//!
//! `idempotency_key` is a UNIQUE column derived deterministically from
//! `(dispatch_id, task_type)` by [`derive_idempotency_key`]. Re-running
//! `tachi_complete` for the same dispatch_id+task_type upserts the existing
//! row in place (last write wins on the mutable fields) rather than
//! duplicating it — see [`upsert_outcome`].

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

/// Fields for one dispatch outcome row. `outcome_id` is caller-supplied
/// (callers mint a fresh id per logical write attempt); `idempotency_key`
/// dedupes re-completion of the same dispatch_id+task_type onto one row
/// regardless of how many `outcome_id`s were attempted.
#[derive(Debug, Clone, Default)]
pub struct NewDispatchOutcome {
    pub outcome_id: String,
    pub dispatch_id: String,
    pub eval_memory_id: Option<String>,
    pub model: Option<String>,
    pub vendor: String,
    pub role: Option<String>,
    pub seat: Option<String>,
    pub task_type: Option<String>,
    pub execution_outcome: String,
    pub retry_count: u32,
    pub error_class: Option<String>,
    pub issue_ref: Option<String>,
    pub pr_ref: Option<String>,
    pub flow_id: Option<String>,
    pub cost_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub verification_present: bool,
    pub diff_present: bool,
    /// JSON array of evidence references (files/issue refs/run artifacts).
    pub evidence_refs: Value,
}

/// A persisted row in `dispatch_outcomes`.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchOutcomeRow {
    pub outcome_id: String,
    pub dispatch_id: String,
    pub eval_memory_id: Option<String>,
    pub model: Option<String>,
    pub vendor: String,
    pub role: Option<String>,
    pub seat: Option<String>,
    pub task_type: Option<String>,
    pub execution_outcome: String,
    pub retry_count: u32,
    pub error_class: Option<String>,
    pub issue_ref: Option<String>,
    pub pr_ref: Option<String>,
    pub flow_id: Option<String>,
    pub cost_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub verification_present: bool,
    pub diff_present: bool,
    pub evidence_refs: Value,
    pub idempotency_key: String,
    pub created_at: String,
    pub updated_at: String,
}

const SELECT_COLUMNS: &str = "outcome_id, dispatch_id, eval_memory_id, model, vendor, role, seat, \
     task_type, execution_outcome, retry_count, \
     error_class, issue_ref, pr_ref, flow_id, cost_tokens, cost_usd, \
     verification_present, diff_present, evidence_refs, idempotency_key, created_at, updated_at";

fn row_to_outcome(row: &rusqlite::Row<'_>) -> Result<DispatchOutcomeRow, rusqlite::Error> {
    let evidence_refs_raw: String = row.get(18)?;
    let evidence_refs =
        serde_json::from_str(&evidence_refs_raw).unwrap_or_else(|_| Value::Array(Vec::new()));
    let retry_count: i64 = row.get(9)?;
    let cost_tokens: Option<i64> = row.get(14)?;
    Ok(DispatchOutcomeRow {
        outcome_id: row.get(0)?,
        dispatch_id: row.get(1)?,
        eval_memory_id: row.get(2)?,
        model: row.get(3)?,
        vendor: row.get(4)?,
        role: row.get(5)?,
        seat: row.get(6)?,
        task_type: row.get(7)?,
        execution_outcome: row.get(8)?,
        retry_count: retry_count.max(0) as u32,
        error_class: row.get(10)?,
        issue_ref: row.get(11)?,
        pr_ref: row.get(12)?,
        flow_id: row.get(13)?,
        cost_tokens: cost_tokens.map(|v| v.max(0) as u64),
        cost_usd: row.get(15)?,
        verification_present: row.get::<_, i64>(16)? != 0,
        diff_present: row.get::<_, i64>(17)? != 0,
        evidence_refs,
        idempotency_key: row.get(19)?,
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
    })
}

/// Deterministic idempotency key for a (dispatch_id, task_type) pair. Two
/// `tachi_complete` calls for the same dispatch and task_type collapse onto
/// one row; an empty/absent `task_type` is normalized to a stable sentinel
/// so it still dedupes rather than comparing unequal empty strings vs None.
pub fn derive_idempotency_key(dispatch_id: &str, task_type: Option<&str>) -> String {
    let task_type_norm = task_type
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("_untyped");
    format!("{dispatch_id}::{task_type_norm}")
}

/// Insert-or-update one outcome row keyed by `idempotency_key`
/// (`derive_idempotency_key(dispatch_id, task_type)`). Re-running `complete`
/// for the same dispatch_id+task_type updates the existing row in place
/// (same `outcome_id`, refreshed mutable fields, `updated_at` bumped)
/// instead of inserting a duplicate — the canonical-row contract is "one row
/// per logical completion", not "one row per call".
pub fn upsert_outcome(
    conn: &Connection,
    new: &NewDispatchOutcome,
) -> Result<DispatchOutcomeRow, MemoryError> {
    let idempotency_key = derive_idempotency_key(&new.dispatch_id, new.task_type.as_deref());
    let now = normalize_utc_iso_or_now("");
    let evidence_refs_json =
        serde_json::to_string(&new.evidence_refs).unwrap_or_else(|_| "[]".to_string());
    let retry_count = new.retry_count as i64;
    let cost_tokens = new.cost_tokens.map(|v| v as i64);

    let existing_id: Option<String> = conn
        .query_row(
            "SELECT outcome_id FROM dispatch_outcomes WHERE idempotency_key = ?1",
            params![idempotency_key],
            |r| r.get(0),
        )
        .optional()?;

    match existing_id {
        Some(outcome_id) => {
            conn.execute(
                "UPDATE dispatch_outcomes SET
                    eval_memory_id = ?2, model = ?3, vendor = ?4, role = ?5, seat = ?6,
                    task_type = ?7, execution_outcome = ?8, retry_count = ?9,
                    error_class = ?10, issue_ref = ?11, pr_ref = ?12, flow_id = ?13,
                    cost_tokens = ?14, cost_usd = ?15, verification_present = ?16,
                    diff_present = ?17, evidence_refs = ?18, updated_at = ?19
                 WHERE outcome_id = ?1",
                params![
                    outcome_id,
                    new.eval_memory_id,
                    new.model,
                    new.vendor,
                    new.role,
                    new.seat,
                    new.task_type,
                    new.execution_outcome,
                    retry_count,
                    new.error_class,
                    new.issue_ref,
                    new.pr_ref,
                    new.flow_id,
                    cost_tokens,
                    new.cost_usd,
                    new.verification_present as i64,
                    new.diff_present as i64,
                    evidence_refs_json,
                    now,
                ],
            )?;
            get_outcome(conn, &outcome_id)?.ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "dispatch_outcomes row {outcome_id} vanished immediately after update"
                ))
            })
        }
        None => {
            conn.execute(
                "INSERT INTO dispatch_outcomes
                 (outcome_id, dispatch_id, eval_memory_id, model, vendor, role, seat,
                  task_type, execution_outcome, retry_count, error_class, issue_ref,
                  pr_ref, flow_id, cost_tokens, cost_usd, verification_present,
                  diff_present, evidence_refs, idempotency_key, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?21)",
                params![
                    new.outcome_id,
                    new.dispatch_id,
                    new.eval_memory_id,
                    new.model,
                    new.vendor,
                    new.role,
                    new.seat,
                    new.task_type,
                    new.execution_outcome,
                    retry_count,
                    new.error_class,
                    new.issue_ref,
                    new.pr_ref,
                    new.flow_id,
                    cost_tokens,
                    new.cost_usd,
                    new.verification_present as i64,
                    new.diff_present as i64,
                    evidence_refs_json,
                    idempotency_key,
                    now,
                ],
            )?;
            get_outcome(conn, &new.outcome_id)?.ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "dispatch_outcomes row {} vanished immediately after insert",
                    new.outcome_id
                ))
            })
        }
    }
}

/// Fetch a single outcome row by its primary key.
pub fn get_outcome(
    conn: &Connection,
    outcome_id: &str,
) -> Result<Option<DispatchOutcomeRow>, MemoryError> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM dispatch_outcomes WHERE outcome_id = ?1");
    Ok(conn
        .query_row(&sql, params![outcome_id], row_to_outcome)
        .optional()?)
}

/// Read surface 1: outcomes for a vendor within a `created_at` window
/// (`[since, until)`, both RFC3339). `until = None` means unbounded upper
/// end. Newest first — the router's real query shape (vendor, ts).
pub fn list_outcomes_by_vendor_window(
    conn: &Connection,
    vendor: &str,
    since: &str,
    until: Option<&str>,
) -> Result<Vec<DispatchOutcomeRow>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM dispatch_outcomes \
         WHERE vendor = ?1 AND created_at >= ?2 AND (?3 IS NULL OR created_at < ?3) \
         ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![vendor, since, until], row_to_outcome)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Read surface 2: outcomes linked to a given `issue_ref`. Newest first.
pub fn list_outcomes_by_issue_ref(
    conn: &Connection,
    issue_ref: &str,
) -> Result<Vec<DispatchOutcomeRow>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM dispatch_outcomes \
         WHERE issue_ref = ?1 ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![issue_ref], row_to_outcome)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        libsimple::enable_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn new_outcome(outcome_id: &str, dispatch_id: &str) -> NewDispatchOutcome {
        NewDispatchOutcome {
            outcome_id: outcome_id.to_string(),
            dispatch_id: dispatch_id.to_string(),
            eval_memory_id: Some("eval-1".to_string()),
            model: Some("claude-sonnet-5".to_string()),
            vendor: "claude".to_string(),
            role: Some("implementer".to_string()),
            seat: Some("wizard".to_string()),
            task_type: Some("fix_request".to_string()),
            execution_outcome: "success".to_string(),
            retry_count: 0,
            error_class: None,
            issue_ref: Some("kckylechen1/tachi#773".to_string()),
            pr_ref: Some("kckylechen1/tachi#1020".to_string()),
            flow_id: Some("flow-1".to_string()),
            cost_tokens: Some(1234),
            cost_usd: Some(0.05),
            verification_present: true,
            diff_present: true,
            evidence_refs: serde_json::json!(["cargo test", "clippy"]),
        }
    }

    #[test]
    fn insert_and_get_roundtrip() {
        let conn = open_conn();
        let inserted = upsert_outcome(&conn, &new_outcome("o-1", "d-1")).unwrap();
        assert_eq!(inserted.outcome_id, "o-1");
        assert_eq!(inserted.dispatch_id, "d-1");
        assert_eq!(inserted.vendor, "claude");
        assert_eq!(inserted.retry_count, 0);
        assert_eq!(inserted.cost_tokens, Some(1234));
        assert!(inserted.verification_present);
        assert!(inserted.diff_present);
        assert_eq!(
            inserted.evidence_refs,
            serde_json::json!(["cargo test", "clippy"])
        );
        assert!(!inserted.created_at.is_empty());
        assert_eq!(inserted.created_at, inserted.updated_at);

        let got = get_outcome(&conn, "o-1").unwrap().expect("row present");
        assert_eq!(got, inserted);
    }

    #[test]
    fn idempotent_recomplete_updates_same_row_not_duplicate() {
        let conn = open_conn();
        let first = upsert_outcome(&conn, &new_outcome("o-1", "d-1")).unwrap();

        // Re-completing the same dispatch_id+task_type with a different
        // outcome_id (simulating a second `tachi_complete` call minting a
        // fresh id) must update the SAME row, not create a second one.
        let mut second_attempt = new_outcome("o-2", "d-1");
        second_attempt.execution_outcome = "failure".to_string();
        second_attempt.retry_count = 1;
        let second = upsert_outcome(&conn, &second_attempt).unwrap();

        assert_eq!(second.outcome_id, first.outcome_id, "same canonical row");
        assert_eq!(second.execution_outcome, "failure");
        assert_eq!(second.retry_count, 1);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dispatch_outcomes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "no duplicate row created");
    }

    #[test]
    fn different_task_type_is_a_distinct_row() {
        let conn = open_conn();
        upsert_outcome(&conn, &new_outcome("o-1", "d-1")).unwrap();
        let mut other_type = new_outcome("o-2", "d-1");
        other_type.task_type = Some("plan_request".to_string());
        upsert_outcome(&conn, &other_type).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dispatch_outcomes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "distinct task_type is a distinct logical outcome");
    }

    #[test]
    fn list_by_vendor_window_orders_newest_first_and_respects_bounds() {
        let conn = open_conn();
        let mut a = new_outcome("o-a", "d-a");
        a.vendor = "grok".to_string();
        upsert_outcome(&conn, &a).unwrap();
        conn.execute(
            "UPDATE dispatch_outcomes SET created_at = '2026-01-01T00:00:00Z' WHERE outcome_id = 'o-a'",
            [],
        )
        .unwrap();

        let mut b = new_outcome("o-b", "d-b");
        b.vendor = "grok".to_string();
        upsert_outcome(&conn, &b).unwrap();
        conn.execute(
            "UPDATE dispatch_outcomes SET created_at = '2026-06-01T00:00:00Z' WHERE outcome_id = 'o-b'",
            [],
        )
        .unwrap();

        let mut other_vendor = new_outcome("o-c", "d-c");
        other_vendor.vendor = "codex".to_string();
        upsert_outcome(&conn, &other_vendor).unwrap();

        let rows =
            list_outcomes_by_vendor_window(&conn, "grok", "2026-01-01T00:00:00Z", None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].outcome_id, "o-b", "newest first");
        assert_eq!(rows[1].outcome_id, "o-a");

        let bounded = list_outcomes_by_vendor_window(
            &conn,
            "grok",
            "2026-01-01T00:00:00Z",
            Some("2026-06-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(bounded.len(), 1, "upper bound excludes o-b");
        assert_eq!(bounded[0].outcome_id, "o-a");
    }

    #[test]
    fn list_by_issue_ref_filters_correctly() {
        let conn = open_conn();
        upsert_outcome(&conn, &new_outcome("o-1", "d-1")).unwrap();
        let mut other_issue = new_outcome("o-2", "d-2");
        other_issue.issue_ref = Some("kckylechen1/tachi#1".to_string());
        upsert_outcome(&conn, &other_issue).unwrap();

        let rows = list_outcomes_by_issue_ref(&conn, "kckylechen1/tachi#773").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome_id, "o-1");
    }

    #[test]
    fn get_missing_outcome_returns_none() {
        let conn = open_conn();
        assert!(get_outcome(&conn, "nope").unwrap().is_none());
    }
}
