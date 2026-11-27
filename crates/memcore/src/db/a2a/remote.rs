//! Durable remote rejection and authorized, body-free task snapshots.
//!
//! The TLS/wire boundary validates the closed business payload and supplies
//! current operator grants. Storage binds it to the exact current mTLS receipt
//! and a real local gateway admission in one transaction. This slice has no
//! execution adapter: receipt version two always records execution_unavailable.

use super::{
    refuse_blank, resolve_current_local_admission, validate_limit, A2aDeliveryReceipt,
    A2aTransitionActor, A2A_SAME_HOST_TRUST_DOMAIN,
};
use crate::db::{
    with_current_verified_admission_write, VerifiedAdmissionBinding, VERIFIED_ADMISSION_MTLS_METHOD,
};
use crate::error::MemoryError;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::str::FromStr;

pub const A2A_REMOTE_JOB_KIND: &str = "tachi.remote_job/v1";
pub const MAX_REMOTE_JOB_BYTES: usize = 32768;
const MAX_JOB_TTL_SECS: i64 = 15 * 60;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
/// Largest accepted difference between the other host's clock and this one.
/// Origin and destination compare each other's timestamps against their own
/// clock, so only "the other host's timestamp is ahead of us" is relaxed by
/// this bound. Expiry keeps using the local clock: a first submission is never
/// admitted once its declared window has closed here.
pub const REMOTE_CLOCK_SKEW_TOLERANCE_SECS: i64 = 60;
/// Tasks with a retained body that one issuer may hold. It bounds what a
/// single enrolled peer can store. Retention scrubs bodies after 90 days, so
/// the capacity returns; a replay returns its existing task and never counts.
pub const MAX_RETAINED_REMOTE_TASKS_PER_PEER: i64 = 5_000;
/// `MemoryError::InvalidArg` message of a first submission refused by that bound.
pub const RETAINED_TASK_QUOTA_EXCEEDED: &str = "remote task retained task quota exceeded";

/// Whether a storage error is the retained-task quota refusing a first
/// submission from an issuer that already holds its bound.
pub fn is_retained_task_quota_exceeded(error: &MemoryError) -> bool {
    matches!(error, MemoryError::InvalidArg(message) if message == RETAINED_TASK_QUOTA_EXCEEDED)
}

/// Retained after body GC. The destination host comes from gateway config,
/// not from a claim by the remote sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteTaskHeader {
    pub repository: String,
    pub job: String,
    pub head_sha: String,
    pub origin_claim_id: String,
    pub origin_claim_version: i64,
    pub destination_host_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteJobGrant {
    pub repository: String,
    pub job: String,
}

/// Supplied only by the trusted gateway from its current operator config.
/// This is a scope filter, not an alternative identity admission mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTaskAccess {
    pub destination_agent_identity_id: String,
    pub destination_host_id: String,
    pub grants: Vec<RemoteJobGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRemoteTask {
    pub envelope_id: String,
    /// Canonical bytes of the closed wire object. Storage computes the digest
    /// directly from these bytes; timestamp normalization never changes them.
    pub canonical_body: String,
    pub nonce: String,
    pub header: RemoteTaskHeader,
    pub idempotency_key: String,
    pub issued_at: String,
    pub expires_at: String,
    pub recipient_agent_identity_id: String,
}

/// The persisted envelope without its retained business body. Reads and
/// replays cannot turn into an alternate content-delivery surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTaskEnvelope {
    pub envelope_id: String,
    pub kind: String,
    pub issuer_agent_identity_id: String,
    pub issuer_admission_id: String,
    pub recipient_agent_identity_id: String,
    pub recipient_admission_id: String,
    pub subject_ref: String,
    pub body_digest: String,
    pub issuer_identity_assurance: String,
    pub recipient_identity_assurance: String,
    pub issuer_trust_domain: String,
    pub recipient_trust_domain: String,
    pub issuer_trust_basis: String,
    pub recipient_trust_basis: String,
    pub idempotency_key: String,
    pub created_at: String,
    pub expires_at: String,
    pub current_state: String,
    pub state_version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRejectionReason {
    ExecutionUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReceiptDetail {
    pub reason: RemoteRejectionReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTaskReceipt {
    pub receipt: A2aDeliveryReceipt,
    pub detail: Option<RemoteReceiptDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTask {
    pub header: RemoteTaskHeader,
    pub envelope: RemoteTaskEnvelope,
    pub receipts: Vec<RemoteTaskReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTaskInsertOutcome {
    Created(RemoteTask),
    Replay(RemoteTask),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTaskPage {
    pub tasks: Vec<RemoteTask>,
    pub total_size: usize,
    /// Last visible task id, resolved against current authority on every use.
    pub next_cursor: Option<String>,
}

const TASK_COLUMNS: &str = "e.envelope_id,e.kind,e.issuer_agent_identity_id,e.issuer_admission_id,e.recipient_agent_identity_id,e.recipient_admission_id,e.subject_ref,e.body_digest,e.issuer_identity_assurance,e.recipient_identity_assurance,e.issuer_trust_domain,e.recipient_trust_domain,e.issuer_trust_basis,e.recipient_trust_basis,e.idempotency_key,e.created_at,e.expires_at,e.current_state,e.state_version,e.remote_metadata";

// Count, cursor resolution and page selection use this identical predicate.
// Grants are filtered before pagination; even a known foreign task id cannot
// change the visible count or become a cursor into another principal's rows.
const SCOPE: &str = "e.kind='tachi.remote_job/v1'
    AND e.issuer_agent_identity_id=?1 AND e.issuer_trust_domain=?2
    AND e.recipient_agent_identity_id=?3
    AND json_extract(e.remote_metadata,'$.destination_host_id')=?4
    AND EXISTS (
        SELECT 1 FROM json_each(?5) AS grant
        WHERE json_extract(grant.value,'$.repository')=json_extract(e.remote_metadata,'$.repository')
          AND json_extract(grant.value,'$.job')=json_extract(e.remote_metadata,'$.job')
    )";

fn invalid(message: &str) -> MemoryError {
    MemoryError::InvalidArg(format!("remote task {message}"))
}

fn require_mtls(binding: &VerifiedAdmissionBinding) -> Result<(), MemoryError> {
    if binding.verification_method != VERIFIED_ADMISSION_MTLS_METHOD {
        return Err(invalid("requires a current mtls-peer/v1 admission"));
    }
    Ok(())
}

fn validate_access(access: &RemoteTaskAccess) -> Result<String, MemoryError> {
    refuse_blank(
        "remote destination identity",
        &access.destination_agent_identity_id,
    )?;
    refuse_blank("remote destination host", &access.destination_host_id)?;
    Ok(serde_json::to_string(&access.grants)?)
}

fn has_grant(access: &RemoteTaskAccess, header: &RemoteTaskHeader) -> bool {
    access
        .grants
        .iter()
        .any(|grant| grant.repository == header.repository && grant.job == header.job)
}

fn validate_request(
    binding: &VerifiedAdmissionBinding,
    gateway: &A2aTransitionActor,
    access: &RemoteTaskAccess,
    request: &NewRemoteTask,
) -> Result<(), MemoryError> {
    for (field, value) in [
        ("remote envelope id", request.envelope_id.as_str()),
        ("remote nonce", request.nonce.as_str()),
        ("remote idempotency key", request.idempotency_key.as_str()),
        ("remote repository", request.header.repository.as_str()),
        ("remote head SHA", request.header.head_sha.as_str()),
        (
            "remote origin claim",
            request.header.origin_claim_id.as_str(),
        ),
    ] {
        refuse_blank(field, value)?;
    }
    if request.canonical_body.is_empty() || request.canonical_body.len() > MAX_REMOTE_JOB_BYTES {
        return Err(invalid("body must contain 1..=32768 bytes"));
    }
    if !matches!(request.header.job.as_str(), "review_revision" | "verify_pr")
        || !(0..=MAX_SAFE_INTEGER).contains(&request.header.origin_claim_version)
    {
        return Err(invalid("has an unsupported job or origin claim version"));
    }
    if request.recipient_agent_identity_id != gateway.agent_identity_id
        || request.recipient_agent_identity_id != access.destination_agent_identity_id
        || request.header.destination_host_id != access.destination_host_id
        || !has_grant(access, &request.header)
    {
        return Err(invalid(
            "is not authorized for this destination and repository/job",
        ));
    }
    // The outer wire parser owns the complete closed profile. Re-check the
    // fields that determine storage identity, replay and access here so a
    // producer cannot persist a header/grant unrelated to the digested body.
    let body: serde_json::Value = serde_json::from_str(&request.canonical_body)?;
    for (field, expected) in [
        ("kind", A2A_REMOTE_JOB_KIND),
        ("sender", binding.agent_identity_id.as_str()),
        ("recipient", request.recipient_agent_identity_id.as_str()),
        ("trust_domain", binding.trust_domain.as_str()),
        ("idempotency_key", request.idempotency_key.as_str()),
        ("nonce", request.nonce.as_str()),
        ("issued_at", request.issued_at.as_str()),
        ("expires_at", request.expires_at.as_str()),
        ("repository", request.header.repository.as_str()),
        ("head_sha", request.header.head_sha.as_str()),
        ("job", request.header.job.as_str()),
    ] {
        if body.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
            return Err(invalid(
                "body does not match its admission or retained header",
            ));
        }
    }
    if body["origin_claim"]["claim_id"].as_str() != Some(request.header.origin_claim_id.as_str())
        || body["origin_claim"]["transition_version"].as_i64()
            != Some(request.header.origin_claim_version)
    {
        return Err(invalid("body does not match its retained origin claim"));
    }
    Ok(())
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteTask> {
    let raw_header: String = row.get(19)?;
    let header = serde_json::from_str(&raw_header).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(19, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(RemoteTask {
        header,
        envelope: RemoteTaskEnvelope {
            envelope_id: row.get(0)?,
            kind: row.get(1)?,
            issuer_agent_identity_id: row.get(2)?,
            issuer_admission_id: row.get(3)?,
            recipient_agent_identity_id: row.get(4)?,
            recipient_admission_id: row.get(5)?,
            subject_ref: row.get(6)?,
            body_digest: row.get(7)?,
            issuer_identity_assurance: row.get(8)?,
            recipient_identity_assurance: row.get(9)?,
            issuer_trust_domain: row.get(10)?,
            recipient_trust_domain: row.get(11)?,
            issuer_trust_basis: row.get(12)?,
            recipient_trust_basis: row.get(13)?,
            idempotency_key: row.get(14)?,
            created_at: row.get(15)?,
            expires_at: row.get(16)?,
            current_state: row.get(17)?,
            state_version: row.get(18)?,
        },
        receipts: Vec::new(),
    })
}

fn read_receipts(conn: &Connection, task: &mut RemoteTask) -> Result<(), MemoryError> {
    let mut statement = conn.prepare(
        "SELECT receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,
                actor_admission_id,identity_assurance,trust_domain,trust_basis,occurred_at,detail
         FROM a2a_delivery_receipts WHERE envelope_id=?1 ORDER BY envelope_version",
    )?;
    task.receipts = statement
        .query_map([&task.envelope.envelope_id], |row| {
            let raw_detail: Option<String> = row.get(10)?;
            let detail = raw_detail
                .map(|detail| serde_json::from_str(&detail))
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        10,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
            Ok(RemoteTaskReceipt {
                receipt: A2aDeliveryReceipt {
                    receipt_id: row.get(0)?,
                    envelope_id: row.get(1)?,
                    envelope_version: row.get(2)?,
                    state: row.get(3)?,
                    actor_agent_identity_id: row.get(4)?,
                    actor_admission_id: row.get(5)?,
                    identity_assurance: row.get(6)?,
                    trust_domain: row.get(7)?,
                    trust_basis: row.get(8)?,
                    occurred_at: row.get(9)?,
                },
                detail,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(())
}

fn read_in_scope(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    access: &RemoteTaskAccess,
    grants: &str,
    envelope_id: &str,
) -> Result<Option<RemoteTask>, MemoryError> {
    let mut task = conn
        .query_row(
            &format!(
                "SELECT {TASK_COLUMNS} FROM a2a_envelopes e WHERE {SCOPE} AND e.envelope_id=?6"
            ),
            params![
                binding.agent_identity_id,
                binding.trust_domain,
                access.destination_agent_identity_id,
                access.destination_host_id,
                grants,
                envelope_id
            ],
            row_to_task,
        )
        .optional()?;
    if let Some(task) = &mut task {
        read_receipts(conn, task)?;
    }
    Ok(task)
}

/// Persist both truthful receipts atomically, or return the original snapshot
/// for an exact replay. The fresh time window is checked only after replay
/// lookup; reconnect, expiry and body GC never mint a second task.
pub fn insert_remote_rejection(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    gateway: &A2aTransitionActor,
    access: &RemoteTaskAccess,
    request: &NewRemoteTask,
) -> Result<RemoteTaskInsertOutcome, MemoryError> {
    require_mtls(binding)?;
    let grants = validate_access(access)?;
    validate_request(binding, gateway, access, request)?;
    let digest = format!("{:x}", Sha256::digest(request.canonical_body.as_bytes()));
    with_current_verified_admission_write(conn, binding, |conn| {
        let local = resolve_current_local_admission(
            conn,
            &gateway.agent_identity_id,
            &gateway.connection_id,
        )?
        .ok_or_else(|| invalid("gateway is not the exact current local admission"))?;
        let existing: Option<(String, String)> = conn
            .query_row(
                "SELECT envelope_id,body_digest FROM a2a_envelopes
                 WHERE kind='tachi.remote_job/v1' AND issuer_agent_identity_id=?1 AND idempotency_key=?2",
                params![binding.agent_identity_id, request.idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((envelope_id, existing_digest)) = existing {
            if existing_digest == digest {
                if let Some(task) = read_in_scope(conn, binding, access, &grants, &envelope_id)? {
                    if task.header == request.header {
                        return Ok(RemoteTaskInsertOutcome::Replay(task));
                    }
                }
            }
            return Err(MemoryError::Duplicate(
                "remote task idempotency conflict".into(),
            ));
        }
        let nonce_used: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM a2a_envelopes
             WHERE kind='tachi.remote_job/v1' AND issuer_agent_identity_id=?1
               AND issuer_trust_domain=?2 AND remote_nonce=?3)",
            params![
                binding.agent_identity_id,
                binding.trust_domain,
                request.nonce
            ],
            |row| row.get(0),
        )?;
        if nonce_used {
            return Err(MemoryError::Duplicate(
                "remote task nonce was already used".into(),
            ));
        }
        let now: String =
            conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
                row.get(0)
            })?;
        let parse = |value: &str| {
            jiff::Timestamp::from_str(value).map_err(|_| invalid("has an invalid timestamp"))
        };
        let issued = parse(&request.issued_at)?;
        let expires = parse(&request.expires_at)?;
        let authoritative_now = parse(&now)?;
        // The origin stamped issued_at with its own clock. Admit a job it just
        // created even when that clock runs ahead of this host, bounded by the
        // shared tolerance; expires_at is still judged by the local clock.
        if issued
            > authoritative_now + jiff::SignedDuration::from_secs(REMOTE_CLOCK_SKEW_TOLERANCE_SECS)
            || expires <= authoritative_now
            || expires <= issued
            || expires - issued > jiff::SignedDuration::from_secs(MAX_JOB_TTL_SECS)
        {
            return Err(invalid(
                "first admission requires issued_at within the clock tolerance, now < expires_at and TTL <= 15 minutes",
            ));
        }
        let created_at = crate::db::common::normalize_utc_iso(&request.issued_at)?;
        let expires_at = crate::db::common::normalize_utc_iso(&request.expires_at)?;
        // Counted inside the admission transaction, after the replay lookup, so
        // an issuer at its bound can still replay but not add another task.
        let retained: i64 = conn.query_row(
            "SELECT COUNT(*) FROM a2a_envelopes
             WHERE issuer_agent_identity_id=?1 AND kind='tachi.remote_job/v1' AND body IS NOT NULL",
            params![binding.agent_identity_id],
            |row| row.get(0),
        )?;
        if retained >= MAX_RETAINED_REMOTE_TASKS_PER_PEER {
            return Err(invalid("retained task quota exceeded"));
        }
        let metadata = serde_json::to_string(&request.header)?;
        conn.execute(
            "INSERT INTO a2a_envelopes
             (envelope_id,kind,issuer_agent_identity_id,issuer_admission_id,recipient_agent_identity_id,
              recipient_admission_id,subject_ref,body,body_digest,issuer_identity_assurance,
              recipient_identity_assurance,issuer_trust_domain,recipient_trust_domain,
              issuer_trust_basis,recipient_trust_basis,idempotency_key,created_at,expires_at,
              current_state,state_version,canonical_project,remote_nonce,remote_metadata)
             VALUES (?1,'tachi.remote_job/v1',?2,?3,?4,?5,?6,?7,?8,'verified','self_asserted',?9,
                     'same_host','current_verified_mtls_peer','current_local_connection',?10,?11,?12,
                     'remote_rejected',2,?13,?14,?15)",
            params![request.envelope_id, binding.agent_identity_id, binding.admission_id,
                gateway.agent_identity_id, local.admission_id,
                format!("work_claim:{}", request.header.origin_claim_id), request.canonical_body,
                digest, binding.trust_domain, request.idempotency_key, created_at, expires_at,
                format!("remote:{}", request.header.repository), request.nonce, metadata],
        )?;
        let rejection = serde_json::to_string(&RemoteReceiptDetail {
            reason: RemoteRejectionReason::ExecutionUnavailable,
        })?;
        for (version, state, detail) in [
            (1_i64, "remote_received", None),
            (2_i64, "remote_rejected", Some(rejection.as_str())),
        ] {
            conn.execute(
                "INSERT INTO a2a_delivery_receipts
                 (receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,
                  actor_admission_id,identity_assurance,trust_domain,trust_basis,occurred_at,detail)
                 VALUES (?1,?2,?3,?4,?5,?6,'self_asserted',?7,'current_local_connection',?8,?9)",
                params![
                    format!("{}:{state}:{version}", request.envelope_id),
                    request.envelope_id,
                    version,
                    state,
                    gateway.agent_identity_id,
                    local.admission_id,
                    A2A_SAME_HOST_TRUST_DOMAIN,
                    now,
                    detail
                ],
            )?;
        }
        let task = read_in_scope(conn, binding, access, &grants, &request.envelope_id)?
            .ok_or_else(|| {
                MemoryError::Internal("new remote task is outside its authorized scope".into())
            })?;
        Ok(RemoteTaskInsertOutcome::Created(task))
    })
}

/// Missing and unauthorized task ids both return `None`. The current receipt
/// check, header and receipts are read under one composable transaction.
pub fn read_remote_task(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    access: &RemoteTaskAccess,
    envelope_id: &str,
) -> Result<Option<RemoteTask>, MemoryError> {
    require_mtls(binding)?;
    let grants = validate_access(access)?;
    with_current_verified_admission_write(conn, binding, |conn| {
        read_in_scope(conn, binding, access, &grants, envelope_id)
    })
}

pub fn list_remote_tasks(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    access: &RemoteTaskAccess,
    limit: usize,
    cursor: Option<&str>,
) -> Result<RemoteTaskPage, MemoryError> {
    require_mtls(binding)?;
    let grants = validate_access(access)?;
    let limit = validate_limit(limit)?;
    with_current_verified_admission_write(conn, binding, |conn| {
        let cursor_created = match cursor {
            Some(cursor) => Some(conn.query_row(
                &format!("SELECT e.created_at FROM a2a_envelopes e WHERE {SCOPE} AND e.envelope_id=?6"),
                params![binding.agent_identity_id, binding.trust_domain, access.destination_agent_identity_id,
                    access.destination_host_id, grants, cursor],
                |row| row.get::<_, String>(0),
            ).optional()?.ok_or_else(|| invalid("cursor is unavailable"))?),
            None => None,
        };
        let count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM a2a_envelopes e WHERE {SCOPE}"),
            params![
                binding.agent_identity_id,
                binding.trust_domain,
                access.destination_agent_identity_id,
                access.destination_host_id,
                grants
            ],
            |row| row.get(0),
        )?;
        let mut statement = conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM a2a_envelopes e WHERE {SCOPE}
               AND (?6 IS NULL OR (e.created_at,e.envelope_id)<(?6,?7))
             ORDER BY e.created_at DESC,e.envelope_id DESC LIMIT ?8"
        ))?;
        let mut tasks = statement
            .query_map(
                params![
                    binding.agent_identity_id,
                    binding.trust_domain,
                    access.destination_agent_identity_id,
                    access.destination_host_id,
                    grants,
                    cursor_created,
                    cursor,
                    limit + 1
                ],
                row_to_task,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = tasks.len() > limit as usize;
        tasks.truncate(limit as usize);
        let next_cursor = if has_more {
            tasks.last().map(|task| task.envelope.envelope_id.clone())
        } else {
            None
        };
        for task in &mut tasks {
            read_receipts(conn, task)?;
        }
        Ok(RemoteTaskPage {
            tasks,
            total_size: usize::try_from(count)
                .map_err(|_| MemoryError::Internal("remote task count exceeds usize".into()))?,
            next_cursor,
        })
    })
}

#[cfg(test)]
mod tests;
