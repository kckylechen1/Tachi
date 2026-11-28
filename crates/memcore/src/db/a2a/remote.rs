//! Durable remote admission, run correlation, lifecycle receipts, and
//! authorized, body-free task snapshots.
//!
//! The TLS/wire boundary validates the closed business payload and supplies
//! current operator grants. Storage binds it to the exact current mTLS receipt
//! and a real local gateway admission in one transaction.

use super::{
    refuse_blank, resolve_current_local_admission, validate_limit, A2aDeliveryReceipt,
    A2aTransitionActor, A2A_SAME_HOST_TRUST_DOMAIN,
};
use crate::db::common::with_composable_write;
use crate::db::session_claims::{
    get_claim, insert_local_project_work_claim, release_work_claim, work_claim_project, ClaimState,
    NewWorkClaim, WorkClaimMode,
};
use crate::db::{
    get_verified_admission_receipt, verified_mtls_key_is_revoked,
    with_current_verified_admission_write, VerifiedAdmissionBinding,
    VERIFIED_ADMISSION_MTLS_METHOD,
};
use crate::error::MemoryError;
use crate::time_compat::millis_z;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<i64>,
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
    PolicyDenied,
    HeadNotFound,
    ResourceExhausted,
    ClaimConflict,
}

impl RemoteRejectionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExecutionUnavailable => "execution_unavailable",
            Self::PolicyDenied => "policy_denied",
            Self::HeadNotFound => "head_not_found",
            Self::ResourceExhausted => "resource_exhausted",
            Self::ClaimConflict => "claim_conflict",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAdapterKind {
    NativeReview,
    Dgx2VerifyPr,
}

impl RemoteAdapterKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NativeReview => "native_review",
            Self::Dgx2VerifyPr => "dgx2_verify_pr",
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "native_review" => Ok(Self::NativeReview),
            "dgx2_verify_pr" => Ok(Self::Dgx2VerifyPr),
            other => Err(format!("unknown remote adapter kind '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRunLifecycleState {
    Admitted,
    Started,
    CancelRequested,
    Completed,
    Failed,
    Canceled,
}

impl RemoteRunLifecycleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Started => "started",
            Self::CancelRequested => "cancel_requested",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "admitted" => Ok(Self::Admitted),
            "started" => Ok(Self::Started),
            "cancel_requested" => Ok(Self::CancelRequested),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            other => Err(format!("unknown remote run lifecycle state '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCleanupStatus {
    Pending,
    Cleaned,
    Failed,
}

impl RemoteCleanupStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Cleaned => "cleaned",
            Self::Failed => "failed",
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "pending" => Ok(Self::Pending),
            "cleaned" => Ok(Self::Cleaned),
            "failed" => Ok(Self::Failed),
            other => Err(format!("unknown remote cleanup status '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteTerminalOutcome {
    Completed,
    Failed,
    Canceled,
}

impl RemoteTerminalOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    fn envelope_state(&self) -> &'static str {
        match self {
            Self::Completed => "remote_completed",
            Self::Failed => "remote_failed",
            Self::Canceled => "remote_canceled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RemoteReceiptDetail {
    Refusal {
        reason: RemoteRejectionReason,
    },
    Admitted {
        local_run_id: String,
        local_claim_id: String,
        local_claim_version: i64,
        adapter_kind: RemoteAdapterKind,
        policy_profile: String,
        resource_slot: String,
        lease_expires_at: String,
    },
    Started {
        local_run_id: String,
        adapter_kind: RemoteAdapterKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_ref: Option<String>,
    },
    CancelRequested {
        local_run_id: String,
        requested_by_identity_id: String,
    },
    Terminal {
        local_run_id: String,
        outcome: RemoteTerminalOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary_digest: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_class: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim_released_version: Option<i64>,
    },
    Cleanup {
        local_run_id: String,
        cleanup_state: RemoteCleanupStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cleaned_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_class: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTaskReceipt {
    pub receipt: A2aDeliveryReceipt,
    pub detail: Option<RemoteReceiptDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTaskRun {
    pub local_run_id: String,
    pub canonical_project: String,
    pub local_claim_id: String,
    pub local_claim_version: i64,
    pub repository: String,
    pub job: String,
    pub head_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<i64>,
    pub adapter_kind: RemoteAdapterKind,
    pub policy_profile: String,
    pub resource_slot: String,
    pub lifecycle_state: RemoteRunLifecycleState,
    pub cleanup_state: RemoteCleanupStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    pub admitted_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTask {
    pub header: RemoteTaskHeader,
    pub envelope: RemoteTaskEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RemoteTaskRun>,
    pub receipts: Vec<RemoteTaskReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLocalAdmissionSpec {
    pub canonical_project: String,
    pub adapter_kind: RemoteAdapterKind,
    pub policy_profile: String,
    pub resource_slot: String,
    pub max_concurrent_runs: u32,
    pub lease_ttl_secs: u32,
    pub issue_ref: String,
    pub worktree_path: String,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAdmissionDecision {
    Admit(RemoteLocalAdmissionSpec),
    /// The gateway supervisor owns terminal transitions, including lease expiry.
    AdmitSupervised(RemoteLocalAdmissionSpec),
    Reject(RemoteRejectionReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTerminalTransition {
    Applied(RemoteTaskRun),
    AlreadyTerminal(RemoteTaskRun),
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
    match request.header.job.as_str() {
        "review_revision" => {
            if request.header.pull_request.is_some() || body.get("pull_request").is_some() {
                return Err(invalid("review_revision must not carry pull_request"));
            }
        }
        "verify_pr" => match (
            request.header.pull_request,
            body.get("pull_request").and_then(serde_json::Value::as_i64),
        ) {
            (Some(pr), Some(body_pr))
                if pr == body_pr && (1..=i64::from(u32::MAX)).contains(&pr) => {}
            _ => {
                return Err(invalid(
                    "verify_pr requires a matching positive pull_request",
                ))
            }
        },
        _ => unreachable!("checked above"),
    }
    Ok(())
}

fn validate_admission_spec(
    header: &RemoteTaskHeader,
    spec: &RemoteLocalAdmissionSpec,
) -> Result<(), MemoryError> {
    for (field, value) in [
        ("local canonical project", spec.canonical_project.as_str()),
        ("local policy profile", spec.policy_profile.as_str()),
        ("local resource slot", spec.resource_slot.as_str()),
        ("local issue ref", spec.issue_ref.as_str()),
    ] {
        refuse_blank(field, value)?;
    }
    if spec.max_concurrent_runs == 0 || spec.lease_ttl_secs == 0 {
        return Err(invalid("concurrency limit and lease TTL must be positive"));
    }
    if spec.writable && spec.worktree_path.trim().is_empty() {
        return Err(invalid("writable admission requires a worktree path"));
    }
    let expected_adapter = match header.job.as_str() {
        "review_revision" => RemoteAdapterKind::NativeReview,
        "verify_pr" => RemoteAdapterKind::Dgx2VerifyPr,
        _ => return Err(invalid("unsupported remote job")),
    };
    if spec.adapter_kind != expected_adapter {
        return Err(invalid("adapter kind does not match remote job"));
    }
    Ok(())
}

fn validate_digest_64(value: &str) -> Result<(), MemoryError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid(
            "summary digest must be 64 lowercase hex characters",
        ));
    }
    Ok(())
}

fn validate_error_class(value: &str) -> Result<(), MemoryError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return Err(invalid("error class must be a bounded snake_case token"));
    }
    Ok(())
}

fn sqlite_parse_ts(value: &str) -> Result<jiff::Timestamp, MemoryError> {
    jiff::Timestamp::from_str(value).map_err(|_| invalid("has an invalid timestamp"))
}

fn sqlite_now_ts(conn: &Connection) -> Result<(String, jiff::Timestamp), MemoryError> {
    let now: String = conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
        row.get(0)
    })?;
    let parsed = sqlite_parse_ts(&now)?;
    Ok((now, parsed))
}

const RUN_COLUMNS: &str = "local_run_id,canonical_project,local_claim_id,local_claim_version,repository,job,head_sha,pull_request,adapter_kind,policy_profile,resource_slot,lifecycle_state,cleanup_state,external_ref,admitted_at,updated_at";

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteTaskRun> {
    let adapter_raw: String = row.get(8)?;
    let lifecycle_raw: String = row.get(11)?;
    let cleanup_raw: String = row.get(12)?;
    let conv_err = |col, message: String| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            )),
        )
    };
    Ok(RemoteTaskRun {
        local_run_id: row.get(0)?,
        canonical_project: row.get(1)?,
        local_claim_id: row.get(2)?,
        local_claim_version: row.get(3)?,
        repository: row.get(4)?,
        job: row.get(5)?,
        head_sha: row.get(6)?,
        pull_request: row.get(7)?,
        adapter_kind: RemoteAdapterKind::parse(&adapter_raw).map_err(|e| conv_err(8, e))?,
        policy_profile: row.get(9)?,
        resource_slot: row.get(10)?,
        lifecycle_state: RemoteRunLifecycleState::parse(&lifecycle_raw)
            .map_err(|e| conv_err(11, e))?,
        cleanup_state: RemoteCleanupStatus::parse(&cleanup_raw).map_err(|e| conv_err(12, e))?,
        external_ref: row.get(13)?,
        admitted_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn read_run_row(
    conn: &Connection,
    envelope_id: &str,
) -> Result<Option<RemoteTaskRun>, MemoryError> {
    Ok(conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM a2a_remote_runs WHERE envelope_id=?1"),
            [envelope_id],
            row_to_run,
        )
        .optional()?)
}

fn hydrate_task(conn: &Connection, task: &mut RemoteTask) -> Result<(), MemoryError> {
    read_receipts(conn, task)?;
    task.run = if task.envelope.current_state == "remote_rejected" {
        None
    } else {
        read_run_row(conn, &task.envelope.envelope_id)?
    };
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
        run: None,
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
        hydrate_task(conn, task)?;
    }
    Ok(task)
}

#[allow(clippy::too_many_arguments)]
fn insert_receipt(
    conn: &Connection,
    envelope_id: &str,
    version: i64,
    state: &str,
    gateway: &A2aTransitionActor,
    local_admission_id: &str,
    occurred_at: &str,
    detail: Option<&RemoteReceiptDetail>,
) -> Result<(), MemoryError> {
    let encoded = detail.map(serde_json::to_string).transpose()?;
    conn.execute(
        "INSERT INTO a2a_delivery_receipts
         (receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,
          actor_admission_id,identity_assurance,trust_domain,trust_basis,occurred_at,detail)
         VALUES (?1,?2,?3,?4,?5,?6,'self_asserted',?7,'current_local_connection',?8,?9)",
        params![
            format!("{envelope_id}:{state}:{version}"),
            envelope_id,
            version,
            state,
            gateway.agent_identity_id,
            local_admission_id,
            A2A_SAME_HOST_TRUST_DOMAIN,
            occurred_at,
            encoded
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_envelope(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    gateway: &A2aTransitionActor,
    local_admission_id: &str,
    request: &NewRemoteTask,
    digest: &str,
    created_at: &str,
    expires_at: &str,
    current_state: &str,
) -> Result<(), MemoryError> {
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
                 ?13,2,?14,?15,?16)",
        params![
            request.envelope_id,
            binding.agent_identity_id,
            binding.admission_id,
            gateway.agent_identity_id,
            local_admission_id,
            format!("work_claim:{}", request.header.origin_claim_id),
            request.canonical_body,
            digest,
            binding.trust_domain,
            request.idempotency_key,
            created_at,
            expires_at,
            current_state,
            format!("remote:{}", request.header.repository),
            request.nonce,
            metadata
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish_rejected_admission(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    gateway: &A2aTransitionActor,
    local_admission_id: &str,
    access: &RemoteTaskAccess,
    grants: &str,
    request: &NewRemoteTask,
    digest: &str,
    created_at: &str,
    expires_at: &str,
    now: &str,
    reason: RemoteRejectionReason,
) -> Result<RemoteTaskInsertOutcome, MemoryError> {
    insert_envelope(
        conn,
        binding,
        gateway,
        local_admission_id,
        request,
        digest,
        created_at,
        expires_at,
        "remote_rejected",
    )?;
    insert_receipt(
        conn,
        &request.envelope_id,
        1,
        "remote_received",
        gateway,
        local_admission_id,
        now,
        None,
    )?;
    let rejection = RemoteReceiptDetail::Refusal { reason };
    insert_receipt(
        conn,
        &request.envelope_id,
        2,
        "remote_rejected",
        gateway,
        local_admission_id,
        now,
        Some(&rejection),
    )?;
    let task =
        read_in_scope(conn, binding, access, grants, &request.envelope_id)?.ok_or_else(|| {
            MemoryError::Internal("new remote task is outside its authorized scope".into())
        })?;
    Ok(RemoteTaskInsertOutcome::Created(task))
}

/// Atomically persist an admission or durable refusal, or return the original
/// snapshot for an exact replay. Replay lookup precedes decision evaluation so
/// upgraded nodes preserve historical refusal decisions without relaunching.
pub fn admit_remote_task(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    gateway: &A2aTransitionActor,
    access: &RemoteTaskAccess,
    request: &NewRemoteTask,
    decision: &RemoteAdmissionDecision,
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
                    if headers_match_for_replay(&task.header, &request.header) {
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
        let (now, authoritative_now) = sqlite_now_ts(conn)?;
        let issued = sqlite_parse_ts(&request.issued_at)?;
        let expires = sqlite_parse_ts(&request.expires_at)?;
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
        let retained: i64 = conn.query_row(
            "SELECT COUNT(*) FROM a2a_envelopes
             WHERE issuer_agent_identity_id=?1 AND kind='tachi.remote_job/v1' AND body IS NOT NULL",
            params![binding.agent_identity_id],
            |row| row.get(0),
        )?;
        if retained >= MAX_RETAINED_REMOTE_TASKS_PER_PEER {
            return Err(invalid("retained task quota exceeded"));
        }
        let spec = match decision {
            RemoteAdmissionDecision::Reject(reason) => {
                return finish_rejected_admission(
                    conn,
                    binding,
                    gateway,
                    &local.admission_id,
                    access,
                    &grants,
                    request,
                    &digest,
                    &created_at,
                    &expires_at,
                    &now,
                    *reason,
                );
            }
            RemoteAdmissionDecision::Admit(spec)
            | RemoteAdmissionDecision::AdmitSupervised(spec) => {
                validate_admission_spec(&request.header, spec)?;
                spec
            }
        };
        if matches!(decision, RemoteAdmissionDecision::Admit(_)) {
            reconcile_active_remote_runs(conn, gateway)?;
        }
        let active_in_slot: i64 = conn.query_row(
            "SELECT COUNT(*) FROM a2a_remote_runs
             WHERE resource_slot=?1 AND lifecycle_state IN ('admitted','started','cancel_requested')",
            params![spec.resource_slot],
            |row| row.get(0),
        )?;
        if active_in_slot >= i64::from(spec.max_concurrent_runs) {
            return finish_rejected_admission(
                conn,
                binding,
                gateway,
                &local.admission_id,
                access,
                &grants,
                request,
                &digest,
                &created_at,
                &expires_at,
                &now,
                RemoteRejectionReason::ResourceExhausted,
            );
        }
        let local_run_id = format!("rrun-{}", uuid::Uuid::new_v4());
        let local_claim_id = format!("rclaim-{}", uuid::Uuid::new_v4());
        let lease_expires_at = millis_z(
            authoritative_now + jiff::SignedDuration::from_secs(i64::from(spec.lease_ttl_secs)),
        );
        let claim = NewWorkClaim {
            claim_id: local_claim_id.clone(),
            agent_identity_id: gateway.agent_identity_id.clone(),
            session_client: None,
            issue_ref: Some(spec.issue_ref.clone()),
            flow_id: None,
            dispatch_id: Some(local_run_id.clone()),
            branch: request.header.repository.clone(),
            worktree_path: spec.worktree_path.clone(),
            declared_file_scope: "readonly:remote_job".into(),
            role: match request.header.job.as_str() {
                "review_revision" => "remote_review".into(),
                _ => "remote_verify".into(),
            },
            mode: if spec.writable {
                WorkClaimMode::Writable
            } else {
                WorkClaimMode::ReadOnly
            },
            expected_head: request.header.head_sha.clone(),
            lease_expires_at: lease_expires_at.clone(),
            created_at: now.clone(),
        };
        match insert_local_project_work_claim(
            conn,
            &claim,
            &gateway.connection_id,
            &spec.canonical_project,
        ) {
            Ok(()) => {}
            Err(MemoryError::WorkClaimConflict(_))
            | Err(MemoryError::WorkClaimIncompatibleState(_)) => {
                return finish_rejected_admission(
                    conn,
                    binding,
                    gateway,
                    &local.admission_id,
                    access,
                    &grants,
                    request,
                    &digest,
                    &created_at,
                    &expires_at,
                    &now,
                    RemoteRejectionReason::ClaimConflict,
                );
            }
            Err(other) => return Err(other),
        }
        insert_envelope(
            conn,
            binding,
            gateway,
            &local.admission_id,
            request,
            &digest,
            &created_at,
            &expires_at,
            "remote_admitted",
        )?;
        conn.execute(
            "INSERT INTO a2a_remote_runs
             (envelope_id,local_run_id,canonical_project,local_claim_id,local_claim_version,
              repository,job,head_sha,pull_request,adapter_kind,policy_profile,resource_slot,
              lifecycle_state,cleanup_state,external_ref,admitted_at,updated_at)
             VALUES (?1,?2,?3,?4,0,?5,?6,?7,?8,?9,?10,?11,'admitted','pending',NULL,?12,?12)",
            params![
                request.envelope_id,
                local_run_id,
                spec.canonical_project,
                local_claim_id,
                request.header.repository,
                request.header.job,
                request.header.head_sha,
                request.header.pull_request,
                spec.adapter_kind.as_str(),
                spec.policy_profile,
                spec.resource_slot,
                now
            ],
        )?;
        insert_receipt(
            conn,
            &request.envelope_id,
            1,
            "remote_received",
            gateway,
            &local.admission_id,
            &now,
            None,
        )?;
        let admitted_detail = RemoteReceiptDetail::Admitted {
            local_run_id,
            local_claim_id,
            local_claim_version: 0,
            adapter_kind: spec.adapter_kind,
            policy_profile: spec.policy_profile.clone(),
            resource_slot: spec.resource_slot.clone(),
            lease_expires_at,
        };
        insert_receipt(
            conn,
            &request.envelope_id,
            2,
            "remote_admitted",
            gateway,
            &local.admission_id,
            &now,
            Some(&admitted_detail),
        )?;
        let task = read_in_scope(conn, binding, access, &grants, &request.envelope_id)?
            .ok_or_else(|| {
                MemoryError::Internal("new remote task is outside its authorized scope".into())
            })?;
        Ok(RemoteTaskInsertOutcome::Created(task))
    })
}

/// Persist both truthful refusal receipts atomically, or return the original
/// snapshot for an exact replay.
pub fn insert_remote_rejection(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    gateway: &A2aTransitionActor,
    access: &RemoteTaskAccess,
    request: &NewRemoteTask,
) -> Result<RemoteTaskInsertOutcome, MemoryError> {
    admit_remote_task(
        conn,
        binding,
        gateway,
        access,
        request,
        &RemoteAdmissionDecision::Reject(RemoteRejectionReason::ExecutionUnavailable),
    )
}

/// Transition an admitted run into `started` only after rechecking expiry,
/// non-revoked sender mTLS identity, and the active local WorkClaim.
pub fn record_remote_task_started(
    conn: &Connection,
    gateway: &A2aTransitionActor,
    envelope_id: &str,
    external_ref: Option<&str>,
) -> Result<RemoteTaskRun, MemoryError> {
    if let Some(ext) = external_ref {
        refuse_blank("external reference", ext)?;
    }
    with_composable_write(conn, |conn| {
        let local = resolve_current_local_admission(
            conn,
            &gateway.agent_identity_id,
            &gateway.connection_id,
        )?
        .ok_or_else(|| invalid("gateway is not the exact current local admission"))?;
        let (now, authoritative_now) = sqlite_now_ts(conn)?;
        let row: Option<(String, String, String, String, String, i64, String)> = conn
            .query_row(
                "SELECT issuer_agent_identity_id,issuer_admission_id,issuer_trust_domain,
                        recipient_agent_identity_id,current_state,state_version,expires_at
                 FROM a2a_envelopes WHERE envelope_id=?1 AND kind='tachi.remote_job/v1'",
                [envelope_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            issuer_identity,
            issuer_admission_id,
            issuer_domain,
            recipient_identity,
            envelope_state,
            state_version,
            expires_at,
        )) = row
        else {
            return Err(MemoryError::NotFound(format!("remote task {envelope_id}")));
        };
        let run = read_run_row(conn, envelope_id)?
            .ok_or_else(|| invalid("run does not exist for remote task"))?;
        if recipient_identity != gateway.agent_identity_id
            || envelope_state != "remote_admitted"
            || run.lifecycle_state != RemoteRunLifecycleState::Admitted
        {
            return Err(invalid("remote task is not in admitted state for start"));
        }
        if sqlite_parse_ts(&expires_at)? <= authoritative_now {
            return Err(invalid(
                "cannot start remote task after its start window expired",
            ));
        }
        let receipt = get_verified_admission_receipt(conn, &issuer_admission_id)?
            .ok_or_else(|| invalid("sender verified admission receipt is unavailable"))?;
        if receipt.current_state != "verified"
            || receipt.agent_identity_id != issuer_identity
            || receipt.trust_domain != issuer_domain
            || receipt.verification_method != VERIFIED_ADMISSION_MTLS_METHOD
            || sqlite_parse_ts(&receipt.evidence_expires_at)? <= authoritative_now
            || verified_mtls_key_is_revoked(conn, &receipt.issuer_id, &receipt.evidence_ref)?
        {
            return Err(invalid(
                "sender mTLS admission is expired or revoked at launch boundary",
            ));
        }
        let claim = get_claim(conn, &run.local_claim_id)?
            .ok_or_else(|| invalid("local WorkClaim is missing at launch boundary"))?;
        if claim.state != ClaimState::Active
            || claim.transition_version != run.local_claim_version
            || claim.agent_identity_id.as_deref() != Some(&gateway.agent_identity_id)
            || work_claim_project(conn, &run.local_claim_id)?.as_deref()
                != Some(&run.canonical_project)
            || !claim
                .lease_expires_at
                .as_deref()
                .map(sqlite_parse_ts)
                .transpose()?
                .is_some_and(|exp| exp > authoritative_now)
        {
            return Err(invalid(
                "local WorkClaim is inactive, mismatched, or expired at launch boundary",
            ));
        }
        let next_version = state_version + 1;
        let ext_owned = external_ref.map(str::to_string);
        let changed = conn.execute(
            "UPDATE a2a_remote_runs
             SET lifecycle_state='started',external_ref=COALESCE(?2,external_ref),updated_at=?3
             WHERE envelope_id=?1 AND lifecycle_state='admitted'",
            params![envelope_id, ext_owned, now],
        )?;
        if changed != 1 {
            return Err(invalid("remote run state changed concurrently"));
        }
        conn.execute(
            "UPDATE a2a_envelopes SET current_state='remote_started',state_version=?2
             WHERE envelope_id=?1 AND state_version=?3 AND current_state='remote_admitted'",
            params![envelope_id, next_version, state_version],
        )?;
        let detail = RemoteReceiptDetail::Started {
            local_run_id: run.local_run_id.clone(),
            adapter_kind: run.adapter_kind,
            external_ref: ext_owned,
        };
        insert_receipt(
            conn,
            envelope_id,
            next_version,
            "remote_started",
            gateway,
            &local.admission_id,
            &now,
            Some(&detail),
        )?;
        read_run_row(conn, envelope_id)?
            .ok_or_else(|| MemoryError::Internal("started remote run disappeared".into()))
    })
}

/// Authenticated peer request to cancel an admitted/started run. Cross-peer or
/// unauthorized IDs return `Ok(None)` so `CancelTask` reveals no foreign IDs.
pub fn record_remote_task_cancel_requested(
    conn: &Connection,
    binding: &VerifiedAdmissionBinding,
    gateway: &A2aTransitionActor,
    access: &RemoteTaskAccess,
    envelope_id: &str,
) -> Result<Option<RemoteTask>, MemoryError> {
    require_mtls(binding)?;
    let grants = validate_access(access)?;
    with_current_verified_admission_write(conn, binding, |conn| {
        let local = resolve_current_local_admission(
            conn,
            &gateway.agent_identity_id,
            &gateway.connection_id,
        )?
        .ok_or_else(|| invalid("gateway is not the exact current local admission"))?;
        let Some(task) = read_in_scope(conn, binding, access, &grants, envelope_id)? else {
            return Ok(None);
        };
        match task.envelope.current_state.as_str() {
            "remote_rejected" | "remote_completed" | "remote_failed" | "remote_canceled" => {
                return Err(invalid("task is already terminal and cannot be canceled"));
            }
            "remote_cancel_requested" => return Ok(Some(task)),
            "remote_admitted" | "remote_started" => {}
            _ => return Err(invalid("task state cannot accept cancel request")),
        }
        let run = task
            .run
            .as_ref()
            .ok_or_else(|| invalid("admitted task is missing its local run record"))?;
        let (now, _) = sqlite_now_ts(conn)?;
        let next_version = task.envelope.state_version + 1;
        let changed = conn.execute(
            "UPDATE a2a_remote_runs SET lifecycle_state='cancel_requested',updated_at=?2
             WHERE envelope_id=?1 AND lifecycle_state IN ('admitted','started')",
            params![envelope_id, now],
        )?;
        if changed != 1 {
            return Err(invalid(
                "remote run state changed concurrently during cancel",
            ));
        }
        conn.execute(
            "UPDATE a2a_envelopes SET current_state='remote_cancel_requested',state_version=?2
             WHERE envelope_id=?1 AND state_version=?3",
            params![envelope_id, next_version, task.envelope.state_version],
        )?;
        let detail = RemoteReceiptDetail::CancelRequested {
            local_run_id: run.local_run_id.clone(),
            requested_by_identity_id: binding.agent_identity_id.clone(),
        };
        insert_receipt(
            conn,
            envelope_id,
            next_version,
            "remote_cancel_requested",
            gateway,
            &local.admission_id,
            &now,
            Some(&detail),
        )?;
        read_in_scope(conn, binding, access, &grants, envelope_id)
    })
}

/// Persist terminal lifecycle truth and release the destination WorkClaim. Late
/// terminal calls return `AlreadyTerminal` without modifying state or receipts.
pub fn record_remote_task_terminal(
    conn: &Connection,
    gateway: &A2aTransitionActor,
    envelope_id: &str,
    outcome: RemoteTerminalOutcome,
    summary_digest: Option<&str>,
    error_class: Option<&str>,
    external_ref: Option<&str>,
) -> Result<RemoteTerminalTransition, MemoryError> {
    if let Some(digest) = summary_digest {
        validate_digest_64(digest)?;
    }
    if let Some(err) = error_class {
        validate_error_class(err)?;
    }
    if let Some(ext) = external_ref {
        refuse_blank("external reference", ext)?;
    }
    with_composable_write(conn, |conn| {
        let local = resolve_current_local_admission(
            conn,
            &gateway.agent_identity_id,
            &gateway.connection_id,
        )?
        .ok_or_else(|| invalid("gateway is not the exact current local admission"))?;
        let env_row: Option<(String, String, i64)> = conn
            .query_row(
                "SELECT recipient_agent_identity_id,current_state,state_version
                 FROM a2a_envelopes WHERE envelope_id=?1 AND kind='tachi.remote_job/v1'",
                [envelope_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((recipient_identity, _, state_version)) = env_row else {
            return Err(MemoryError::NotFound(format!("remote task {envelope_id}")));
        };
        let run = read_run_row(conn, envelope_id)?
            .ok_or_else(|| invalid("run does not exist for remote task"))?;
        if recipient_identity != gateway.agent_identity_id {
            return Err(invalid("gateway identity does not own this remote run"));
        }
        if matches!(
            run.lifecycle_state,
            RemoteRunLifecycleState::Completed
                | RemoteRunLifecycleState::Failed
                | RemoteRunLifecycleState::Canceled
        ) {
            return Ok(RemoteTerminalTransition::AlreadyTerminal(run));
        }
        if outcome == RemoteTerminalOutcome::Completed {
            let has_started: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM a2a_delivery_receipts
                 WHERE envelope_id=?1 AND state='remote_started')",
                [envelope_id],
                |row| row.get(0),
            )?;
            if !has_started
                || !matches!(
                    run.lifecycle_state,
                    RemoteRunLifecycleState::Started | RemoteRunLifecycleState::CancelRequested
                )
            {
                return Err(invalid(
                    "cannot record completed outcome without an authenticated started receipt",
                ));
            }
        }
        let claim_released_version = match get_claim(conn, &run.local_claim_id)? {
            Some(claim)
                if matches!(claim.state, ClaimState::Active | ClaimState::Orphaned)
                    && claim.agent_identity_id.as_deref()
                        == Some(gateway.agent_identity_id.as_str())
                    && claim.transition_version == run.local_claim_version =>
            {
                Some(release_work_claim(
                    conn,
                    &run.local_claim_id,
                    &gateway.agent_identity_id,
                    run.local_claim_version,
                    outcome.as_str(),
                )?)
            }
            _ => None,
        };
        let (now, _) = sqlite_now_ts(conn)?;
        let next_version = state_version + 1;
        let final_ext = external_ref
            .map(str::to_string)
            .or(run.external_ref.clone());
        conn.execute(
            "UPDATE a2a_remote_runs
             SET lifecycle_state=?2,external_ref=?3,updated_at=?4
             WHERE envelope_id=?1 AND lifecycle_state IN ('admitted','started','cancel_requested')",
            params![envelope_id, outcome.as_str(), final_ext, now],
        )?;
        conn.execute(
            "UPDATE a2a_envelopes SET current_state=?2,state_version=?3
             WHERE envelope_id=?1 AND state_version=?4",
            params![
                envelope_id,
                outcome.envelope_state(),
                next_version,
                state_version
            ],
        )?;
        let detail = RemoteReceiptDetail::Terminal {
            local_run_id: run.local_run_id.clone(),
            outcome,
            summary_digest: summary_digest.map(str::to_string),
            error_class: error_class.map(str::to_string),
            external_ref: final_ext,
            claim_released_version,
        };
        insert_receipt(
            conn,
            envelope_id,
            next_version,
            outcome.envelope_state(),
            gateway,
            &local.admission_id,
            &now,
            Some(&detail),
        )?;
        let updated = read_run_row(conn, envelope_id)?
            .ok_or_else(|| MemoryError::Internal("terminal remote run disappeared".into()))?;
        Ok(RemoteTerminalTransition::Applied(updated))
    })
}

/// Record post-terminal cleanup outcome while preserving `a2a_envelopes.current_state`.
pub fn record_remote_task_cleanup(
    conn: &Connection,
    gateway: &A2aTransitionActor,
    envelope_id: &str,
    cleaned: bool,
    error_class: Option<&str>,
) -> Result<RemoteTaskRun, MemoryError> {
    if let Some(err) = error_class {
        validate_error_class(err)?;
    }
    with_composable_write(conn, |conn| {
        let local = resolve_current_local_admission(
            conn,
            &gateway.agent_identity_id,
            &gateway.connection_id,
        )?
        .ok_or_else(|| invalid("gateway is not the exact current local admission"))?;
        let env_row: Option<(String, String, i64)> = conn
            .query_row(
                "SELECT recipient_agent_identity_id,current_state,state_version
                 FROM a2a_envelopes WHERE envelope_id=?1 AND kind='tachi.remote_job/v1'",
                [envelope_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((recipient_identity, envelope_state, state_version)) = env_row else {
            return Err(MemoryError::NotFound(format!("remote task {envelope_id}")));
        };
        let run = read_run_row(conn, envelope_id)?
            .ok_or_else(|| invalid("run does not exist for remote task"))?;
        if recipient_identity != gateway.agent_identity_id
            || !matches!(
                run.lifecycle_state,
                RemoteRunLifecycleState::Completed
                    | RemoteRunLifecycleState::Failed
                    | RemoteRunLifecycleState::Canceled
            )
            || !matches!(
                envelope_state.as_str(),
                "remote_completed" | "remote_failed" | "remote_canceled"
            )
            || run.cleanup_state != RemoteCleanupStatus::Pending
        {
            return Err(invalid(
                "cleanup can only be recorded once on a terminal remote run",
            ));
        }
        let (now, _) = sqlite_now_ts(conn)?;
        let cleanup_state = if cleaned {
            RemoteCleanupStatus::Cleaned
        } else {
            RemoteCleanupStatus::Failed
        };
        let cleaned_at = if cleaned { Some(now.clone()) } else { None };
        let next_version = state_version + 1;
        conn.execute(
            "UPDATE a2a_remote_runs SET cleanup_state=?2,updated_at=?3
             WHERE envelope_id=?1 AND cleanup_state='pending'",
            params![envelope_id, cleanup_state.as_str(), now],
        )?;
        conn.execute(
            "UPDATE a2a_envelopes SET state_version=?2
             WHERE envelope_id=?1 AND state_version=?3",
            params![envelope_id, next_version, state_version],
        )?;
        let detail = RemoteReceiptDetail::Cleanup {
            local_run_id: run.local_run_id.clone(),
            cleanup_state,
            cleaned_at,
            error_class: error_class.map(str::to_string),
        };
        insert_receipt(
            conn,
            envelope_id,
            next_version,
            "remote_cleanup",
            gateway,
            &local.admission_id,
            &now,
            Some(&detail),
        )?;
        read_run_row(conn, envelope_id)?
            .ok_or_else(|| MemoryError::Internal("cleaned remote run disappeared".into()))
    })
}

/// Fail orphaned or lease-expired active destination runs deterministically.
fn headers_match_for_replay(existing: &RemoteTaskHeader, incoming: &RemoteTaskHeader) -> bool {
    existing.repository == incoming.repository
        && existing.job == incoming.job
        && existing.head_sha == incoming.head_sha
        && existing.origin_claim_id == incoming.origin_claim_id
        && existing.origin_claim_version == incoming.origin_claim_version
        && existing.destination_host_id == incoming.destination_host_id
        && (existing.pull_request == incoming.pull_request || existing.pull_request.is_none())
}

pub fn reconcile_active_remote_runs(
    conn: &Connection,
    gateway: &A2aTransitionActor,
) -> Result<Vec<String>, MemoryError> {
    with_composable_write(conn, |conn| {
        resolve_current_local_admission(conn, &gateway.agent_identity_id, &gateway.connection_id)?
            .ok_or_else(|| invalid("gateway is not the exact current local admission"))?;
        let (_, authoritative_now) = sqlite_now_ts(conn)?;
        let mut statement = conn.prepare(
            "SELECT r.envelope_id,r.local_claim_id,r.local_claim_version,e.expires_at,
                    EXISTS(SELECT 1 FROM a2a_delivery_receipts d WHERE d.envelope_id=r.envelope_id AND d.state='remote_started')
             FROM a2a_remote_runs r
             JOIN a2a_envelopes e ON e.envelope_id=r.envelope_id
             WHERE e.recipient_agent_identity_id=?1
               AND r.lifecycle_state IN ('admitted','started','cancel_requested')
             ORDER BY r.admitted_at,r.envelope_id",
        )?;
        let active = statement
            .query_map([&gateway.agent_identity_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut transitioned = Vec::new();
        for (envelope_id, claim_id, claim_version, envelope_expires_at, has_started) in active {
            let expired_before_start =
                !has_started && sqlite_parse_ts(&envelope_expires_at)? <= authoritative_now;
            let claim_bad = match get_claim(conn, &claim_id)? {
                None => true,
                Some(claim) => {
                    claim.state != ClaimState::Active
                        || claim.agent_identity_id.as_deref()
                            != Some(gateway.agent_identity_id.as_str())
                        || claim.transition_version != claim_version
                        || claim
                            .lease_expires_at
                            .as_deref()
                            .map(sqlite_parse_ts)
                            .transpose()?
                            .is_none_or(|exp| exp <= authoritative_now)
                }
            };
            if (expired_before_start || claim_bad)
                && matches!(
                    record_remote_task_terminal(
                        conn,
                        gateway,
                        &envelope_id,
                        RemoteTerminalOutcome::Failed,
                        None,
                        Some("lease_expired_or_orphaned"),
                        None,
                    )?,
                    RemoteTerminalTransition::Applied(_)
                )
            {
                transitioned.push(envelope_id);
            }
        }
        Ok(transitioned)
    })
}

/// Transactionally expire one active destination run if its start window or
/// bound local claim lease has elapsed, so an idle gateway's `GetTask` never
/// serves a permanently stale `admitted` snapshot.
pub fn reconcile_remote_run_if_expired(
    conn: &Connection,
    gateway: &A2aTransitionActor,
    envelope_id: &str,
) -> Result<bool, MemoryError> {
    with_composable_write(conn, |conn| {
        resolve_current_local_admission(conn, &gateway.agent_identity_id, &gateway.connection_id)?
            .ok_or_else(|| invalid("gateway is not the exact current local admission"))?;
        let (_, authoritative_now) = sqlite_now_ts(conn)?;
        let row: Option<(String, i64, String, bool)> = conn
            .query_row(
                "SELECT r.local_claim_id,r.local_claim_version,e.expires_at,
                        EXISTS(SELECT 1 FROM a2a_delivery_receipts d WHERE d.envelope_id=r.envelope_id AND d.state='remote_started')
                 FROM a2a_remote_runs r
                 JOIN a2a_envelopes e ON e.envelope_id=r.envelope_id
                 WHERE r.envelope_id=?1
                   AND e.recipient_agent_identity_id=?2
                   AND r.lifecycle_state IN ('admitted','started','cancel_requested')",
                params![envelope_id, &gateway.agent_identity_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((claim_id, claim_version, envelope_expires_at, has_started)) = row else {
            return Ok(false);
        };
        let expired_before_start =
            !has_started && sqlite_parse_ts(&envelope_expires_at)? <= authoritative_now;
        let claim_bad = match get_claim(conn, &claim_id)? {
            None => true,
            Some(claim) => {
                claim.state != ClaimState::Active
                    || claim.agent_identity_id.as_deref()
                        != Some(gateway.agent_identity_id.as_str())
                    || claim.transition_version != claim_version
                    || claim
                        .lease_expires_at
                        .as_deref()
                        .map(sqlite_parse_ts)
                        .transpose()?
                        .is_none_or(|exp| exp <= authoritative_now)
            }
        };
        if !(expired_before_start || claim_bad) {
            return Ok(false);
        }
        Ok(matches!(
            record_remote_task_terminal(
                conn,
                gateway,
                envelope_id,
                RemoteTerminalOutcome::Failed,
                None,
                Some("lease_expired_or_orphaned"),
                None,
            )?,
            RemoteTerminalTransition::Applied(_)
        ))
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
            hydrate_task(conn, task)?;
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
