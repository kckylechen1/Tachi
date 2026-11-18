//! v43 (#2012 Slice 1): adjudication provenance on both adjudication ledgers.
//!
//! Each `dispatch_adjudications` / `mirror_eval_adjudications` row carries a
//! server-held receipt (how the adjudicator identity was established) and a
//! frozen evidence basis graded on the five-token vocabulary of
//! `tachi_dispatch::IdentityAttributionBasis`. Both ledgers are append-only:
//! a no-replace / no-update / no-delete trigger triple refuses every in-place
//! mutation, including REPLACE under `recursive_triggers=OFF`.
//!
//! Channel → basis (Frozen Spec v3.1):
//! - `verified_admission` → `acknowledged_overlay` (actor identity only);
//! - `self_asserted_local` → `planned_unconfirmed`;
//! - `unverified_caller`, a structurally illegal receipt, a receipt whose
//!   subject does not match the row, or no receipt → `unknown`;
//! - pre-v43 rows (actor strings without receipts) → `fallback_unreceipted`.
//!
//! `observed` stays reserved for execution-witnessed identity; nothing
//! produces it. `operator_cli` does not exist: the column CHECK rejects it.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::StoreProfile;
use crate::error::MemoryError;

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "admin"))]
mod runtime_tests;

pub(crate) const V43_SENTINEL: &str = "v43_adjudication_provenance";

/// Admission fact the server used to establish the adjudicator identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjudicationChannel {
    VerifiedAdmission,
    SelfAssertedLocal,
    UnverifiedCaller,
}

impl AdjudicationChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedAdmission => "verified_admission",
            Self::SelfAssertedLocal => "self_asserted_local",
            Self::UnverifiedCaller => "unverified_caller",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "verified_admission" => Some(Self::VerifiedAdmission),
            "self_asserted_local" => Some(Self::SelfAssertedLocal),
            "unverified_caller" => Some(Self::UnverifiedCaller),
            _ => None,
        }
    }

    /// Basis token this channel can at most justify.
    pub fn attribution_basis(self) -> &'static str {
        match self {
            Self::VerifiedAdmission => "acknowledged_overlay",
            Self::SelfAssertedLocal => "planned_unconfirmed",
            Self::UnverifiedCaller => "unknown",
        }
    }
}

/// `model` is the claimed reviewer model; it is replay identity, never evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjudicationReceiptSubject {
    pub actor: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// Server-held receipt: `{subject: {actor, model}, channel, binding, captured_at}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjudicationReceipt {
    pub subject: AdjudicationReceiptSubject,
    pub channel: AdjudicationChannel,
    /// Admission coordinates the channel was read from. Excluded from replay identity.
    #[serde(default)]
    pub binding: Option<serde_json::Value>,
    /// Excluded from replay identity.
    pub captured_at: String,
}

/// Replay identity of a receipt: `binding` and `captured_at` are excluded so a
/// transport retry carrying a fresh capture stays idempotent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptStableSubset {
    pub actor: String,
    pub model: Option<String>,
    pub channel: AdjudicationChannel,
}

impl AdjudicationReceipt {
    pub fn stable_subset(&self) -> ReceiptStableSubset {
        ReceiptStableSubset {
            actor: self.subject.actor.clone(),
            model: self.subject.model.clone(),
            channel: self.channel,
        }
    }

    pub(crate) fn to_json(&self) -> Result<String, MemoryError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Strict structural parse; any shape deviation is `None`, never a guess.
    pub fn parse_stored(raw: &str) -> Option<Self> {
        let receipt = serde_json::from_str::<Self>(raw).ok()?;
        receipt.is_structurally_legal().then_some(receipt)
    }

    fn is_structurally_legal(&self) -> bool {
        !self.subject.actor.trim().is_empty()
            && self
                .subject
                .model
                .as_deref()
                .is_none_or(|model| !model.trim().is_empty())
            && !self.captured_at.trim().is_empty()
    }
}

/// Stable subset of a stored receipt column. `Err(())` marks a stored value
/// that fails structural validation; it never equals any replay.
pub(crate) fn stored_stable_subset(raw: Option<&str>) -> Result<Option<ReceiptStableSubset>, ()> {
    match raw {
        None => Ok(None),
        Some(raw) => AdjudicationReceipt::parse_stored(raw)
            .map(|receipt| Some(receipt.stable_subset()))
            .ok_or(()),
    }
}

/// Same-key replays are identical only when the stable receipt subsets match.
pub(crate) fn receipt_replay_matches(
    stored: Option<&str>,
    replay: Option<&AdjudicationReceipt>,
) -> bool {
    stored_stable_subset(stored)
        .is_ok_and(|subset| subset == replay.map(AdjudicationReceipt::stable_subset))
}

/// Five-token vocabulary, byte-identical to `IdentityAttributionBasis::as_str`.
pub const ADJUDICATION_ATTRIBUTION_BASES: [&str; 5] = [
    "planned_unconfirmed",
    "acknowledged_overlay",
    "observed",
    "fallback_unreceipted",
    "unknown",
];

/// INSERT normalize gate: a non-table token becomes `unknown`.
pub fn normalize_adjudication_basis(raw: &str) -> &'static str {
    ADJUDICATION_ATTRIBUTION_BASES
        .iter()
        .copied()
        .find(|token| *token == raw)
        .unwrap_or("unknown")
}

/// Which row columns the receipt subject must match.
#[derive(Debug, Clone, Copy)]
pub enum ReceiptRowSubject<'a> {
    /// `dispatch_adjudications`: no model column; only the actor is bound.
    Actor(&'a str),
    /// `mirror_eval_adjudications`: actor and verifier_model are both bound.
    ActorAndModel(&'a str, Option<&'a str>),
}

/// The one classifier shared by the INSERT gate and the v43 backfill. A
/// receipt that is absent, unparseable, structurally illegal, or attests a
/// different subject than the row it is stored on is `unknown`.
pub fn adjudication_basis_for_receipt(
    raw: Option<&str>,
    row: ReceiptRowSubject<'_>,
) -> &'static str {
    let Some(receipt) = raw.and_then(AdjudicationReceipt::parse_stored) else {
        return "unknown";
    };
    let subject_matches = match row {
        ReceiptRowSubject::Actor(actor) => receipt.subject.actor == actor,
        ReceiptRowSubject::ActorAndModel(actor, model) => {
            receipt.subject.actor == actor && receipt.subject.model.as_deref() == model
        }
    };
    if !subject_matches {
        return "unknown";
    }
    normalize_adjudication_basis(receipt.channel.attribution_basis())
}

/// Serialized receipt plus its normalized basis, ready to bind on INSERT.
pub(crate) fn receipt_columns(
    receipt: Option<&AdjudicationReceipt>,
    row: ReceiptRowSubject<'_>,
) -> Result<(Option<String>, &'static str), MemoryError> {
    let json = receipt.map(AdjudicationReceipt::to_json).transpose()?;
    let basis = adjudication_basis_for_receipt(json.as_deref(), row);
    Ok((json, basis))
}

// `IS` (not `=`) so an absent channel evaluates false, never a passing NULL.
const RECEIPT_COLUMN_SQL: &str = "adjudication_receipt TEXT CHECK (\
adjudication_receipt IS NULL OR CASE WHEN json_valid(adjudication_receipt) THEN \
json_type(adjudication_receipt) IS 'object' \
AND json_type(adjudication_receipt, '$.channel') IS 'text' \
AND json_extract(adjudication_receipt, '$.channel') IN \
('verified_admission', 'self_asserted_local', 'unverified_caller') \
ELSE 0 END)";

const BASIS_COLUMN_SQL: &str = "adjudication_attribution_basis TEXT NOT NULL DEFAULT 'unknown' \
CHECK (adjudication_attribution_basis IN \
('planned_unconfirmed', 'acknowledged_overlay', 'observed', 'fallback_unreceipted', 'unknown'))";

struct Ledger {
    table: &'static str,
    model_column: Option<&'static str>,
    unique_targets: &'static [(&'static str, &'static [&'static str])],
}

const LEDGERS: [Ledger; 2] = [
    Ledger {
        table: "dispatch_adjudications",
        model_column: None,
        unique_targets: &[
            ("pk", &["adjudication_id"]),
            ("u", &["event_key"]),
            ("u", &["outcome_id", "insertion_seq"]),
        ],
    },
    Ledger {
        table: "mirror_eval_adjudications",
        model_column: Some("verifier_model"),
        unique_targets: &[
            ("pk", &["adjudication_id"]),
            ("u", &["event_key"]),
            ("u", &["eval_run_id", "insertion_seq"]),
        ],
    },
];

const DISPATCH_NO_REPLACE_SQL: &str = r#"
CREATE TRIGGER dispatch_adjudications_no_replace
BEFORE INSERT ON dispatch_adjudications
WHEN EXISTS (
    SELECT 1 FROM dispatch_adjudications
    WHERE rowid = NEW.rowid
       OR adjudication_id = NEW.adjudication_id
       OR event_key = NEW.event_key
       OR (outcome_id = NEW.outcome_id AND insertion_seq = NEW.insertion_seq)
)
BEGIN
    SELECT RAISE(ABORT, 'dispatch adjudications are append-only: unique conflict target already recorded');
END
"#;

const DISPATCH_NO_UPDATE_SQL: &str = r#"
CREATE TRIGGER dispatch_adjudications_no_update
BEFORE UPDATE ON dispatch_adjudications
BEGIN
    SELECT RAISE(ABORT, 'dispatch adjudications are append-only');
END
"#;

const DISPATCH_NO_DELETE_SQL: &str = r#"
CREATE TRIGGER dispatch_adjudications_no_delete
BEFORE DELETE ON dispatch_adjudications
BEGIN
    SELECT RAISE(ABORT, 'dispatch adjudications are append-only');
END
"#;

const MIRROR_NO_REPLACE_SQL: &str = r#"
CREATE TRIGGER mirror_eval_adjudications_no_replace
BEFORE INSERT ON mirror_eval_adjudications
WHEN EXISTS (
    SELECT 1 FROM mirror_eval_adjudications
    WHERE rowid = NEW.rowid
       OR adjudication_id = NEW.adjudication_id
       OR event_key = NEW.event_key
       OR (eval_run_id = NEW.eval_run_id AND insertion_seq = NEW.insertion_seq)
)
BEGIN
    SELECT RAISE(ABORT, 'mirror eval adjudications are append-only: unique conflict target already recorded');
END
"#;

const MIRROR_NO_UPDATE_SQL: &str = r#"
CREATE TRIGGER mirror_eval_adjudications_no_update
BEFORE UPDATE ON mirror_eval_adjudications
BEGIN
    SELECT RAISE(ABORT, 'mirror eval adjudications are append-only');
END
"#;

const MIRROR_NO_DELETE_SQL: &str = r#"
CREATE TRIGGER mirror_eval_adjudications_no_delete
BEFORE DELETE ON mirror_eval_adjudications
BEGIN
    SELECT RAISE(ABORT, 'mirror eval adjudications are append-only');
END
"#;

/// Canonical `(name, table, sql)` inventory of the v43 trigger triple pair.
pub(crate) const TRIGGERS: [(&str, &str, &str); 6] = [
    (
        "dispatch_adjudications_no_replace",
        "dispatch_adjudications",
        DISPATCH_NO_REPLACE_SQL,
    ),
    (
        "dispatch_adjudications_no_update",
        "dispatch_adjudications",
        DISPATCH_NO_UPDATE_SQL,
    ),
    (
        "dispatch_adjudications_no_delete",
        "dispatch_adjudications",
        DISPATCH_NO_DELETE_SQL,
    ),
    (
        "mirror_eval_adjudications_no_replace",
        "mirror_eval_adjudications",
        MIRROR_NO_REPLACE_SQL,
    ),
    (
        "mirror_eval_adjudications_no_update",
        "mirror_eval_adjudications",
        MIRROR_NO_UPDATE_SQL,
    ),
    (
        "mirror_eval_adjudications_no_delete",
        "mirror_eval_adjudications",
        MIRROR_NO_DELETE_SQL,
    ),
];

pub(crate) fn expected_trigger(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    TRIGGERS
        .iter()
        .copied()
        .find(|(canonical, _, _)| name.eq_ignore_ascii_case(canonical))
}

/// v43 body. Columns are added and pre-v43 rows back-filled only in the run
/// that adds the basis column; the triggers are installed afterwards, so a
/// replay changes zero rows and never attempts an UPDATE the triggers refuse.
pub(crate) fn migrate_v43_adjudication_provenance(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    if !profile.includes_product() {
        return Ok(0);
    }
    let mut backfilled = 0;
    for ledger in &LEDGERS {
        validate_rowid_layout(conn, ledger.table)?;
        backfilled += install_ledger_columns(conn, ledger)?;
    }
    for (name, _, sql) in TRIGGERS {
        if !object_exists(conn, "trigger", name)? {
            conn.execute_batch(sql)?;
        }
    }
    validate_adjudication_provenance_schema(conn)?;
    Ok(backfilled)
}

fn install_ledger_columns(conn: &Connection, ledger: &Ledger) -> Result<usize, MemoryError> {
    if !has_column(conn, ledger.table, "adjudication_receipt")? {
        conn.execute_batch(&format!(
            "ALTER TABLE {} ADD COLUMN {RECEIPT_COLUMN_SQL}",
            ledger.table
        ))?;
    }
    if has_column(conn, ledger.table, "adjudication_attribution_basis")? {
        return Ok(0);
    }
    conn.execute_batch(&format!(
        "ALTER TABLE {} ADD COLUMN {BASIS_COLUMN_SQL}",
        ledger.table
    ))?;
    let model = ledger.model_column.unwrap_or("NULL");
    let mut statement = conn.prepare(&format!(
        "SELECT rowid, actor, {model}, adjudication_receipt FROM {}",
        ledger.table
    ))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut backfilled = 0;
    for (rowid, actor, model, receipt) in rows {
        let basis = match receipt.as_deref() {
            None if !actor.trim().is_empty() => "fallback_unreceipted",
            None => "unknown",
            Some(raw) => {
                let row = match ledger.model_column {
                    None => ReceiptRowSubject::Actor(&actor),
                    Some(_) => ReceiptRowSubject::ActorAndModel(&actor, model.as_deref()),
                };
                adjudication_basis_for_receipt(Some(raw), row)
            }
        };
        if basis != "unknown" {
            conn.execute(
                &format!(
                    "UPDATE {} SET adjudication_attribution_basis = ?2 WHERE rowid = ?1",
                    ledger.table
                ),
                params![rowid, basis],
            )?;
            backfilled += 1;
        }
    }
    Ok(backfilled)
}

/// Whether this store claims v43 (Product schema + sentinel, or v43 column
/// present on a ledger); the trigger triple pair is then required by the
/// persistent trigger inventory.
pub(crate) fn require_triggers(
    conn: &Connection,
    found: &HashSet<&'static str>,
) -> Result<(), MemoryError> {
    let sentinel = object_exists(conn, "table", "hard_state")?
        && conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM hard_state WHERE namespace = 'migrations' AND key = ?1)",
            [V43_SENTINEL],
            |row| row.get::<_, bool>(0),
        )?;
    let product_schema = object_exists(conn, "table", "identity_admissions")?;
    for ledger in &LEDGERS {
        let table_present = object_exists(conn, "table", ledger.table)?;
        let claims_v43 = (product_schema && sentinel)
            || (table_present && has_column(conn, ledger.table, "adjudication_attribution_basis")?);
        if !claims_v43 {
            continue;
        }
        for (name, trigger_table, _) in TRIGGERS {
            if trigger_table == ledger.table && !found.contains(name) {
                return Err(MemoryError::InvalidArg(format!(
                    "unsafe persistent trigger inventory: required trigger '{name}' is missing"
                )));
            }
        }
    }
    Ok(())
}

/// Read-only shape check: canonical columns, triggers, rowid layout and the
/// exact unique conflict targets the no-replace triggers cover.
pub(crate) fn validate_adjudication_provenance_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    for ledger in &LEDGERS {
        validate_rowid_layout(conn, ledger.table)?;
        let table_sql: String = conn
            .query_row(
                "SELECT COALESCE(sql, '') FROM main.sqlite_schema WHERE type='table' AND name=?1",
                [ledger.table],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default();
        let normalized = normalize_sql(&table_sql);
        for column in [RECEIPT_COLUMN_SQL, BASIS_COLUMN_SQL] {
            if !normalized.contains(&normalize_sql(column)) {
                return Err(incomplete(format!(
                    "table '{}' is missing a canonical provenance column",
                    ledger.table
                )));
            }
        }
        if normalized.to_ascii_uppercase().contains("CONFLICT") {
            return Err(incomplete(format!(
                "table '{}' has a non-canonical conflict policy",
                ledger.table
            )));
        }
        let mut actual = unique_targets(conn, ledger.table)?;
        let mut expected: Vec<(String, Vec<String>)> = ledger
            .unique_targets
            .iter()
            .map(|(origin, columns)| {
                (
                    origin.to_string(),
                    columns.iter().map(|c| c.to_string()).collect(),
                )
            })
            .collect();
        actual.sort();
        expected.sort();
        if actual != expected {
            return Err(incomplete(format!(
                "table '{}' has unexpected unique conflict targets",
                ledger.table
            )));
        }
    }
    for (name, table, sql) in TRIGGERS {
        let found: Option<(String, String)> = conn
            .query_row(
                "SELECT tbl_name, COALESCE(sql, '') FROM main.sqlite_schema \
                 WHERE type='trigger' AND name=?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if found.as_ref().is_none_or(|(actual_table, actual_sql)| {
            actual_table != table || normalize_sql(actual_sql) != normalize_sql(sql)
        }) {
            return Err(incomplete(format!(
                "trigger '{name}' is missing or non-canonical"
            )));
        }
    }
    Ok(())
}

fn incomplete(detail: String) -> MemoryError {
    MemoryError::InvalidArg(format!(
        "incomplete v43 adjudication provenance schema: {detail}"
    ))
}

fn normalize_sql(sql: &str) -> String {
    sql.trim_end_matches(|ch: char| ch == ';' || ch.is_ascii_whitespace())
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn object_exists(conn: &Connection, kind: &str, name: &str) -> Result<bool, MemoryError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type=?1 AND name=?2)",
        params![kind, name],
        |row| row.get(0),
    )?)
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1, 'main') WHERE name = ?2)",
        params![table, column],
        |row| row.get(0),
    )?)
}

/// The triggers address SQLite's physical rowid; an alias column or a
/// WITHOUT ROWID rebuild would change that meaning.
fn validate_rowid_layout(conn: &Connection, table: &str) -> Result<(), MemoryError> {
    let canonical: bool = conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM pragma_table_list
             WHERE schema = 'main' AND name = ?1 AND type = 'table' AND wr = 0
         ) AND NOT EXISTS (
             SELECT 1 FROM pragma_table_xinfo(?1, 'main')
             WHERE name COLLATE NOCASE IN ('rowid', 'oid', '_rowid_')
         )",
        [table],
        |row| row.get(0),
    )?;
    if !canonical {
        return Err(incomplete(format!(
            "table '{table}' has a non-canonical rowid layout"
        )));
    }
    Ok(())
}

fn unique_targets(
    conn: &Connection,
    table: &str,
) -> Result<Vec<(String, Vec<String>)>, MemoryError> {
    let mut statement = conn
        .prepare("SELECT name, origin, partial FROM pragma_index_list(?1) WHERE \"unique\" = 1")?;
    let indexes = statement
        .query_map([table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut targets = Vec::with_capacity(indexes.len());
    for (name, origin, partial) in indexes {
        let mut columns = conn.prepare(
            "SELECT name, coll, desc FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno",
        )?;
        let columns = columns
            .query_map([&name], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut canonical = Vec::with_capacity(columns.len());
        for (column, collation, descending) in columns {
            match column {
                Some(column)
                    if !partial && collation.as_deref() == Some("BINARY") && !descending =>
                {
                    canonical.push(column)
                }
                _ => {
                    return Err(incomplete(format!(
                        "table '{table}' has a non-canonical unique conflict target"
                    )))
                }
            }
        }
        targets.push((origin, canonical));
    }
    Ok(targets)
}
