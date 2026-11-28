//! Bounded origin submission state, using the existing admission and WorkClaim.
//! A reserved attempt is durable before I/O. It is not a destination receipt.

use super::{resolve_current_local_admission, A2aTransitionActor, MAX_REMOTE_JOB_BYTES};
use crate::db::common::{normalize_utc_iso, now_utc_iso, with_composable_write};
use crate::db::session_claims::{get_claim, work_claim_project, ClaimState};
use crate::error::MemoryError;
use crate::time_compat::millis_z;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteSubmissionBinding {
    pub issuer_id: String,
    pub canonical_project: String,
    pub destination: String,
    pub destination_host_id: String,
    pub destination_key_sha256: String,
    pub endpoint: String,
    pub repository: String,
    pub job: String,
    pub head_sha: String,
    pub origin_claim_id: String,
    pub origin_claim_version: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteSubmission {
    pub submission_id: String,
    pub origin: String,
    pub origin_admission_id: String,
    pub binding: RemoteSubmissionBinding,
    pub idempotency_key: String,
    pub nonce: String,
    pub trust_domain: String,
    #[serde(skip)]
    pub body: Option<String>,
    pub body_digest: String,
    pub issued_at: String,
    pub expires_at: String,
    pub current_state: String,
    pub state_version: i64,
    pub attempts: u32,
    pub attempt_until: Option<String>,
    pub last_error: Option<String>,
    pub retry_allowed: bool,
    pub remote_task_id: Option<String>,
    pub remote_receipt: Option<Value>,
    pub updated_at: String,
}

pub struct RemoteSubmissionAttempt {
    pub submission: RemoteSubmission,
    pub token: String,
    pub body: String,
}

pub enum RemoteSubmissionResult {
    Accepted {
        state: &'static str,
        task_id: String,
        receipt: Value,
    },
    Refused {
        task_id: String,
        receipt: Value,
    },
    Unknown {
        error_class: &'static str,
        retry_allowed: bool,
    },
}

fn invalid(message: &str) -> MemoryError {
    MemoryError::InvalidArg(format!("remote submission {message}"))
}

fn timestamp(value: &str) -> Result<jiff::Timestamp, MemoryError> {
    value
        .parse()
        .map_err(|_| invalid("has an invalid timestamp"))
}

fn admission(conn: &Connection, actor: &A2aTransitionActor) -> Result<String, MemoryError> {
    resolve_current_local_admission(conn, &actor.agent_identity_id, &actor.connection_id)?
        .map(|current| current.admission_id)
        .ok_or_else(|| invalid("requires a current local admission"))
}

fn authorize(
    conn: &Connection,
    actor: &A2aTransitionActor,
    binding: &RemoteSubmissionBinding,
    now: jiff::Timestamp,
) -> Result<(), MemoryError> {
    admission(conn, actor)?;
    let claim = get_claim(conn, &binding.origin_claim_id)?
        .ok_or_else(|| invalid("origin WorkClaim is unavailable"))?;
    if claim.agent_identity_id.as_deref() != Some(&actor.agent_identity_id)
        || claim.state != ClaimState::Active
        || claim.mode.is_none()
        || claim.transition_version != binding.origin_claim_version
        || claim.expected_head.as_deref() != Some(&binding.head_sha)
        || work_claim_project(conn, &claim.claim_id)?.as_deref() != Some(&binding.canonical_project)
        || claim
            .lease_expires_at
            .as_deref()
            .map(timestamp)
            .transpose()?
            .is_none_or(|end| end <= now)
        || crate::db::verified_mtls_key_is_revoked(
            conn,
            &binding.issuer_id,
            &format!("attestation:mtls-peer:{}", binding.destination_key_sha256),
        )?
    {
        return Err(invalid(
            "current origin authority does not match the frozen submission",
        ));
    }
    Ok(())
}

const COLUMNS: &str = "submission_id,origin_agent_identity_id,origin_admission_id,binding,idempotency_key,nonce,trust_domain,body,body_digest,issued_at,expires_at,current_state,state_version,attempts,attempt_until,last_error,retry_allowed,remote_task_id,remote_receipt,updated_at";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteSubmission> {
    let json = |index| -> rusqlite::Result<Value> {
        serde_json::from_str(&row.get::<_, String>(index)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    };
    let binding = serde_json::from_value(json(3)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(RemoteSubmission {
        submission_id: row.get(0)?,
        origin: row.get(1)?,
        origin_admission_id: row.get(2)?,
        binding,
        idempotency_key: row.get(4)?,
        nonce: row.get(5)?,
        trust_domain: row.get(6)?,
        body: row.get(7)?,
        body_digest: row.get(8)?,
        issued_at: row.get(9)?,
        expires_at: row.get(10)?,
        current_state: row.get(11)?,
        state_version: row.get(12)?,
        attempts: row.get(13)?,
        attempt_until: row.get(14)?,
        last_error: row.get(15)?,
        retry_allowed: row.get(16)?,
        remote_task_id: row.get(17)?,
        remote_receipt: if row.get::<_, Option<String>>(18)?.is_some() {
            Some(json(18)?)
        } else {
            None
        },
        updated_at: row.get(19)?,
    })
}

fn read(
    conn: &Connection,
    actor: &A2aTransitionActor,
    id: &str,
) -> Result<RemoteSubmission, MemoryError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM a2a_remote_submissions WHERE submission_id=?1 AND origin_agent_identity_id=?2"),
        params![id, actor.agent_identity_id], row,
    ).optional()?.ok_or_else(|| invalid("not found"))
}

pub fn read_remote_submission(
    conn: &Connection,
    actor: &A2aTransitionActor,
    id: &str,
) -> Result<RemoteSubmission, MemoryError> {
    with_composable_write(conn, |conn| {
        admission(conn, actor)?;
        read(conn, actor, id)
    })
}

/// The trusted wire boundary owns the closed job profile and current operator
/// allowlist. Storage rechecks every authority/replay field against its body.
pub fn prepare_remote_submission(
    conn: &Connection,
    actor: &A2aTransitionActor,
    binding: &RemoteSubmissionBinding,
    canonical_body: &str,
) -> Result<RemoteSubmission, MemoryError> {
    if canonical_body.is_empty() || canonical_body.len() > MAX_REMOTE_JOB_BYTES {
        return Err(invalid("body exceeds the bounded profile"));
    }
    let body: Value = serde_json::from_str(canonical_body)?;
    let text = |field: &str| {
        body[field]
            .as_str()
            .ok_or_else(|| invalid("missing immutable field"))
    };
    for (field, expected) in [
        ("kind", super::A2A_REMOTE_JOB_KIND),
        ("sender", actor.agent_identity_id.as_str()),
        ("recipient", binding.destination.as_str()),
        ("repository", binding.repository.as_str()),
        ("job", binding.job.as_str()),
        ("head_sha", binding.head_sha.as_str()),
    ] {
        if text(field)? != expected {
            return Err(invalid("body and authority binding differ"));
        }
    }
    if body["origin_claim"]["claim_id"].as_str() != Some(&binding.origin_claim_id)
        || body["origin_claim"]["transition_version"].as_i64() != Some(binding.origin_claim_version)
        || !matches!(binding.job.as_str(), "review_revision" | "verify_pr")
        || binding.canonical_project.is_empty()
    {
        return Err(invalid("body and origin WorkClaim differ"));
    }
    let key = text("idempotency_key")?;
    let nonce = text("nonce")?;
    let domain = text("trust_domain")?;
    let issued = timestamp(text("issued_at")?)?;
    let expires = timestamp(text("expires_at")?)?;
    // The row keeps the canonical millisecond UTC spelling of the origin's
    // window; the frozen body and its digest keep the operator's own bytes.
    let issued_at = normalize_utc_iso(text("issued_at")?)?;
    let expires_at = normalize_utc_iso(text("expires_at")?)?;
    let digest = format!("{:x}", Sha256::digest(canonical_body.as_bytes()));
    with_composable_write(conn, |conn| {
        let now = jiff::Timestamp::now();
        let admission_id = admission(conn, actor)?;
        let previous = conn.query_row(
            &format!("SELECT {COLUMNS} FROM a2a_remote_submissions WHERE origin_agent_identity_id=?1 AND idempotency_key=?2"),
            params![actor.agent_identity_id, key], row,
        ).optional()?;
        if let Some(previous) = previous {
            if previous.body_digest != digest
                || previous.binding != *binding
                || previous.trust_domain != domain
            {
                return Err(invalid(
                    "idempotency conflict; the original submission is immutable",
                ));
            }
            return Ok(previous);
        }
        authorize(conn, actor, binding, now)?;
        if issued > now
            || expires <= now
            || expires <= issued
            || expires - issued > jiff::SignedDuration::from_mins(15)
        {
            return Err(invalid("first submission is outside its start window"));
        }
        let id = format!("remote-submit-{}", uuid::Uuid::new_v4());
        conn.execute(
            "INSERT INTO a2a_remote_submissions
             (submission_id,origin_agent_identity_id,origin_admission_id,binding,idempotency_key,nonce,trust_domain,
              body,body_digest,issued_at,expires_at,current_state,state_version,attempts,retry_allowed,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',1,0,1,?12)",
            params![id, actor.agent_identity_id, admission_id, serde_json::to_string(binding)?, key, nonce, domain,
                canonical_body, digest, issued_at, expires_at, now_utc_iso()],
        )?;
        read(conn, actor, &id)
    })
}

fn stop(conn: &Connection, submission: &RemoteSubmission, reason: &str) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE a2a_remote_submissions SET current_state=?2,state_version=state_version+1,
         attempt_token=NULL,attempt_until=NULL,last_error=?3,retry_allowed=0,updated_at=?4 WHERE submission_id=?1",
        params![submission.submission_id, if submission.attempts == 0 {"blocked"} else {"unknown"},
            reason, now_utc_iso()],
    )?;
    Ok(())
}

/// No database lock crosses network I/O. The 45-second reservation exceeds
/// the health + POST deadlines (15 seconds each); crashed owners are fenced.
pub fn reserve_remote_submission(
    conn: &Connection,
    actor: &A2aTransitionActor,
    binding: &RemoteSubmissionBinding,
    trust_domain: &str,
    id: &str,
) -> Result<Option<RemoteSubmissionAttempt>, MemoryError> {
    with_composable_write(conn, |conn| {
        admission(conn, actor)?;
        let submission = read(conn, actor, id)?;
        let now = jiff::Timestamp::now();
        if !submission.retry_allowed
            || matches!(
                submission.current_state.as_str(),
                "refused"
                    | "blocked"
                    | "submitted"
                    | "working"
                    | "completed"
                    | "failed"
                    | "canceled"
            )
        {
            return Ok(None);
        }
        if submission
            .attempt_until
            .as_deref()
            .map(timestamp)
            .transpose()?
            .is_some_and(|until| until > now)
        {
            return Ok(None);
        }
        let authority = if submission.binding != *binding || submission.trust_domain != trust_domain
        {
            Err("operator_binding_changed")
        } else {
            match authorize(conn, actor, binding, now) {
                Ok(()) => Ok(()),
                // Only a real authority mismatch stops the submission. A
                // storage error is returned unchanged and leaves the row as
                // it was, so a later resume can reserve the same attempt.
                Err(MemoryError::InvalidArg(_)) => Err("origin_authority_changed"),
                Err(error) => return Err(error),
            }
        };
        let reason = match authority {
            Err(reason) => Some(reason),
            Ok(()) if timestamp(&submission.expires_at)? <= now => Some("start_window_expired"),
            Ok(()) if submission.attempts >= 3 => Some("attempts_exhausted"),
            Ok(()) => None,
        };
        if let Some(reason) = reason {
            stop(conn, &submission, reason)?;
            return Ok(None);
        }
        let body = submission
            .body
            .as_ref()
            .ok_or_else(|| invalid("frozen body is unavailable"))?;
        if format!("{:x}", Sha256::digest(body.as_bytes())) != submission.body_digest {
            return Err(invalid("frozen body digest mismatch"));
        }
        let token = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "UPDATE a2a_remote_submissions SET current_state='in_flight',state_version=state_version+1,
             attempts=attempts+1,attempt_token=?2,attempt_until=?3,last_error=NULL,updated_at=?4 WHERE submission_id=?1",
            params![id, token, millis_z(now + jiff::SignedDuration::from_secs(45)), now_utc_iso()],
        )?;
        Ok(Some(RemoteSubmissionAttempt {
            submission: read(conn, actor, id)?,
            token,
            body: body.clone(),
        }))
    })
}

/// Recheck the claim and key after TLS admission, immediately before POST.
pub fn remote_submission_attempt_is_current(
    conn: &Connection,
    actor: &A2aTransitionActor,
    attempt: &RemoteSubmissionAttempt,
) -> Result<bool, MemoryError> {
    with_composable_write(conn, |conn| {
        let now = jiff::Timestamp::now();
        authorize(conn, actor, &attempt.submission.binding, now)?;
        if timestamp(&attempt.submission.expires_at)? <= now {
            return Ok(false);
        }
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM a2a_remote_submissions WHERE submission_id=?1 AND
             origin_agent_identity_id=?2 AND attempt_token=?3 AND current_state='in_flight'
             AND unixepoch(attempt_until)>unixepoch(?4))",
            params![
                attempt.submission.submission_id,
                actor.agent_identity_id,
                attempt.token,
                millis_z(now)
            ],
            |row| row.get(0),
        )?)
    })
}

pub fn finish_remote_submission(
    conn: &Connection,
    actor: &A2aTransitionActor,
    attempt: &RemoteSubmissionAttempt,
    result: &RemoteSubmissionResult,
) -> Result<RemoteSubmission, MemoryError> {
    with_composable_write(conn, |conn| {
        admission(conn, actor)?;
        let (state, error, retry, task_id, receipt) = match result {
            RemoteSubmissionResult::Accepted {
                state,
                task_id,
                receipt,
            } => {
                if !matches!(
                    *state,
                    "submitted" | "working" | "completed" | "failed" | "canceled"
                ) || task_id.trim().is_empty()
                {
                    return Err(invalid("accepted state or task id is invalid"));
                }
                (
                    *state,
                    None,
                    false,
                    Some(task_id.as_str()),
                    Some(serde_json::to_string(receipt)?),
                )
            }
            RemoteSubmissionResult::Refused { task_id, receipt } => (
                "refused",
                None,
                false,
                Some(task_id.as_str()),
                Some(serde_json::to_string(receipt)?),
            ),
            RemoteSubmissionResult::Unknown {
                error_class,
                retry_allowed,
            } => {
                if !error_class
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_')
                    || error_class.len() > 64
                {
                    return Err(invalid("error class is not a bounded token"));
                }
                (
                    "unknown",
                    Some(*error_class),
                    *retry_allowed && attempt.submission.attempts < 3,
                    None,
                    None,
                )
            }
        };
        let changed = conn.execute(
            "UPDATE a2a_remote_submissions SET current_state=?4,state_version=state_version+1,
             attempt_token=NULL,attempt_until=NULL,last_error=?5,retry_allowed=?6,remote_task_id=?7,
             remote_receipt=?8,updated_at=?9 WHERE submission_id=?1 AND origin_agent_identity_id=?2
             AND attempt_token=?3 AND current_state='in_flight'",
            params![
                attempt.submission.submission_id,
                actor.agent_identity_id,
                attempt.token,
                state,
                error,
                retry,
                task_id,
                receipt,
                now_utc_iso()
            ],
        )?;
        if changed != 1 {
            return Err(invalid(
                "attempt was superseded; late result was not applied",
            ));
        }
        read(conn, actor, &attempt.submission.submission_id)
    })
}

fn submission_state_rank(state: &str) -> Result<u8, MemoryError> {
    match state {
        "unknown" => Ok(0),
        "submitted" => Ok(1),
        "working" => Ok(2),
        "completed" | "failed" | "canceled" | "refused" => Ok(3),
        _ => Err(invalid("cannot reconcile into non-remote state")),
    }
}

fn receipt_revision(receipt: &Value) -> Result<i64, MemoryError> {
    receipt
        .pointer("/metadata/urn:tachi:a2a:ext:provenance-receipt:v1/revision")
        .and_then(Value::as_i64)
        .filter(|rev| *rev >= 2)
        .ok_or_else(|| invalid("reconciled receipt is missing a valid revision"))
}

fn verify_append_only_provenance(
    task_id: &str,
    old_receipt: &Value,
    new_receipt: &Value,
) -> Result<(), MemoryError> {
    if new_receipt.get("id").and_then(Value::as_str) != Some(task_id) {
        return Err(invalid("reconciled receipt task id mismatch"));
    }
    fn provenance(receipt: &Value) -> Result<&serde_json::Map<String, Value>, MemoryError> {
        receipt
            .pointer("/metadata/urn:tachi:a2a:ext:provenance-receipt:v1")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("reconciled receipt is missing provenance metadata"))
    }
    let old_prov = provenance(old_receipt)?;
    let new_prov = provenance(new_receipt)?;
    for key in [
        "schema_version",
        "task_id",
        "input_digest",
        "origin",
        "origin_admission_ref",
        "destination",
        "destination_host_id",
        "trust_domain",
        "repository",
        "head_sha",
        "job",
        "origin_claim",
        "local_claim",
        "local_run_id",
    ] {
        let Some(old_val) = old_prov.get(key) else {
            return Err(invalid("existing receipt provenance is incomplete"));
        };
        if new_prov.get(key) != Some(old_val) {
            return Err(invalid(
                "cannot mutate immutable reconciled provenance field",
            ));
        }
    }
    let old_receipts = old_prov
        .get("receipts")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("existing receipt list is missing"))?;
    let new_receipts = new_prov
        .get("receipts")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("reconciled receipt list is missing"))?;
    if new_receipts.len() < old_receipts.len()
        || new_receipts[..old_receipts.len()] != old_receipts[..]
    {
        return Err(invalid(
            "cannot rewrite or drop previously observed receipts",
        ));
    }
    Ok(())
}

/// Persist an authenticated `GetTask` snapshot onto an existing submission
/// without allocating a second SendMessage attempt.
pub fn reconcile_remote_submission(
    conn: &Connection,
    actor: &A2aTransitionActor,
    id: &str,
    new_state: &str,
    task_id: &str,
    receipt: &Value,
) -> Result<RemoteSubmission, MemoryError> {
    if task_id.trim().is_empty() {
        return Err(invalid("reconciled task id must be non-empty"));
    }
    let new_rank = submission_state_rank(new_state)?;
    if new_rank == 0 {
        return Err(invalid("reconciled state cannot be unknown"));
    }
    let new_rev = receipt_revision(receipt)?;
    with_composable_write(conn, |conn| {
        admission(conn, actor)?;
        let current = read(conn, actor, id)?;
        if current.attempts == 0
            || matches!(
                current.current_state.as_str(),
                "pending" | "blocked" | "in_flight"
            )
        {
            return Err(invalid(
                "cannot reconcile a submission that is pending, blocked, or in flight",
            ));
        }
        if let Some(existing_task) = current.remote_task_id.as_deref() {
            if existing_task != task_id {
                return Err(invalid("reconciled task id does not match submission"));
            }
        }
        let old_rank = submission_state_rank(&current.current_state)?;
        if let Some(old_receipt) = current.remote_receipt.as_ref() {
            let old_rev = receipt_revision(old_receipt)?;
            if new_rev < old_rev {
                return Err(invalid("cannot regress reconciled receipt revision"));
            }
            if new_rev == old_rev {
                if current.current_state == new_state && old_receipt == receipt {
                    return Ok(current);
                }
                return Err(invalid("conflicting reconciled receipt at same revision"));
            }
            verify_append_only_provenance(task_id, old_receipt, receipt)?;
        }
        if new_rank < old_rank {
            return Err(invalid("cannot regress reconciled remote submission state"));
        }
        if old_rank == 3 && current.current_state != new_state {
            return Err(invalid("cannot overwrite terminal submission state"));
        }
        let encoded_receipt = serde_json::to_string(receipt)?;
        let changed = conn.execute(
            "UPDATE a2a_remote_submissions SET current_state=?3,state_version=state_version+1,
             attempt_token=NULL,attempt_until=NULL,last_error=NULL,retry_allowed=0,
             remote_task_id=?4,remote_receipt=?5,updated_at=?6
             WHERE submission_id=?1 AND origin_agent_identity_id=?2 AND state_version=?7",
            params![
                id,
                actor.agent_identity_id,
                new_state,
                task_id,
                encoded_receipt,
                now_utc_iso(),
                current.state_version
            ],
        )?;
        if changed != 1 {
            return Err(invalid("submission changed during reconciliation"));
        }
        read(conn, actor, id)
    })
}

#[cfg(test)]
mod tests;
