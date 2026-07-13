//! v17 (#1065 option D) — add and back-fill the frozen
//! `identity_attribution_basis` column on `dispatch_outcomes`.
//!
//! The basis names the evidence class of the flat identity columns
//! (model/vendor/role/seat) so readers can tell planned routing intent from
//! carrier-observed execution fact. Back-fill derives it from each row's
//! frozen receipt:
//!
//! - receipt with `acknowledgement: "unconfirmed"` → `planned_unconfirmed`
//! - `"acknowledged"` → `acknowledged_overlay`
//! - `"substituted"` / `"ignored"` → `observed`
//! - receipt present but unparseable → `unknown`
//! - no receipt, but the row carries real attribution → `fallback_unreceipted`
//!   (it was reconstructed from profile/agent at write time)
//! - no receipt and no attribution (`vendor='unknown'`, model NULL) → `unknown`

use rusqlite::Connection;

use crate::error::MemoryError;

use super::dispatch_outcomes_reported::table_exists;
use super::legacy_columns::table_has_column;

pub(super) fn migrate_v17_dispatch_outcomes_attribution_basis(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    if !table_exists(conn, "dispatch_outcomes")? {
        return Ok(0);
    }
    if !table_has_column(conn, "dispatch_outcomes", "identity_attribution_basis")? {
        conn.execute(
            "ALTER TABLE dispatch_outcomes ADD COLUMN \
             identity_attribution_basis TEXT NOT NULL DEFAULT 'unknown'",
            [],
        )?;
    }

    // Back-fill rows still on the column default. Receipt parsing happens in
    // Rust (not json_extract) so the acknowledgement vocabulary check is the
    // same closed enum the runtime uses.
    let mut stmt = conn.prepare(
        "SELECT outcome_id, identity_receipt, vendor, model FROM dispatch_outcomes \
         WHERE identity_attribution_basis = 'unknown'",
    )?;
    let rows: Vec<(String, Option<String>, String, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let mut backfilled = 0usize;
    for (outcome_id, receipt_json, vendor, model) in rows {
        let basis = match receipt_json.as_deref() {
            Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(value) => match value
                    .pointer("/observed/acknowledgement")
                    .and_then(serde_json::Value::as_str)
                {
                    Some("unconfirmed") => "planned_unconfirmed",
                    Some("acknowledged") => "acknowledged_overlay",
                    Some("substituted") | Some("ignored") => "observed",
                    _ => "unknown",
                },
                Err(_) => "unknown",
            },
            None if vendor != "unknown" || model.is_some() => "fallback_unreceipted",
            None => "unknown",
        };
        if basis != "unknown" {
            conn.execute(
                "UPDATE dispatch_outcomes SET identity_attribution_basis = ?2 \
                 WHERE outcome_id = ?1",
                rusqlite::params![outcome_id, basis],
            )?;
            backfilled += 1;
        }
    }
    Ok(backfilled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE dispatch_outcomes (
                 outcome_id TEXT PRIMARY KEY,
                 dispatch_id TEXT NOT NULL,
                 model TEXT,
                 vendor TEXT NOT NULL DEFAULT 'unknown',
                 execution_outcome TEXT NOT NULL,
                 identity_receipt TEXT,
                 idempotency_key TEXT NOT NULL
             );",
        )
        .unwrap();
    }

    fn insert(
        conn: &Connection,
        id: &str,
        vendor: &str,
        model: Option<&str>,
        receipt: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO dispatch_outcomes
                 (outcome_id, dispatch_id, model, vendor, execution_outcome,
                  identity_receipt, idempotency_key)
             VALUES (?1, ?1, ?2, ?3, 'completed', ?4, ?1)",
            rusqlite::params![id, model, vendor, receipt],
        )
        .unwrap();
    }

    fn basis(conn: &Connection, id: &str) -> String {
        conn.query_row(
            "SELECT identity_attribution_basis FROM dispatch_outcomes WHERE outcome_id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn v17_backfills_basis_from_frozen_receipts_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_table(&conn);
        insert(
            &conn,
            "o-unconfirmed",
            "glm",
            Some("m"),
            Some(r#"{"observed":{"acknowledgement":"unconfirmed"}}"#),
        );
        insert(
            &conn,
            "o-substituted",
            "glm",
            Some("m"),
            Some(r#"{"observed":{"acknowledgement":"substituted"}}"#),
        );
        insert(
            &conn,
            "o-acked",
            "glm",
            Some("m"),
            Some(r#"{"observed":{"acknowledgement":"acknowledged"}}"#),
        );
        insert(&conn, "o-corrupt", "glm", Some("m"), Some("not json"));
        insert(&conn, "o-legacy", "codex", None, None);
        insert(&conn, "o-void", "unknown", None, None);

        let first = migrate_v17_dispatch_outcomes_attribution_basis(&conn).unwrap();
        assert_eq!(first, 4, "four rows earn a non-unknown basis");
        assert_eq!(basis(&conn, "o-unconfirmed"), "planned_unconfirmed");
        assert_eq!(basis(&conn, "o-substituted"), "observed");
        assert_eq!(basis(&conn, "o-acked"), "acknowledged_overlay");
        assert_eq!(basis(&conn, "o-corrupt"), "unknown");
        assert_eq!(basis(&conn, "o-legacy"), "fallback_unreceipted");
        assert_eq!(basis(&conn, "o-void"), "unknown");

        // Re-run: column exists, only still-unknown rows are revisited, and
        // they legitimately stay unknown — no churn.
        let second = migrate_v17_dispatch_outcomes_attribution_basis(&conn).unwrap();
        assert_eq!(second, 0);
    }
}
