//! Durable, recipient-isolated terminal dispatch receipts.
//!
//! This is deliberately separate from memories/handoffs: delivery is a
//! dispatch lifecycle fact, and the dispatch id is its exactly-once key.

use rusqlite::{params, Connection, OptionalExtension};

use super::common::normalize_utc_iso_or_now;
use crate::error::MemoryError;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TerminalReceipt {
    pub dispatch_id: String,
    pub recipient_session_client: String,
    pub terminal_state: String,
    pub safe_summary: String,
    pub reference: Option<String>,
    pub created_at: String,
    pub acknowledged_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewTerminalReceipt {
    pub dispatch_id: String,
    pub recipient_session_client: String,
    pub terminal_state: String,
    pub safe_summary: String,
    pub reference: Option<String>,
    pub created_at: String,
}

fn row(row: &rusqlite::Row<'_>) -> Result<TerminalReceipt, rusqlite::Error> {
    Ok(TerminalReceipt {
        dispatch_id: row.get(0)?,
        recipient_session_client: row.get(1)?,
        terminal_state: row.get(2)?,
        safe_summary: row.get(3)?,
        reference: row.get(4)?,
        created_at: row.get(5)?,
        acknowledged_at: row.get(6)?,
    })
}

/// Insert only once. A duplicate is successful but never overwrites the first
/// terminal state or summary, preserving the terminal-state invariant.
pub fn insert_terminal_receipt(
    conn: &Connection,
    receipt: &NewTerminalReceipt,
) -> Result<bool, MemoryError> {
    let created_at = normalize_utc_iso_or_now(&receipt.created_at);
    Ok(conn.execute(
        "INSERT INTO terminal_dispatch_inbox (dispatch_id, recipient_session_client, terminal_state, safe_summary, reference, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(dispatch_id) DO NOTHING",
        params![receipt.dispatch_id, receipt.recipient_session_client, receipt.terminal_state,
            receipt.safe_summary, receipt.reference, created_at],
    )? == 1)
}

pub fn list_terminal_receipts(
    conn: &Connection,
    recipient: &str,
    include_acknowledged: bool,
    limit: usize,
) -> Result<Vec<TerminalReceipt>, MemoryError> {
    let sql = if include_acknowledged {
        "SELECT dispatch_id, recipient_session_client, terminal_state, safe_summary, reference, created_at, acknowledged_at FROM terminal_dispatch_inbox WHERE recipient_session_client = ?1 ORDER BY created_at DESC LIMIT ?2"
    } else {
        "SELECT dispatch_id, recipient_session_client, terminal_state, safe_summary, reference, created_at, acknowledged_at FROM terminal_dispatch_inbox WHERE recipient_session_client = ?1 AND acknowledged_at IS NULL ORDER BY created_at DESC LIMIT ?2"
    };
    let mut stmt = conn.prepare(sql)?;
    let receipts = stmt
        .query_map(params![recipient, limit as i64], row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(receipts)
}

/// Ack is recipient-scoped; a caller can never acknowledge another seat's row.
pub fn acknowledge_terminal_receipt(
    conn: &Connection,
    dispatch_id: &str,
    recipient: &str,
) -> Result<bool, MemoryError> {
    let changed = conn.execute(
        "UPDATE terminal_dispatch_inbox SET acknowledged_at = ?3 WHERE dispatch_id = ?1 AND recipient_session_client = ?2 AND acknowledged_at IS NULL",
        params![dispatch_id, recipient, normalize_utc_iso_or_now("")],
    )?;
    Ok(changed == 1)
}

pub fn get_terminal_receipt(
    conn: &Connection,
    dispatch_id: &str,
) -> Result<Option<TerminalReceipt>, MemoryError> {
    conn.query_row("SELECT dispatch_id, recipient_session_client, terminal_state, safe_summary, reference, created_at, acknowledged_at FROM terminal_dispatch_inbox WHERE dispatch_id = ?1", params![dispatch_id], row).optional().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_is_exactly_once_and_ack_is_recipient_isolated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE terminal_dispatch_inbox (dispatch_id TEXT PRIMARY KEY, recipient_session_client TEXT NOT NULL, terminal_state TEXT NOT NULL, safe_summary TEXT NOT NULL DEFAULT '', reference TEXT, created_at TEXT NOT NULL, acknowledged_at TEXT)").unwrap();
        let first = NewTerminalReceipt {
            dispatch_id: "d1".into(),
            recipient_session_client: "seat-a".into(),
            terminal_state: "TASK_STATE_FAILED".into(),
            safe_summary: "first".into(),
            reference: None,
            created_at: "".into(),
        };
        assert!(insert_terminal_receipt(&conn, &first).unwrap());
        let mut duplicate = first.clone();
        duplicate.terminal_state = "TASK_STATE_COMPLETED".into();
        assert!(!insert_terminal_receipt(&conn, &duplicate).unwrap());
        assert!(!acknowledge_terminal_receipt(&conn, "d1", "seat-b").unwrap());
        assert!(acknowledge_terminal_receipt(&conn, "d1", "seat-a").unwrap());
        assert!(!acknowledge_terminal_receipt(&conn, "d1", "seat-a").unwrap());
        assert!(list_terminal_receipts(&conn, "seat-a", false, 10)
            .unwrap()
            .is_empty());
        let acknowledged = list_terminal_receipts(&conn, "seat-a", true, 10).unwrap();
        assert_eq!(acknowledged.len(), 1);
        assert!(acknowledged[0].acknowledged_at.is_some());
        assert_eq!(
            get_terminal_receipt(&conn, "d1")
                .unwrap()
                .unwrap()
                .terminal_state,
            "TASK_STATE_FAILED"
        );
    }

    /// Focused discrimination for the three filtering/idempotency invariants
    /// the composite test above exercises alongside other behavior. This test
    /// isolates each assertion so a regression in any one is reported with a
    /// precise failure message rather than being masked by a broader check.
    #[test]
    fn second_ack_returns_false_and_include_acknowledged_filter_is_discriminated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE terminal_dispatch_inbox (dispatch_id TEXT PRIMARY KEY, recipient_session_client TEXT NOT NULL, terminal_state TEXT NOT NULL, safe_summary TEXT NOT NULL DEFAULT '', reference TEXT, created_at TEXT NOT NULL, acknowledged_at TEXT)").unwrap();

        let receipt = NewTerminalReceipt {
            dispatch_id: "d-filter".into(),
            recipient_session_client: "seat-filter".into(),
            terminal_state: "TASK_STATE_COMPLETED".into(),
            safe_summary: "filter probe".into(),
            reference: None,
            created_at: "".into(),
        };
        assert!(insert_terminal_receipt(&conn, &receipt).unwrap());

        // First ack returns true.
        let first_ack = acknowledge_terminal_receipt(&conn, "d-filter", "seat-filter").unwrap();
        assert!(first_ack, "first acknowledgement must return true");

        // SECOND ack returns false — idempotent, not a re-stamp.
        let second_ack = acknowledge_terminal_receipt(&conn, "d-filter", "seat-filter").unwrap();
        assert!(
            !second_ack,
            "second acknowledgement must return false, got {second_ack}"
        );

        // include_acknowledged=false EXCLUDES the acknowledged row.
        let unacked = list_terminal_receipts(&conn, "seat-filter", false, 10).unwrap();
        assert!(
            unacked.is_empty(),
            "include_acknowledged=false must exclude the acknowledged row, got {unacked:?}"
        );

        // include_acknowledged=true INCLUDES the acknowledged row.
        let with_acked = list_terminal_receipts(&conn, "seat-filter", true, 10).unwrap();
        assert_eq!(
            with_acked.len(),
            1,
            "include_acknowledged=true must include the acknowledged row"
        );
        assert!(
            with_acked[0].acknowledged_at.is_some(),
            "included row must carry a stamped acknowledged_at"
        );
        assert_eq!(with_acked[0].dispatch_id, "d-filter");
    }
}
