//! Bounded journal tail and canonical projection from one authorized read snapshot.

use super::{
    load_state_row, projection_of, require_attachment, row_to_event, HarnessSessionEvent,
    HarnessSessionStateProjection, EVENT_COLUMNS,
};
use crate::error::MemoryError;
use crate::{HarnessSessionAttachmentSelector, HarnessSessionHostAdmission};
use rusqlite::{params, Connection};

// Provisional per-action bounds: summaries remain intact; excess receipts are omitted.
const DEFAULT_SESSION_EVENT_LIMIT: usize = 20;
const MAX_SESSION_EVENT_LIMIT: usize = 100;

/// The latest receipt tail, not a transcript or a complete event-history export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionStateWithEvents {
    pub state: HarnessSessionStateProjection,
    /// Latest receipts in ingestion order, independent of source revision.
    pub events: Vec<HarnessSessionEvent>,
    pub event_limit: usize,
    /// Older journal rows were omitted at this read snapshot. This does not
    /// describe worker completion or the completeness of host reporting.
    pub events_truncated: bool,
}

/// Read the canonical state and bounded recent receipts under the same host
/// admission and SQLite snapshot. WorkClaim release does not erase evidence.
/// Zero suppresses event content but still authorizes the read and reports
/// whether any receipts were omitted.
pub fn get_harness_session_state_with_events(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
    limit: Option<usize>,
) -> Result<HarnessSessionStateWithEvents, MemoryError> {
    let event_limit = limit
        .unwrap_or(DEFAULT_SESSION_EVENT_LIMIT)
        .min(MAX_SESSION_EVENT_LIMIT);
    let tx = conn.unchecked_transaction()?;
    let attachment = require_attachment(&tx, selector, host, admission_receipt_ref)?;
    let state = projection_of(
        &attachment.attachment_id,
        load_state_row(&tx, &attachment.attachment_id)?,
    );
    let mut events = {
        let mut statement = tx.prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM harness_session_events
             WHERE attachment_id = ?1 ORDER BY event_row_id DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            params![attachment.attachment_id, event_limit + 1],
            row_to_event,
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let events_truncated = events.len() > event_limit;
    events.truncate(event_limit);
    events.reverse();
    tx.commit()?;
    Ok(HarnessSessionStateWithEvents {
        state,
        events,
        event_limit,
        events_truncated,
    })
}
