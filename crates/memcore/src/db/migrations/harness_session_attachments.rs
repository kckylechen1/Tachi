//! v33: durable, host-owned ACP session attachment admission (#1733).
//! Renumbered from v31 in the 2026-08-16 merge resolution (v31/v32 went to
//! the A2A mailbox pair on main); the migration itself is unchanged.
//!
//! The table is deliberately additive and profile-neutral.  A portable store
//! may carry the receipt ledger even though the typed attachment writers live
//! on the full server surface; this keeps the schema/version contract honest
//! for hosts that share a store across the two builds.

use rusqlite::Connection;

use crate::error::MemoryError;

pub(super) fn migrate_v33_harness_session_attachments(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    crate::db::schema::install_harness_session_attachments_schema(conn)?;
    crate::db::schema::validate_harness_session_attachments_schema(conn)?;
    // One table, two lookup indexes.  The return value is a migration receipt
    // count, not a row count; it remains stable on replay because CREATE IF
    // NOT EXISTS is idempotent and the sentinel gate runs before this function.
    Ok(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v33_creates_the_attachment_receipt_ledger_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(migrate_v33_harness_session_attachments(&conn).unwrap(), 3);
        assert_eq!(migrate_v33_harness_session_attachments(&conn).unwrap(), 3);
        crate::db::schema::validate_harness_session_attachments_schema(&conn).unwrap();
    }
}
