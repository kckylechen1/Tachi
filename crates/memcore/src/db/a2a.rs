//! Product-only local A2A mailbox persistence (#1751).
//!
//! This module owns database identity, idempotency and transition atomicity.
//! It does not infer a live peer, scrub content, or grant authority: those are
//! server admission/rendering responsibilities. A historical eligible
//! admission is deliberately sufficient for offline advisory delivery.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::error::MemoryError;

use super::common::normalize_utc_iso;

pub const A2A_TURN_RESPONSE_KIND: &str = "turn_response/v1";
pub const A2A_SAME_HOST_TRUST_DOMAIN: &str = "same_host";
pub const MAX_A2A_STORAGE_BATCH: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct A2aRecipientEligibility {
    pub agent_identity_id: String,
    pub admission_id: String,
    pub identity_assurance: String,
    pub trust_domain: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewA2aEnvelope {
    pub envelope_id: String,
    pub issuer_agent_identity_id: String,
    pub recipient_agent_identity_id: String,
    pub subject_ref: String,
    pub body: String,
    pub body_digest: String,
    pub issuer_identity_assurance: String,
    pub recipient_identity_assurance: String,
    pub idempotency_key: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct A2aEnvelope {
    pub envelope_id: String,
    pub kind: String,
    pub issuer_agent_identity_id: String,
    pub recipient_agent_identity_id: String,
    pub subject_ref: String,
    pub body: String,
    pub body_digest: String,
    pub issuer_identity_assurance: String,
    pub recipient_identity_assurance: String,
    pub issuer_trust_domain: String,
    pub recipient_trust_domain: String,
    pub idempotency_key: String,
    pub created_at: String,
    pub expires_at: String,
    pub current_state: String,
    pub state_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct A2aDeliveryReceipt {
    pub receipt_id: String,
    pub envelope_id: String,
    pub envelope_version: i64,
    pub state: String,
    pub actor_agent_identity_id: String,
    pub identity_assurance: String,
    pub trust_domain: String,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum A2aInsertOutcome {
    Created {
        envelope: A2aEnvelope,
        receipt: A2aDeliveryReceipt,
    },
    Replay {
        envelope: A2aEnvelope,
        receipt: A2aDeliveryReceipt,
    },
}

/// Header-only status row. Body bytes are intentionally absent: status is an
/// issuer/recipient-scoped audit read, never an alternate delivery path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct A2aStatusRow {
    pub envelope_id: String,
    pub kind: String,
    pub issuer_agent_identity_id: String,
    pub recipient_agent_identity_id: String,
    pub subject_ref: String,
    pub body_digest: String,
    pub created_at: String,
    pub expires_at: String,
    pub current_state: String,
    pub state_version: i64,
    pub receipts: Vec<A2aDeliveryReceipt>,
}

fn refuse_blank(field: &str, value: &str) -> Result<(), MemoryError> {
    if value.trim().is_empty() {
        return Err(MemoryError::InvalidArg(format!(
            "a2a {field} must be non-empty"
        )));
    }
    Ok(())
}

fn validate_assurance(field: &str, assurance: &str) -> Result<(), MemoryError> {
    if matches!(assurance, "self_asserted" | "verified") {
        Ok(())
    } else {
        Err(MemoryError::InvalidArg(format!(
            "a2a {field} assurance must be self_asserted or verified"
        )))
    }
}

fn validate_subject_ref(subject_ref: &str) -> Result<(), MemoryError> {
    let valid = ["peer_publication:", "a2a_envelope:"].iter().any(|prefix| {
        subject_ref
            .strip_prefix(prefix)
            .is_some_and(|id| !id.trim().is_empty())
    });
    if valid {
        Ok(())
    } else {
        Err(MemoryError::InvalidArg(
            "a2a subject_ref must be peer_publication:<id> or a2a_envelope:<id>".to_string(),
        ))
    }
}

fn validate_limit(limit: usize) -> Result<i64, MemoryError> {
    if !(1..=MAX_A2A_STORAGE_BATCH).contains(&limit) {
        return Err(MemoryError::InvalidArg(format!(
            "a2a batch limit must be 1..={MAX_A2A_STORAGE_BATCH}"
        )));
    }
    Ok(limit as i64)
}

/// Resolve one exact historical same-host eligible admission. This does not
/// claim current presence: unavailable/rejected-only identities return None,
/// while an older self-asserted/verified row remains valid for offline inbox
/// addressing exactly as the owner clarification requires.
pub fn resolve_a2a_recipient_eligibility(
    conn: &Connection,
    agent_identity_id: &str,
) -> Result<Option<A2aRecipientEligibility>, MemoryError> {
    refuse_blank("recipient agent identity id", agent_identity_id)?;
    conn.query_row(
        "SELECT a.agent_identity_id,ia.admission_id,ia.state
         FROM agent_identities a
         JOIN identity_admissions ia ON ia.agent_identity_id=a.agent_identity_id
         WHERE a.agent_identity_id=?1 AND ia.state IN ('self_asserted','verified')
         ORDER BY ia.created_at DESC, ia.admission_id DESC LIMIT 1",
        [agent_identity_id],
        |row| {
            Ok(A2aRecipientEligibility {
                agent_identity_id: row.get(0)?,
                admission_id: row.get(1)?,
                identity_assurance: row.get(2)?,
                trust_domain: A2A_SAME_HOST_TRUST_DOMAIN.to_string(),
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn assurance_is_historical(
    conn: &Connection,
    agent_identity_id: &str,
    assurance: &str,
) -> Result<bool, MemoryError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM agent_identities a
             JOIN identity_admissions ia ON ia.agent_identity_id=a.agent_identity_id
             WHERE a.agent_identity_id=?1 AND ia.state=?2
               AND ia.state IN ('self_asserted','verified') LIMIT 1",
            params![agent_identity_id, assurance],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn row_to_envelope(row: &rusqlite::Row<'_>) -> rusqlite::Result<A2aEnvelope> {
    Ok(A2aEnvelope {
        envelope_id: row.get(0)?,
        kind: row.get(1)?,
        issuer_agent_identity_id: row.get(2)?,
        recipient_agent_identity_id: row.get(3)?,
        subject_ref: row.get(4)?,
        body: row.get(5)?,
        body_digest: row.get(6)?,
        issuer_identity_assurance: row.get(7)?,
        recipient_identity_assurance: row.get(8)?,
        issuer_trust_domain: row.get(9)?,
        recipient_trust_domain: row.get(10)?,
        idempotency_key: row.get(11)?,
        created_at: row.get(12)?,
        expires_at: row.get(13)?,
        current_state: row.get(14)?,
        state_version: row.get(15)?,
    })
}

const ENVELOPE_COLUMNS: &str = "envelope_id,kind,issuer_agent_identity_id,recipient_agent_identity_id,subject_ref,body,body_digest,issuer_identity_assurance,recipient_identity_assurance,issuer_trust_domain,recipient_trust_domain,idempotency_key,created_at,expires_at,current_state,state_version";

fn read_envelope_by_id(conn: &Connection, envelope_id: &str) -> Result<A2aEnvelope, MemoryError> {
    conn.query_row(
        &format!("SELECT {ENVELOPE_COLUMNS} FROM a2a_envelopes WHERE envelope_id=?1"),
        [envelope_id],
        row_to_envelope,
    )
    .map_err(Into::into)
}

fn read_received_receipt(
    conn: &Connection,
    envelope_id: &str,
) -> Result<A2aDeliveryReceipt, MemoryError> {
    conn.query_row(
        "SELECT receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,identity_assurance,trust_domain,occurred_at
         FROM a2a_delivery_receipts WHERE envelope_id=?1 AND state='received'",
        [envelope_id],
        |row| {
            Ok(A2aDeliveryReceipt {
                receipt_id: row.get(0)?,
                envelope_id: row.get(1)?,
                envelope_version: row.get(2)?,
                state: row.get(3)?,
                actor_agent_identity_id: row.get(4)?,
                identity_assurance: row.get(5)?,
                trust_domain: row.get(6)?,
                occurred_at: row.get(7)?,
            })
        },
    )
    .map_err(Into::into)
}

fn payload_matches(
    existing: &A2aEnvelope,
    request: &NewA2aEnvelope,
    created: &str,
    expires: &str,
) -> bool {
    let ttl_matches = || {
        let parse = |value: &str| chrono::DateTime::parse_from_rfc3339(value).ok();
        let existing_created = parse(&existing.created_at)?;
        let existing_expires = parse(&existing.expires_at)?;
        let replay_created = parse(created)?;
        let replay_expires = parse(expires)?;
        Some(existing_expires - existing_created == replay_expires - replay_created)
    };
    existing.kind == A2A_TURN_RESPONSE_KIND
        && existing.issuer_agent_identity_id == request.issuer_agent_identity_id
        && existing.recipient_agent_identity_id == request.recipient_agent_identity_id
        && existing.subject_ref == request.subject_ref
        && existing.body == request.body
        && existing.body_digest == request.body_digest
        && existing.issuer_identity_assurance == request.issuer_identity_assurance
        && existing.recipient_identity_assurance == request.recipient_identity_assurance
        && existing.issuer_trust_domain == A2A_SAME_HOST_TRUST_DOMAIN
        && existing.recipient_trust_domain == A2A_SAME_HOST_TRUST_DOMAIN
        && ttl_matches() == Some(true)
}

pub fn insert_a2a_envelope(
    conn: &mut Connection,
    request: &NewA2aEnvelope,
) -> Result<A2aInsertOutcome, MemoryError> {
    for (field, value) in [
        ("envelope id", request.envelope_id.as_str()),
        (
            "issuer agent identity id",
            request.issuer_agent_identity_id.as_str(),
        ),
        (
            "recipient agent identity id",
            request.recipient_agent_identity_id.as_str(),
        ),
        ("body", request.body.as_str()),
        ("body digest", request.body_digest.as_str()),
        ("idempotency key", request.idempotency_key.as_str()),
    ] {
        refuse_blank(field, value)?;
    }
    validate_subject_ref(&request.subject_ref)?;
    validate_assurance("issuer", &request.issuer_identity_assurance)?;
    validate_assurance("recipient", &request.recipient_identity_assurance)?;
    let created_at = normalize_utc_iso(&request.created_at)?;
    let expires_at = normalize_utc_iso(&request.expires_at)?;
    if expires_at <= created_at {
        return Err(MemoryError::InvalidArg(
            "a2a expires_at must be after created_at".to_string(),
        ));
    }
    if !assurance_is_historical(
        conn,
        &request.issuer_agent_identity_id,
        &request.issuer_identity_assurance,
    )? {
        return Err(MemoryError::InvalidArg(
            "a2a issuer assurance is not backed by an eligible admission".to_string(),
        ));
    }
    if !assurance_is_historical(
        conn,
        &request.recipient_agent_identity_id,
        &request.recipient_identity_assurance,
    )? {
        return Err(MemoryError::InvalidArg(
            "a2a recipient assurance is not backed by an eligible historical same-host admission"
                .to_string(),
        ));
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = tx
        .query_row(
            &format!("SELECT {ENVELOPE_COLUMNS} FROM a2a_envelopes WHERE issuer_agent_identity_id=?1 AND idempotency_key=?2"),
            params![request.issuer_agent_identity_id, request.idempotency_key],
            row_to_envelope,
        )
        .optional()?;
    if let Some(existing) = existing {
        if !payload_matches(&existing, request, &created_at, &expires_at) {
            return Err(MemoryError::Duplicate(format!(
                "a2a idempotency conflict for issuer '{}' and key '{}'",
                request.issuer_agent_identity_id, request.idempotency_key
            )));
        }
        let receipt = read_received_receipt(&tx, &existing.envelope_id)?;
        tx.commit()?;
        return Ok(A2aInsertOutcome::Replay {
            envelope: existing,
            receipt,
        });
    }

    tx.execute(
        "INSERT INTO a2a_envelopes
         (envelope_id,kind,issuer_agent_identity_id,recipient_agent_identity_id,subject_ref,body,body_digest,issuer_identity_assurance,recipient_identity_assurance,issuer_trust_domain,recipient_trust_domain,idempotency_key,created_at,expires_at,current_state,state_version)
         VALUES (?1,'turn_response/v1',?2,?3,?4,?5,?6,?7,?8,'same_host','same_host',?9,?10,?11,'received',1)",
        params![
            request.envelope_id,
            request.issuer_agent_identity_id,
            request.recipient_agent_identity_id,
            request.subject_ref,
            request.body,
            request.body_digest,
            request.issuer_identity_assurance,
            request.recipient_identity_assurance,
            request.idempotency_key,
            created_at,
            expires_at,
        ],
    )?;
    #[cfg(test)]
    test_hooks::fail_after_envelope_insert()?;
    let receipt_id = format!("{}:received:1", request.envelope_id);
    tx.execute(
        "INSERT INTO a2a_delivery_receipts
         (receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,identity_assurance,trust_domain,occurred_at)
         VALUES (?1,?2,1,'received',?3,?4,'same_host',?5)",
        params![
            receipt_id,
            request.envelope_id,
            request.issuer_agent_identity_id,
            request.issuer_identity_assurance,
            created_at,
        ],
    )?;
    let envelope = read_envelope_by_id(&tx, &request.envelope_id)?;
    let receipt = read_received_receipt(&tx, &request.envelope_id)?;
    tx.commit()?;
    Ok(A2aInsertOutcome::Created { envelope, receipt })
}

pub fn list_a2a_status(
    conn: &Connection,
    actor_agent_identity_id: &str,
    limit: usize,
) -> Result<Vec<A2aStatusRow>, MemoryError> {
    refuse_blank("status actor agent identity id", actor_agent_identity_id)?;
    let limit = validate_limit(limit)?;
    let mut stmt = conn.prepare(
        "SELECT e.envelope_id,e.kind,e.issuer_agent_identity_id,e.recipient_agent_identity_id,
                e.subject_ref,e.body_digest,e.created_at,e.expires_at,e.current_state,e.state_version
         FROM a2a_envelopes e
         WHERE e.issuer_agent_identity_id=?1 OR e.recipient_agent_identity_id=?1
         ORDER BY e.created_at DESC,e.envelope_id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![actor_agent_identity_id, limit], |row| {
        Ok(A2aStatusRow {
            envelope_id: row.get(0)?,
            kind: row.get(1)?,
            issuer_agent_identity_id: row.get(2)?,
            recipient_agent_identity_id: row.get(3)?,
            subject_ref: row.get(4)?,
            body_digest: row.get(5)?,
            created_at: row.get(6)?,
            expires_at: row.get(7)?,
            current_state: row.get(8)?,
            state_version: row.get(9)?,
            receipts: Vec::new(),
        })
    })?;
    let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut receipt_stmt = conn.prepare(
        "SELECT receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,
                identity_assurance,trust_domain,occurred_at
         FROM a2a_delivery_receipts WHERE envelope_id=?1 ORDER BY envelope_version",
    )?;
    for status in &mut rows {
        status.receipts = receipt_stmt
            .query_map([status.envelope_id.as_str()], |row| {
                Ok(A2aDeliveryReceipt {
                    receipt_id: row.get(0)?,
                    envelope_id: row.get(1)?,
                    envelope_version: row.get(2)?,
                    state: row.get(3)?,
                    actor_agent_identity_id: row.get(4)?,
                    identity_assurance: row.get(5)?,
                    trust_domain: row.get(6)?,
                    occurred_at: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
    }
    Ok(rows)
}

fn append_transition_receipt(
    tx: &Transaction<'_>,
    envelope: &A2aEnvelope,
    version: i64,
    state: &str,
    actor_agent_identity_id: &str,
    assurance: &str,
    occurred_at: &str,
) -> Result<(), MemoryError> {
    let receipt_id = format!("{}:{state}:{version}", envelope.envelope_id);
    tx.execute(
        "INSERT INTO a2a_delivery_receipts
         (receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,identity_assurance,trust_domain,occurred_at)
         VALUES (?1,?2,?3,?4,?5,?6,'same_host',?7)",
        params![
            receipt_id,
            envelope.envelope_id,
            version,
            state,
            actor_agent_identity_id,
            assurance,
            occurred_at,
        ],
    )?;
    Ok(())
}

fn expire_a2a_for_recipient_in_tx(
    tx: &Transaction<'_>,
    recipient_agent_identity_id: &str,
    limit: i64,
    now: &str,
) -> Result<usize, MemoryError> {
    let mut stmt = tx.prepare(&format!(
        "SELECT {ENVELOPE_COLUMNS} FROM a2a_envelopes
         WHERE recipient_agent_identity_id=?1 AND current_state IN ('received','accepted')
           AND expires_at<=?2 ORDER BY expires_at,envelope_id LIMIT ?3"
    ))?;
    let envelopes = stmt
        .query_map(
            params![recipient_agent_identity_id, now, limit],
            row_to_envelope,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for envelope in &envelopes {
        let next_version = envelope.state_version + 1;
        let changed = tx.execute(
            "UPDATE a2a_envelopes SET current_state='expired',state_version=?1
             WHERE envelope_id=?2 AND current_state=?3 AND state_version=?4",
            params![
                next_version,
                envelope.envelope_id,
                envelope.current_state,
                envelope.state_version,
            ],
        )?;
        if changed != 1 {
            return Err(MemoryError::Internal(format!(
                "a2a expiry transition lost atomic ownership for '{}'",
                envelope.envelope_id
            )));
        }
        append_transition_receipt(
            tx,
            envelope,
            next_version,
            "expired",
            recipient_agent_identity_id,
            &envelope.recipient_identity_assurance,
            now,
        )?;
    }
    Ok(envelopes.len())
}

pub fn expire_a2a_for_recipient(
    conn: &mut Connection,
    recipient_agent_identity_id: &str,
    limit: usize,
    now: &str,
) -> Result<usize, MemoryError> {
    refuse_blank(
        "expiry recipient agent identity id",
        recipient_agent_identity_id,
    )?;
    let limit = validate_limit(limit)?;
    let now = normalize_utc_iso(now)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let expired = expire_a2a_for_recipient_in_tx(&tx, recipient_agent_identity_id, limit, &now)?;
    tx.commit()?;
    Ok(expired)
}

pub fn consume_a2a_for_recipient(
    conn: &mut Connection,
    recipient_agent_identity_id: &str,
    limit: usize,
    now: &str,
) -> Result<Vec<A2aEnvelope>, MemoryError> {
    refuse_blank(
        "consume recipient agent identity id",
        recipient_agent_identity_id,
    )?;
    let limit = validate_limit(limit)?;
    let now = normalize_utc_iso(now)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    expire_a2a_for_recipient_in_tx(&tx, recipient_agent_identity_id, limit, &now)?;

    let mut stmt = tx.prepare(&format!(
        "SELECT {ENVELOPE_COLUMNS} FROM a2a_envelopes
         WHERE recipient_agent_identity_id=?1 AND current_state='received' AND expires_at>?2
         ORDER BY created_at,envelope_id LIMIT ?3"
    ))?;
    let envelopes = stmt
        .query_map(
            params![recipient_agent_identity_id, now, limit],
            row_to_envelope,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut consumed = Vec::with_capacity(envelopes.len());
    for envelope in envelopes {
        let accepted_version = envelope.state_version + 1;
        let changed = tx.execute(
            "UPDATE a2a_envelopes SET current_state='accepted',state_version=?1
             WHERE envelope_id=?2 AND recipient_agent_identity_id=?3
               AND current_state='received' AND state_version=?4",
            params![
                accepted_version,
                envelope.envelope_id,
                recipient_agent_identity_id,
                envelope.state_version,
            ],
        )?;
        if changed != 1 {
            return Err(MemoryError::Internal(format!(
                "a2a consume lost atomic ownership for '{}'",
                envelope.envelope_id
            )));
        }
        append_transition_receipt(
            &tx,
            &envelope,
            accepted_version,
            "accepted",
            recipient_agent_identity_id,
            &envelope.recipient_identity_assurance,
            &now,
        )?;
        #[cfg(test)]
        test_hooks::fail_after_accepted_receipt()?;
        let consumed_version = accepted_version + 1;
        let changed = tx.execute(
            "UPDATE a2a_envelopes SET current_state='consumed',state_version=?1
             WHERE envelope_id=?2 AND recipient_agent_identity_id=?3
               AND current_state='accepted' AND state_version=?4",
            params![
                consumed_version,
                envelope.envelope_id,
                recipient_agent_identity_id,
                accepted_version,
            ],
        )?;
        if changed != 1 {
            return Err(MemoryError::Internal(format!(
                "a2a accepted-to-consumed transition lost ownership for '{}'",
                envelope.envelope_id
            )));
        }
        append_transition_receipt(
            &tx,
            &envelope,
            consumed_version,
            "consumed",
            recipient_agent_identity_id,
            &envelope.recipient_identity_assurance,
            &now,
        )?;
        consumed.push(read_envelope_by_id(&tx, &envelope.envelope_id)?);
    }
    tx.commit()?;
    Ok(consumed)
}

#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::Cell;

    use crate::error::MemoryError;

    thread_local! {
        static FAIL_AFTER_ENVELOPE_INSERT: Cell<bool> = const { Cell::new(false) };
        static FAIL_AFTER_ACCEPTED_RECEIPT: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn arm_fail_after_envelope_insert() {
        FAIL_AFTER_ENVELOPE_INSERT.with(|flag| flag.set(true));
    }

    pub(crate) fn arm_fail_after_accepted_receipt() {
        FAIL_AFTER_ACCEPTED_RECEIPT.with(|flag| flag.set(true));
    }

    pub(super) fn fail_after_envelope_insert() -> Result<(), MemoryError> {
        if FAIL_AFTER_ENVELOPE_INSERT.with(|flag| flag.replace(false)) {
            return Err(MemoryError::Internal(
                "test_hooks: injected failure after a2a envelope insert".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn fail_after_accepted_receipt() -> Result<(), MemoryError> {
        if FAIL_AFTER_ACCEPTED_RECEIPT.with(|flag| flag.replace(false)) {
            return Err(MemoryError::Internal(
                "test_hooks: injected failure after a2a accepted receipt".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::session_claims::{
        insert_agent_identity, record_unverified_admission, AgentIdentity, UnverifiedAdmissionState,
    };
    use crate::{DbOpenContext, MemoryStore, StoreProfile};

    fn store() -> MemoryStore {
        MemoryStore::open_in_memory().expect("open product store")
    }

    fn identity(
        store: &MemoryStore,
        id: &str,
        admission: &str,
        assurance: UnverifiedAdmissionState,
    ) {
        insert_agent_identity(
            store.connection(),
            &AgentIdentity {
                agent_identity_id: id.to_string(),
                display_name: None,
                seat: None,
                capability_json: None,
                created_at: "2026-08-12T00:00:00Z".to_string(),
            },
        )
        .expect("identity");
        record_unverified_admission(
            store.connection(),
            admission,
            id,
            &format!("connection-{admission}"),
            assurance,
        )
        .expect("admission");
    }

    fn envelope(id: &str, key: &str, recipient: &str) -> NewA2aEnvelope {
        NewA2aEnvelope {
            envelope_id: id.to_string(),
            issuer_agent_identity_id: "issuer".to_string(),
            recipient_agent_identity_id: recipient.to_string(),
            subject_ref: "peer_publication:publication-1".to_string(),
            body: "scrubbed response".to_string(),
            body_digest: "digest-1".to_string(),
            issuer_identity_assurance: "self_asserted".to_string(),
            recipient_identity_assurance: "verified".to_string(),
            idempotency_key: key.to_string(),
            created_at: "2026-08-12T00:00:00Z".to_string(),
            expires_at: "2026-08-19T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn historical_admission_is_offline_eligible_and_preserves_exact_assurance() {
        let store = store();
        identity(
            &store,
            "recipient",
            "admission-verified",
            UnverifiedAdmissionState::SelfAsserted,
        );
        store
            .connection()
            .execute(
                "UPDATE identity_admissions SET state='verified' WHERE admission_id='admission-verified'",
                [],
            )
            .expect("test-only trusted admission fixture");

        let eligible = resolve_a2a_recipient_eligibility(store.connection(), "recipient")
            .expect("eligibility")
            .expect("eligible historical recipient");
        assert_eq!(eligible.admission_id, "admission-verified");
        assert_eq!(eligible.identity_assurance, "verified");
        assert_eq!(eligible.trust_domain, "same_host");

        identity(
            &store,
            "unavailable-only",
            "admission-unavailable",
            UnverifiedAdmissionState::Unavailable,
        );
        assert!(
            resolve_a2a_recipient_eligibility(store.connection(), "unavailable-only")
                .expect("lookup")
                .is_none()
        );
    }

    #[test]
    fn insert_received_receipt_replay_and_conflict_are_atomic() {
        let mut store = store();
        identity(
            &store,
            "issuer",
            "admission-issuer",
            UnverifiedAdmissionState::SelfAsserted,
        );
        identity(
            &store,
            "recipient",
            "admission-recipient",
            UnverifiedAdmissionState::SelfAsserted,
        );
        let mut request = envelope("envelope-1", "key-1", "recipient");
        request.recipient_identity_assurance = "self_asserted".to_string();

        let created = insert_a2a_envelope(store.connection_mut(), &request).expect("create");
        assert!(matches!(created, A2aInsertOutcome::Created { .. }));
        let replay = insert_a2a_envelope(store.connection_mut(), &request).expect("replay");
        assert!(matches!(replay, A2aInsertOutcome::Replay { .. }));

        let mut conflict = request.clone();
        conflict.envelope_id = "envelope-2".to_string();
        conflict.body = "different".to_string();
        conflict.body_digest = "digest-2".to_string();
        let error = insert_a2a_envelope(store.connection_mut(), &conflict).expect_err("conflict");
        assert!(error.to_string().contains("idempotency conflict"));

        let counts: (i64, i64) = store
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM a2a_envelopes), (SELECT COUNT(*) FROM a2a_delivery_receipts)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("counts");
        assert_eq!(counts, (1, 1));
    }

    #[test]
    fn scoped_status_and_bounded_consume_create_one_transition_chain() {
        let mut store = store();
        for id in ["issuer", "recipient", "stranger"] {
            identity(
                &store,
                id,
                &format!("admission-{id}"),
                UnverifiedAdmissionState::SelfAsserted,
            );
        }
        let mut request = envelope("envelope-1", "key-1", "recipient");
        request.recipient_identity_assurance = "self_asserted".to_string();
        insert_a2a_envelope(store.connection_mut(), &request).expect("create");

        let issuer_status = list_a2a_status(store.connection(), "issuer", 10).unwrap();
        assert_eq!(issuer_status.len(), 1);
        assert_eq!(issuer_status[0].receipts.len(), 1);
        assert_eq!(
            list_a2a_status(store.connection(), "recipient", 10)
                .unwrap()
                .len(),
            1
        );
        assert!(list_a2a_status(store.connection(), "stranger", 10)
            .unwrap()
            .is_empty());

        let first = consume_a2a_for_recipient(
            store.connection_mut(),
            "recipient",
            1,
            "2026-08-13T00:00:00Z",
        )
        .expect("consume");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].body, "scrubbed response");
        assert!(consume_a2a_for_recipient(
            store.connection_mut(),
            "recipient",
            1,
            "2026-08-13T00:00:00Z",
        )
        .expect("second consume")
        .is_empty());

        let states: Vec<String> = store
            .connection()
            .prepare("SELECT state FROM a2a_delivery_receipts ORDER BY envelope_version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(states, ["received", "accepted", "consumed"]);
    }

    #[test]
    fn expiry_is_once_and_injected_failure_rolls_back_both_sides() {
        let mut store = store();
        for id in ["issuer", "recipient"] {
            identity(
                &store,
                id,
                &format!("admission-{id}"),
                UnverifiedAdmissionState::SelfAsserted,
            );
        }
        let mut expired = envelope("expired", "expired-key", "recipient");
        expired.recipient_identity_assurance = "self_asserted".to_string();
        expired.expires_at = "2026-08-12T12:00:00Z".to_string();
        insert_a2a_envelope(store.connection_mut(), &expired).expect("insert expired candidate");
        assert_eq!(
            expire_a2a_for_recipient(
                store.connection_mut(),
                "recipient",
                10,
                "2026-08-13T00:00:00Z"
            )
            .unwrap(),
            1
        );
        assert_eq!(
            expire_a2a_for_recipient(
                store.connection_mut(),
                "recipient",
                10,
                "2026-08-13T00:00:00Z"
            )
            .unwrap(),
            0
        );

        test_hooks::arm_fail_after_envelope_insert();
        let mut failed = envelope("failed", "failed-key", "recipient");
        failed.recipient_identity_assurance = "self_asserted".to_string();
        assert!(insert_a2a_envelope(store.connection_mut(), &failed).is_err());
        let failed_rows: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM a2a_envelopes WHERE envelope_id='failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failed_rows, 0);

        let mut live = envelope("live", "live-key", "recipient");
        live.recipient_identity_assurance = "self_asserted".to_string();
        insert_a2a_envelope(store.connection_mut(), &live).expect("insert live candidate");
        test_hooks::arm_fail_after_accepted_receipt();
        assert!(consume_a2a_for_recipient(
            store.connection_mut(),
            "recipient",
            1,
            "2026-08-13T00:00:00Z",
        )
        .is_err());
        let (state, version, receipts): (String, i64, i64) = store
            .connection()
            .query_row(
                "SELECT e.current_state,e.state_version,COUNT(r.receipt_id) FROM a2a_envelopes e JOIN a2a_delivery_receipts r USING(envelope_id) WHERE e.envelope_id='live' GROUP BY e.envelope_id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((state.as_str(), version, receipts), ("received", 1, 1));
    }

    #[test]
    fn portable_store_omits_a2a_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("portable.db");
        let context =
            DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel);
        let store = MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap();
        for table in ["a2a_envelopes", "a2a_delivery_receipts"] {
            let present: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 0, "portable table {table}");
        }
    }

    #[test]
    fn legacy_v30_product_store_migrates_both_tables_and_stamp() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_schema(&connection).expect("build current product fixture");
        connection
            .execute_batch(
                "DROP TABLE a2a_delivery_receipts;
                 DROP TABLE a2a_envelopes;
                 DELETE FROM hard_state WHERE namespace='migrations' AND key='v31_a2a_mailbox';
                 PRAGMA user_version=30;",
            )
            .expect("downgrade fixture to exact v30 shape");

        let report = crate::db::migrations::run_data_migrations_with_profile(
            &mut connection,
            "global",
            std::path::Path::new(":memory:"),
            StoreProfile::TachiFull,
        )
        .expect("migrate v30 product store");
        assert_eq!(report.a2a_mailbox_schema_objects_created, 5);
        assert_eq!(
            crate::db::migrations::read_schema_version(&connection).unwrap(),
            crate::db::migrations::EXPECTED_SCHEMA_VERSION
        );
        for table in ["a2a_envelopes", "a2a_delivery_receipts"] {
            let present: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "migrated product table {table}");
        }
    }

    #[test]
    fn current_product_stamp_missing_both_a2a_tables_is_refused_not_repaired() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_schema(&connection).expect("build current product fixture");
        connection
            .execute_batch("DROP TABLE a2a_delivery_receipts; DROP TABLE a2a_envelopes;")
            .expect("damage both mailbox tables");
        let error = crate::db::migrations::run_data_migrations_with_profile(
            &mut connection,
            "global",
            std::path::Path::new(":memory:"),
            StoreProfile::TachiFull,
        )
        .expect_err("current stamp with missing mailbox must refuse");
        assert!(error.to_string().contains("incomplete v31 A2A mailbox"));
    }
}
