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
//! `complete` may rewrite its mutable execution columns. The optional
//! `identity_receipt` is frozen on its first write so a later replay cannot
//! reinterpret an already-executed dispatch through changed profile metadata.
//!
//! ## Truthfulness: reported vs machine-resolved (#773 Layer-2 ②)
//!
//! Two outcome columns keep the row honest about self-report vs machine fact:
//! `reported_outcome` is the agent's raw `tachi_complete` claim verbatim,
//! while `execution_outcome` is the MACHINE-RESOLVED terminal value — the
//! value after the #878-A completion predicate has had its chance to
//! intercept a false `success` (routing it to `failed`), or the terminal
//! state a non-`tachi_complete` path reached (backend/preflight/watchdog/
//! cancel). A row where `reported_outcome='success'` but
//! `execution_outcome='failed'` is exactly the false-success the router must
//! learn to distrust. Terminal paths that never carried a self-report leave
//! `reported_outcome` NULL. This is still an execution FACT (what the machine
//! observed), distinct from the S2 adjudication VERDICT above.
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
#[derive(Debug, Clone)]
pub struct NewDispatchOutcome {
    pub outcome_id: String,
    pub dispatch_id: String,
    pub eval_memory_id: Option<String>,
    pub model: Option<String>,
    pub vendor: String,
    pub role: Option<String>,
    pub seat: Option<String>,
    pub task_type: Option<String>,
    /// Machine-resolved terminal verdict for this dispatch — the value AFTER
    /// the completion predicate (#878-A) has had a chance to intercept a false
    /// self-report, or the terminal state a non-`tachi_complete` path reached
    /// (backend/preflight/watchdog/cancel). NOT the raw self-report; that lives
    /// in [`Self::reported_outcome`]. Vocabulary: `completed`/`failed`/
    /// `aborted`/`partial` (the `normalize_dispatch_outcome` kanban vocab).
    pub execution_outcome: String,
    /// The raw self-reported outcome as the agent stated it at
    /// `tachi_complete` (e.g. `success`/`failure`/`partial`/`aborted`), kept
    /// verbatim so a false `success` intercepted into `execution_outcome=
    /// 'failed'` is still auditable against what was claimed. `None` for
    /// terminal paths that never carried a self-report (a backend/preflight/
    /// watchdog/cancel failure the agent never `tachi_complete`d).
    pub reported_outcome: Option<String>,
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
    /// The dispatch-time identity receipt, copied verbatim from the run ledger.
    /// Legacy rows legitimately carry `None`.
    pub identity_receipt: Option<Value>,
    /// Evidence class of the flat identity columns (#1065 D):
    /// `planned_unconfirmed` | `acknowledged_overlay` | `observed` |
    /// `fallback_unreceipted` | `unknown`. Callers derive it from the receipt
    /// (`tachi_dispatch::IdentityAttributionBasis::as_str`); memcore stores it
    /// verbatim and freezes it with the receipt.
    pub identity_attribution_basis: String,
}

/// Hand-written (#1065 D BUG-2): a derived `Default` would give
/// `identity_attribution_basis` an empty string, which is not a member of
/// the closed vocabulary the DDL default (`'unknown'`) and every reader
/// (`OutcomeEvidenceClass::sql_predicate`, the runtime attribution match)
/// assume. Every other field keeps its ordinary zero value; only the basis
/// is special-cased to the vocabulary's own "no evidence" member.
impl Default for NewDispatchOutcome {
    fn default() -> Self {
        Self {
            outcome_id: Default::default(),
            dispatch_id: Default::default(),
            eval_memory_id: Default::default(),
            model: Default::default(),
            vendor: Default::default(),
            role: Default::default(),
            seat: Default::default(),
            task_type: Default::default(),
            execution_outcome: Default::default(),
            reported_outcome: Default::default(),
            retry_count: Default::default(),
            error_class: Default::default(),
            issue_ref: Default::default(),
            pr_ref: Default::default(),
            flow_id: Default::default(),
            cost_tokens: Default::default(),
            cost_usd: Default::default(),
            verification_present: Default::default(),
            diff_present: Default::default(),
            evidence_refs: Value::Null,
            identity_receipt: Default::default(),
            identity_attribution_basis: "unknown".to_string(),
        }
    }
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
    pub reported_outcome: Option<String>,
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
    pub identity_receipt: Option<Value>,
    pub identity_attribution_basis: String,
    pub idempotency_key: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Evidence-class filter for outcome read surfaces (#1065 D). Readers must
/// state what attribution evidence is sufficient for their purpose instead of
/// silently conflating planned routing intent with observed execution fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeEvidenceClass {
    /// Any attribution, including planned-only and profile fallback.
    AnyAttribution,
    /// Only rows whose identity a carrier acknowledged (overlay or observed).
    CarrierAcknowledged,
    /// Only rows carrying carrier-observed identity verbatim.
    ObservedOnly,
}

impl OutcomeEvidenceClass {
    fn sql_predicate(&self) -> &'static str {
        match self {
            OutcomeEvidenceClass::AnyAttribution => "1=1",
            OutcomeEvidenceClass::CarrierAcknowledged => {
                "identity_attribution_basis IN ('acknowledged_overlay', 'observed')"
            }
            OutcomeEvidenceClass::ObservedOnly => "identity_attribution_basis = 'observed'",
        }
    }
}

// `reported_outcome` and `identity_receipt` are appended LAST (indices 22/23) so the pre-existing column
// indices in `row_to_outcome` stay stable when the truthfulness column (#773)
// was added — a positional shift of the older columns would have been an easy
// off-by-one hazard.
const SELECT_COLUMNS: &str = "outcome_id, dispatch_id, eval_memory_id, model, vendor, role, seat, \
     task_type, execution_outcome, retry_count, \
     error_class, issue_ref, pr_ref, flow_id, cost_tokens, cost_usd, \
     verification_present, diff_present, evidence_refs, idempotency_key, created_at, updated_at, \
      reported_outcome, identity_receipt, identity_attribution_basis";

fn row_to_outcome(row: &rusqlite::Row<'_>) -> Result<DispatchOutcomeRow, rusqlite::Error> {
    let evidence_refs_raw: String = row.get(18)?;
    let evidence_refs =
        serde_json::from_str(&evidence_refs_raw).unwrap_or_else(|_| Value::Array(Vec::new()));
    let identity_receipt = row
        .get::<_, Option<String>>(23)?
        .and_then(|raw| serde_json::from_str(&raw).ok());
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
        reported_outcome: row.get(22)?,
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
        identity_receipt,
        identity_attribution_basis: row.get(24)?,
        idempotency_key: row.get(19)?,
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
    })
}

/// Closed-vocabulary gate for `identity_attribution_basis` at the write
/// boundary (#1065 D BUG-2). A caller building `NewDispatchOutcome` by hand
/// (or via a stale/incomplete construction) can hand this an empty string or
/// an arbitrary value; the DDL default (`'unknown'`) only protects a column
/// SQLite never received a value for, not one explicitly bound to garbage.
/// Every write path binds through this instead of `new.identity_attribution_basis`
/// directly, so the five-token vocabulary is enforced at the ONE place all
/// writes funnel through, not re-validated (or missed) at each call site.
fn normalize_basis(raw: &str) -> &str {
    match raw {
        "planned_unconfirmed"
        | "acknowledged_overlay"
        | "observed"
        | "fallback_unreceipted"
        | "unknown" => raw,
        _ => "unknown",
    }
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

/// Update the mutable fields of an existing row in place, targeting it by
/// `outcome_id` directly (bypassing idempotency-key derivation/lookup).
/// Factored out of [`upsert_outcome`]'s UPDATE branch so
/// [`upsert_outcome_reconciling_terminal_placeholder`] can retarget a write
/// onto a row its caller has already resolved by a DIFFERENT means (dispatch
/// id, not idempotency key) — see that function's docs for why that's
/// needed.
fn update_outcome_row(
    conn: &Connection,
    outcome_id: &str,
    new: &NewDispatchOutcome,
) -> Result<DispatchOutcomeRow, MemoryError> {
    let now = normalize_utc_iso_or_now("");
    let evidence_refs_json =
        serde_json::to_string(&new.evidence_refs).unwrap_or_else(|_| "[]".to_string());
    let identity_receipt_json = new
        .identity_receipt
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let retry_count = new.retry_count as i64;
    let cost_tokens = new.cost_tokens.map(|v| v as i64);
    // Identity block (#1065): once a row carries a frozen identity receipt,
    // the flat attribution columns derived from it (model/vendor/role/seat)
    // are frozen WITH it — a replay must not leave the row's columns
    // disagreeing with the receipt persisted beside them. A row without a
    // receipt (legacy or placeholder written before one existed) still
    // accepts better attribution, which is what placeholder reconciliation
    // needs.
    conn.execute(
        "UPDATE dispatch_outcomes SET
            eval_memory_id = ?2,
            model  = CASE WHEN identity_receipt IS NOT NULL THEN model  ELSE ?3 END,
            vendor = CASE WHEN identity_receipt IS NOT NULL THEN vendor ELSE ?4 END,
            role   = CASE WHEN identity_receipt IS NOT NULL THEN role   ELSE ?5 END,
            seat   = CASE WHEN identity_receipt IS NOT NULL THEN seat   ELSE ?6 END,
            identity_attribution_basis =
                CASE WHEN identity_receipt IS NOT NULL
                     THEN identity_attribution_basis ELSE ?22 END,
            task_type = ?7, execution_outcome = ?8, reported_outcome = ?9,
            retry_count = ?10, error_class = ?11, issue_ref = ?12, pr_ref = ?13,
            flow_id = ?14, cost_tokens = ?15, cost_usd = ?16, verification_present = ?17,
             diff_present = ?18, evidence_refs = ?19,
             identity_receipt = COALESCE(identity_receipt, ?20), updated_at = ?21
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
            new.reported_outcome,
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
            identity_receipt_json,
            now,
            normalize_basis(&new.identity_attribution_basis),
        ],
    )?;
    get_outcome(conn, outcome_id)?.ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "dispatch_outcomes row {outcome_id} vanished immediately after update"
        ))
    })
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

    let existing_id: Option<String> = conn
        .query_row(
            "SELECT outcome_id FROM dispatch_outcomes WHERE idempotency_key = ?1",
            params![idempotency_key],
            |r| r.get(0),
        )
        .optional()?;

    match existing_id {
        Some(outcome_id) => update_outcome_row(conn, &outcome_id, new),
        None => {
            let now = normalize_utc_iso_or_now("");
            let evidence_refs_json =
                serde_json::to_string(&new.evidence_refs).unwrap_or_else(|_| "[]".to_string());
            let identity_receipt_json = new
                .identity_receipt
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let retry_count = new.retry_count as i64;
            let cost_tokens = new.cost_tokens.map(|v| v as i64);
            conn.execute(
                "INSERT INTO dispatch_outcomes
                 (outcome_id, dispatch_id, eval_memory_id, model, vendor, role, seat,
                  task_type, execution_outcome, reported_outcome, retry_count, error_class,
                  issue_ref, pr_ref, flow_id, cost_tokens, cost_usd, verification_present,
                   diff_present, evidence_refs, identity_receipt, identity_attribution_basis,
                   idempotency_key, created_at, updated_at)
                  VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                          ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?24)",
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
                    new.reported_outcome,
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
                    identity_receipt_json,
                    normalize_basis(&new.identity_attribution_basis),
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

/// True if any `dispatch_outcomes` row already exists for `dispatch_id`
/// (regardless of `task_type`/`idempotency_key`).
///
/// Terminal-failure writers (backend/preflight/watchdog/cancel) use this for
/// FIRST-WRITER-WINS: the code path that first classified the failure keeps
/// its `error_class`, and a later, coarser catch-all (e.g. the generic
/// early-exit closer) does not clobber it with a less specific class. The
/// `tachi_complete` seam does not use this — it deliberately UPSERTS its rich
/// row keyed on `(dispatch_id, task_type)`.
pub fn outcome_exists_for_dispatch(
    conn: &Connection,
    dispatch_id: &str,
) -> Result<bool, MemoryError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM dispatch_outcomes WHERE dispatch_id = ?1 LIMIT 1",
            params![dispatch_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
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

/// Fetch the outcome row for a `dispatch_id`, regardless of `task_type`/
/// `idempotency_key`. Companion to [`outcome_exists_for_dispatch`] (bool
/// only) for callers that need the row itself to reconcile against — see
/// [`upsert_outcome_reconciling_terminal_placeholder`]. `LIMIT 1` with no
/// explicit ordering is intentional: in the one case this function's caller
/// cares about (a lone terminal-placeholder row with `reported_outcome IS
/// NULL`), first-writer-wins on the terminal side already guarantees at
/// most one such row ever exists per `dispatch_id`; when multiple genuine
/// (`reported_outcome` present) rows exist for one `dispatch_id` — the
/// legitimate distinct-`task_type` case `different_task_type_is_a_distinct_row`
/// covers — which one is returned doesn't matter to that caller, since it
/// only acts when the returned row has `reported_outcome IS NULL`.
pub fn find_outcome_by_dispatch_id(
    conn: &Connection,
    dispatch_id: &str,
) -> Result<Option<DispatchOutcomeRow>, MemoryError> {
    let sql =
        format!("SELECT {SELECT_COLUMNS} FROM dispatch_outcomes WHERE dispatch_id = ?1 LIMIT 1");
    Ok(conn
        .query_row(&sql, params![dispatch_id], row_to_outcome)
        .optional()?)
}

/// Insert-or-update the canonical outcome row for `new.dispatch_id`,
/// reconciling with any existing TERMINAL-PLACEHOLDER row for the same
/// `dispatch_id` first (#774 idempotency-key parity).
///
/// `record_terminal_failure_outcome` (`tachi-server::complete_ops`) never
/// carries a `task_type` — it always derives the `_untyped` sentinel
/// `idempotency_key` via [`derive_idempotency_key`] — so if a real
/// `tachi_complete` for the SAME dispatch later carries a real `task_type`,
/// a plain [`upsert_outcome`] (keyed on `(dispatch_id, task_type)`) computes
/// a DIFFERENT key than the placeholder's and inserts a SIBLING row instead
/// of completing it: the two writers disagree about which row is "the"
/// canonical row for this dispatch even though both describe it. A
/// placeholder is identified by `reported_outcome IS NULL` (no writer other
/// than the terminal-failure path ever leaves that null) with a `task_type`
/// that differs from this write's; when found, the write targets that row's
/// `outcome_id` directly via [`update_outcome_row`], superseding the
/// placeholder rather than deriving a fresh key. This makes
/// terminal-then-complete land the same one row that complete-then-terminal
/// already did (the terminal path's own `outcome_exists_for_dispatch`
/// first-writer-wins check already covered that order).
pub fn upsert_outcome_reconciling_terminal_placeholder(
    conn: &Connection,
    new: &NewDispatchOutcome,
) -> Result<DispatchOutcomeRow, MemoryError> {
    if let Some(existing) = find_outcome_by_dispatch_id(conn, &new.dispatch_id)? {
        if existing.reported_outcome.is_none() && existing.task_type != new.task_type {
            return update_outcome_row(conn, &existing.outcome_id, new);
        }
    }
    upsert_outcome(conn, new)
}

/// Read surface 1: outcomes for a vendor within a `created_at` window
/// (`[since, until)`, both RFC3339). `until = None` means unbounded upper
/// end. Newest first — the router's real query shape (vendor, ts).
///
/// `evidence` is mandatory (#1065 D): the caller states what attribution
/// evidence suffices for its purpose. A planned-but-unconfirmed vendor and a
/// carrier-observed vendor share this column; conflating them silently is the
/// exact failure this parameter exists to prevent.
pub fn list_outcomes_by_vendor_window(
    conn: &Connection,
    vendor: &str,
    since: &str,
    until: Option<&str>,
    evidence: OutcomeEvidenceClass,
) -> Result<Vec<DispatchOutcomeRow>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM dispatch_outcomes \
         WHERE vendor = ?1 AND created_at >= ?2 AND (?3 IS NULL OR created_at < ?3) \
         AND {} \
         ORDER BY created_at DESC",
        evidence.sql_predicate()
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
            execution_outcome: "completed".to_string(),
            reported_outcome: Some("success".to_string()),
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
            identity_receipt: Some(
                serde_json::json!({"contract_id": "dispatch_identity_receipt/v1"}),
            ),
            identity_attribution_basis: "planned_unconfirmed".to_string(),
        }
    }

    #[test]
    fn insert_and_get_roundtrip() {
        let conn = open_conn();
        let inserted = upsert_outcome(&conn, &new_outcome("o-1", "d-1")).unwrap();
        assert_eq!(inserted.outcome_id, "o-1");
        assert_eq!(inserted.dispatch_id, "d-1");
        assert_eq!(inserted.vendor, "claude");
        assert_eq!(inserted.execution_outcome, "completed");
        assert_eq!(inserted.reported_outcome.as_deref(), Some("success"));
        assert_eq!(inserted.retry_count, 0);
        assert_eq!(inserted.cost_tokens, Some(1234));
        assert!(inserted.verification_present);
        assert!(inserted.diff_present);
        assert_eq!(
            inserted.evidence_refs,
            serde_json::json!(["cargo test", "clippy"])
        );
        assert_eq!(
            inserted.identity_receipt,
            Some(serde_json::json!({"contract_id": "dispatch_identity_receipt/v1"}))
        );
        assert!(!inserted.created_at.is_empty());
        assert_eq!(inserted.created_at, inserted.updated_at);

        let got = get_outcome(&conn, "o-1").unwrap().expect("row present");
        assert_eq!(got, inserted);
    }

    /// #1065 D BUG-2: a caller that hands in an empty/garbage
    /// `identity_attribution_basis` (e.g. a stale `Default::default()` from
    /// before the closed vocabulary existed, or a plain typo) must not have
    /// that value land verbatim — the write boundary's `normalize_basis`
    /// gate rewrites anything outside the five-token vocabulary to
    /// `"unknown"` on BOTH the insert and update paths.
    #[test]
    fn garbage_basis_is_normalized_to_unknown_on_insert_and_update() {
        let conn = open_conn();

        let mut empty_basis = new_outcome("o-1", "d-1");
        empty_basis.identity_attribution_basis = String::new();
        let inserted = upsert_outcome(&conn, &empty_basis).unwrap();
        assert_eq!(
            inserted.identity_attribution_basis, "unknown",
            "an empty-string basis must be normalized on insert"
        );

        let mut garbage_basis = new_outcome("o-2", "d-2");
        garbage_basis.identity_attribution_basis = "not-a-real-basis".to_string();
        let inserted_garbage = upsert_outcome(&conn, &garbage_basis).unwrap();
        assert_eq!(
            inserted_garbage.identity_attribution_basis, "unknown",
            "an unrecognized basis token must be normalized on insert"
        );

        // A row with NO frozen receipt keeps its attribution columns mutable
        // (see `update_outcome_row`'s CASE WHEN), so re-completing the SAME
        // dispatch_id+task_type (same idempotency key → UPDATE, not INSERT)
        // with a garbage token must ALSO normalize — exercising the other
        // write path, not just the INSERT one above.
        let mut placeholder = new_outcome("o-3", "d-3");
        placeholder.identity_receipt = None;
        placeholder.identity_attribution_basis = "planned_unconfirmed".to_string();
        upsert_outcome(&conn, &placeholder).unwrap();

        let mut replay_garbage = new_outcome("o-4", "d-3");
        replay_garbage.identity_receipt = None;
        replay_garbage.identity_attribution_basis = "still-garbage".to_string();
        let updated = upsert_outcome(&conn, &replay_garbage).unwrap();
        assert_eq!(
            updated.identity_attribution_basis, "unknown",
            "an unrecognized basis token must be normalized on update too"
        );
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
        assert_eq!(
            second.identity_receipt, first.identity_receipt,
            "a replay cannot replace the receipt frozen by the first completion"
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dispatch_outcomes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "no duplicate row created");
    }

    #[test]
    fn frozen_receipt_freezes_flat_identity_columns_on_replay() {
        let conn = open_conn();
        let first = upsert_outcome(&conn, &new_outcome("o-1", "d-1")).unwrap();

        // A replay carrying DIFFERENT identity values (e.g. a rewritten
        // status.json) must not leave the flat attribution columns
        // disagreeing with the receipt frozen beside them.
        let mut replay = new_outcome("o-2", "d-1");
        replay.model = Some("rewritten-model".to_string());
        replay.vendor = "rewritten-vendor".to_string();
        replay.role = Some("rewritten-role".to_string());
        replay.seat = Some("rewritten-seat".to_string());
        replay.identity_receipt = Some(serde_json::json!({"contract_id": "rewritten"}));
        replay.identity_attribution_basis = "observed".to_string();
        let second = upsert_outcome(&conn, &replay).unwrap();

        assert_eq!(second.outcome_id, first.outcome_id);
        assert_eq!(second.model, first.model, "model frozen with the receipt");
        assert_eq!(
            second.vendor, first.vendor,
            "vendor frozen with the receipt"
        );
        assert_eq!(second.role, first.role, "role frozen with the receipt");
        assert_eq!(second.seat, first.seat, "seat frozen with the receipt");
        assert_eq!(second.identity_receipt, first.identity_receipt);
        assert_eq!(
            second.identity_attribution_basis, first.identity_attribution_basis,
            "attribution basis frozen with the receipt"
        );
    }

    #[test]
    fn row_without_receipt_still_accepts_better_attribution() {
        let conn = open_conn();
        let mut placeholder = new_outcome("o-1", "d-1");
        placeholder.identity_receipt = None;
        placeholder.model = None;
        placeholder.vendor = "unknown".to_string();
        placeholder.role = None;
        placeholder.seat = None;
        upsert_outcome(&conn, &placeholder).unwrap();

        // Placeholder reconciliation: a later completion with real identity
        // (and the receipt it came from) upgrades the receipt-less row.
        let completion = new_outcome("o-2", "d-1");
        let reconciled = upsert_outcome(&conn, &completion).unwrap();
        assert_eq!(reconciled.vendor, "claude");
        assert_eq!(reconciled.model.as_deref(), Some("claude-sonnet-5"));
        assert!(reconciled.identity_receipt.is_some());
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

        let rows = list_outcomes_by_vendor_window(
            &conn,
            "grok",
            "2026-01-01T00:00:00Z",
            None,
            OutcomeEvidenceClass::AnyAttribution,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].outcome_id, "o-b", "newest first");
        assert_eq!(rows[1].outcome_id, "o-a");

        let bounded = list_outcomes_by_vendor_window(
            &conn,
            "grok",
            "2026-01-01T00:00:00Z",
            Some("2026-06-01T00:00:00Z"),
            OutcomeEvidenceClass::AnyAttribution,
        )
        .unwrap();
        assert_eq!(bounded.len(), 1, "upper bound excludes o-b");
        assert_eq!(bounded[0].outcome_id, "o-a");
    }

    #[test]
    fn evidence_class_filters_by_attribution_basis() {
        let conn = open_conn();
        let mut planned = new_outcome("o-1", "d-1");
        planned.vendor = "grok".to_string();
        planned.identity_attribution_basis = "planned_unconfirmed".to_string();
        upsert_outcome(&conn, &planned).unwrap();

        let mut acknowledged = new_outcome("o-2", "d-2");
        acknowledged.vendor = "grok".to_string();
        acknowledged.identity_attribution_basis = "acknowledged_overlay".to_string();
        upsert_outcome(&conn, &acknowledged).unwrap();

        let mut observed = new_outcome("o-3", "d-3");
        observed.vendor = "grok".to_string();
        observed.identity_attribution_basis = "observed".to_string();
        upsert_outcome(&conn, &observed).unwrap();

        let any = list_outcomes_by_vendor_window(
            &conn,
            "grok",
            "2020-01-01T00:00:00Z",
            None,
            OutcomeEvidenceClass::AnyAttribution,
        )
        .unwrap();
        assert_eq!(any.len(), 3, "AnyAttribution includes all three bases");

        let acknowledged_or_better = list_outcomes_by_vendor_window(
            &conn,
            "grok",
            "2020-01-01T00:00:00Z",
            None,
            OutcomeEvidenceClass::CarrierAcknowledged,
        )
        .unwrap();
        assert_eq!(
            acknowledged_or_better.len(),
            2,
            "CarrierAcknowledged excludes planned_unconfirmed"
        );

        let observed_only = list_outcomes_by_vendor_window(
            &conn,
            "grok",
            "2020-01-01T00:00:00Z",
            None,
            OutcomeEvidenceClass::ObservedOnly,
        )
        .unwrap();
        assert_eq!(
            observed_only.len(),
            1,
            "ObservedOnly keeps only carrier-observed rows"
        );
        assert_eq!(observed_only[0].outcome_id, "o-3");
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

    #[test]
    fn reported_and_execution_outcome_persist_independently() {
        let conn = open_conn();
        // The truthfulness invariant (#773 ②): a false success keeps the raw
        // self-report ('success') while the machine-resolved column reads
        // 'failed'. A single shared column could not represent this at all.
        let mut o = new_outcome("o-1", "d-1");
        o.reported_outcome = Some("success".to_string());
        o.execution_outcome = "failed".to_string();
        o.error_class = Some("false_success".to_string());
        let row = upsert_outcome(&conn, &o).unwrap();
        assert_eq!(row.reported_outcome.as_deref(), Some("success"));
        assert_eq!(row.execution_outcome, "failed");
        assert_eq!(row.error_class.as_deref(), Some("false_success"));

        // A terminal path with no self-report leaves reported_outcome NULL.
        let mut term = new_outcome("o-2", "d-2");
        term.reported_outcome = None;
        term.execution_outcome = "failed".to_string();
        term.error_class = Some("watchdog".to_string());
        let term_row = upsert_outcome(&conn, &term).unwrap();
        assert_eq!(term_row.reported_outcome, None);
        assert_eq!(term_row.execution_outcome, "failed");
        assert_eq!(term_row.error_class.as_deref(), Some("watchdog"));
    }
}
