//! Fresh attachment re-admission, separate from the historical fact spine.

use rusqlite::{params, Connection};

use super::{load_state_row, projection_of, HarnessSessionStateProjection};
use crate::db::common::with_composable_write;
use crate::db::harness_session_attachments::{
    attachment_not_found, find_attachment_by_id, find_attachment_in_scope,
    verify_attachment_rebind, HarnessSessionAttachment, HarnessSessionAttachmentSelector,
    HarnessSessionAttachmentState, HarnessSessionClaimScope, HarnessSessionHostAdmission,
};
use crate::db::normalize_utc_iso_or_now;
use crate::error::MemoryError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionReconnectReceipt {
    pub attachment: HarnessSessionAttachment,
    pub previous_attachment_state: HarnessSessionAttachmentState,
    pub reconnected: bool,
    /// The host resumes its authoritative event replay from this revision;
    /// canonical state is untouched by a reconnect.
    pub resume_from_revision: i64,
    pub state: HarnessSessionStateProjection,
}

/// Re-admit an Unbound attachment. Canonical project attachments are absent
/// from this compatibility surface, including on an exact reconnect replay.
pub fn reconnect_harness_session(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
    ttl_seconds: i64,
) -> Result<HarnessSessionReconnectReceipt, MemoryError> {
    reconnect_harness_session_in_scope(
        conn,
        selector,
        host,
        admission_receipt_ref,
        ttl_seconds,
        HarnessSessionClaimScope::Unbound,
    )
}

/// Reconnect a project worker only under the host's current local admission
/// and its own active project claim. This does not authorize lifecycle facts.
pub fn reconnect_project_harness_session(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
    project: &str,
    ttl_seconds: i64,
) -> Result<HarnessSessionReconnectReceipt, MemoryError> {
    reconnect_harness_session_in_scope(
        conn,
        selector,
        host,
        admission_receipt_ref,
        ttl_seconds,
        HarnessSessionClaimScope::Project(project),
    )
}

fn reconnect_harness_session_in_scope(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
    ttl_seconds: i64,
    scope: HarnessSessionClaimScope<'_>,
) -> Result<HarnessSessionReconnectReceipt, MemoryError> {
    if ttl_seconds < 0 {
        return Err(MemoryError::InvalidArg(
            "WorkClaim freshness TTL must be non-negative".to_string(),
        ));
    }
    with_composable_write(conn, |conn| {
        scope.authorize_host(conn, host, admission_receipt_ref)?;
        let attachment = find_attachment_in_scope(conn, selector, &host.host_identity, scope)?
            .ok_or_else(attachment_not_found)?;
        let attachment_id = attachment.attachment_id.clone();
        // The exact worker revision and policy remain required on replay.
        // Only this transaction may replace the old admission receipt.
        verify_attachment_rebind(
            conn,
            &attachment,
            host,
            admission_receipt_ref,
            ttl_seconds,
            scope,
        )?;
        let previous = attachment.state;
        let binding_changed = attachment.admission_receipt_ref != admission_receipt_ref;
        let reconnected = previous != HarnessSessionAttachmentState::Attached || binding_changed;
        if reconnected {
            conn.execute(
                "UPDATE harness_session_attachments
                 SET state = 'attached', admission_receipt_ref = ?1, updated_at = ?2
                 WHERE attachment_id = ?3",
                params![
                    admission_receipt_ref,
                    normalize_utc_iso_or_now(""),
                    attachment_id
                ],
            )?;
        }
        let updated_attachment = find_attachment_by_id(conn, &attachment_id)?
            .expect("attachment row just read in the same transaction");
        let state_row = load_state_row(conn, &attachment_id)?;
        let resume_from_revision = state_row
            .as_ref()
            .map(|row| row.canonical_revision)
            .unwrap_or(0);
        Ok(HarnessSessionReconnectReceipt {
            attachment: updated_attachment,
            previous_attachment_state: previous,
            reconnected,
            resume_from_revision,
            state: projection_of(&attachment_id, state_row),
        })
    })
}
