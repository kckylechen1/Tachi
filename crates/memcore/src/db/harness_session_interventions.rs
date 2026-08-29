//! Typed intervention request/result receipts for attached sessions (#1678).
//!
//! Tachi never sends an ACP lifecycle request and never holds a session
//! handle. A request recorded here is a durable, replay-idempotent receipt
//! of a typed ask that the HOST may fulfill; the host reports the
//! authoritative outcome through
//! [`record_harness_session_intervention_result`]. A request is never the
//! resulting lifecycle state: canonical session state moves only through the
//! event spine, and `cancelled` requires the harness's own confirmation
//! bound into a terminal fact.
//!
//! Gate order for a NEW request (all zero-mutation refusals):
//! 1. attachment resolves for the current host binding;
//! 2. canonical state admits the kind (nothing to pause on a terminal);
//! 3. the advertised lifecycle capability admits the kind — otherwise the
//!    typed [`MemoryError::UnsupportedByLifecycleOwner`] is returned and
//!    nothing is written (`unsupported` honesty: no fake `cancelled`);
//! 4. the admitted AgentIdentity policy admits the kind (observe profiles
//!    may only request status);
//! 5. the expected session revision matches the canonical high-water
//!    revision (stale revision is a typed conflict);
//! 6. `(attachment_id, request_id)` idempotency: an identical replay
//!    returns the original receipt; a different shape under the same id is
//!    a typed conflict.
//!
//! Replays of an already-issued receipt return `Replayed` without
//! re-running gates 2-5: an issued receipt is immutable history, and the
//! authoritative result still cannot exist without the host.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::db::harness_session_attachments::{
    HarnessSessionAttachment, HarnessSessionAttachmentCapabilities,
    HarnessSessionAttachmentSelector, HarnessSessionHostAdmission,
};
use crate::db::harness_session_events::{
    load_state_row_for_interventions, HarnessSessionCanonicalState, HarnessSessionStateProjection,
};
use crate::db::normalize_utc_iso_or_now;
use crate::error::MemoryError;

/// Closed intervention vocabulary. The mapping to lifecycle capabilities is
/// frozen: `request_pause` exercises the same host authority as
/// `request_cancel` (the right to ask active work to stop), because the
/// declared capability set has no separate `pause` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessSessionInterventionKind {
    RequestStatus,
    PromptOrCorrect,
    RequestPause,
    RequestCancel,
    RequestResume,
}

impl HarnessSessionInterventionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequestStatus => "request_status",
            Self::PromptOrCorrect => "prompt_or_correct",
            Self::RequestPause => "request_pause",
            Self::RequestCancel => "request_cancel",
            Self::RequestResume => "request_resume",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "request_status" => Ok(Self::RequestStatus),
            "prompt_or_correct" => Ok(Self::PromptOrCorrect),
            "request_pause" => Ok(Self::RequestPause),
            "request_cancel" => Ok(Self::RequestCancel),
            "request_resume" => Ok(Self::RequestResume),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown harness session intervention kind '{other}'"
            ))),
        }
    }

    /// The declared lifecycle capability this request exercises.
    pub fn capability(self) -> &'static str {
        match self {
            Self::RequestStatus => "observe",
            Self::PromptOrCorrect => "prompt",
            Self::RequestPause | Self::RequestCancel => "cancel",
            Self::RequestResume => "resume",
        }
    }

    /// Whether the attachment's admitted policy profile may issue this kind.
    /// Observe profiles are observation-only; delegation authority is
    /// required to ask a session to do (or stop doing) work.
    pub fn policy_admits(self, tool_profile: &str) -> bool {
        match tool_profile {
            "observe" => self == Self::RequestStatus,
            "delegate" => true,
            _ => false,
        }
    }

    fn requires_live_session(self) -> bool {
        !matches!(self, Self::RequestStatus)
    }

    /// Whether the kind is admitted while the session is `unknown_orphaned`:
    /// probing status and asserting cancel authority remain meaningful for a
    /// disappeared session; prompting, pausing, or resuming it is not.
    fn admitted_while_orphaned(self) -> bool {
        matches!(self, Self::RequestStatus | Self::RequestCancel)
    }
}

/// Input for a typed intervention request receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHarnessSessionIntervention {
    pub request_id: String,
    pub kind: HarnessSessionInterventionKind,
    /// Bounded public-safe reason (at most 1000 characters).
    pub reason: String,
    /// Compare-and-swap against the canonical session revision high-water.
    pub expected_session_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSessionIntervention {
    pub intervention_row_id: i64,
    pub attachment_id: String,
    pub request_id: String,
    pub kind: HarnessSessionInterventionKind,
    pub reason: String,
    pub expected_session_revision: i64,
    pub capability_source: CapabilitySource,
    /// The live host connection the request was issued through; derived
    /// server-side, never caller-claimed.
    pub requested_by: String,
    pub requested_at: String,
}

/// Closed dispositions the HOST may authoritatively report. `accepted` on a
/// `request_cancel` must carry the harness confirmation reference that the
/// eventual terminal `cancelled` fact will bind to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessSessionInterventionDisposition {
    Accepted,
    Refused,
    Unsupported,
    Failed,
}

impl HarnessSessionInterventionDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Refused => "refused",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }

    fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "accepted" => Ok(Self::Accepted),
            "refused" => Ok(Self::Refused),
            "unsupported" => Ok(Self::Unsupported),
            "failed" => Ok(Self::Failed),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown harness session intervention disposition '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSessionInterventionResult {
    pub result_row_id: i64,
    pub attachment_id: String,
    pub request_id: String,
    pub disposition: HarnessSessionInterventionDisposition,
    pub authority_confirmation_ref: Option<String>,
    pub detail: Option<String>,
    pub recorded_at: String,
    pub source_host_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessSessionInterventionAdmission {
    Created,
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionInterventionRequestReceipt {
    pub intervention: HarnessSessionIntervention,
    pub admission: HarnessSessionInterventionAdmission,
    /// The capability fact the gate consulted: the latest host advertisement
    /// when one exists, otherwise the attachment's declared set.
    pub capability_source: CapabilitySource,
    pub state: HarnessSessionStateProjection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilitySource {
    /// Latest host advertisement for this attachment.
    Advertised,
    /// No advertisement exists; the attachment's declared set decided.
    Declared,
}

impl CapabilitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advertised => "advertised",
            Self::Declared => "declared",
        }
    }

    fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "advertised" => Ok(Self::Advertised),
            "declared" => Ok(Self::Declared),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown harness session capability source '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionInterventionResultReceipt {
    pub result: HarnessSessionInterventionResult,
    /// Request metadata captured in the same transaction as the result. This
    /// keeps acknowledgements independent of a later attachment rebind.
    pub request_kind: HarnessSessionInterventionKind,
    pub admission: HarnessSessionInterventionAdmission,
    pub state: HarnessSessionStateProjection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHarnessSessionInterventionResult {
    pub request_id: String,
    pub disposition: HarnessSessionInterventionDisposition,
    pub authority_confirmation_ref: Option<String>,
    /// Bounded public-safe detail (at most 2000 characters).
    pub detail: Option<String>,
}

const INTERVENTION_COLUMNS: &str = "intervention_row_id, attachment_id, request_id, kind, reason, \
    expected_session_revision, capability_source, requested_by, requested_at";

fn row_to_intervention(
    row: &rusqlite::Row<'_>,
) -> Result<HarnessSessionIntervention, rusqlite::Error> {
    let kind_raw: String = row.get(3)?;
    let kind = HarnessSessionInterventionKind::parse(&kind_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })?;
    let capability_source_raw: String = row.get(6)?;
    let capability_source = CapabilitySource::parse(&capability_source_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })?;
    Ok(HarnessSessionIntervention {
        intervention_row_id: row.get(0)?,
        attachment_id: row.get(1)?,
        request_id: row.get(2)?,
        kind,
        reason: row.get(4)?,
        expected_session_revision: row.get(5)?,
        capability_source,
        requested_by: row.get(7)?,
        requested_at: row.get(8)?,
    })
}

fn row_to_result(
    row: &rusqlite::Row<'_>,
) -> Result<HarnessSessionInterventionResult, rusqlite::Error> {
    let disposition_raw: String = row.get(3)?;
    let disposition =
        HarnessSessionInterventionDisposition::parse(&disposition_raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    error.to_string(),
                )),
            )
        })?;
    Ok(HarnessSessionInterventionResult {
        result_row_id: row.get(0)?,
        attachment_id: row.get(1)?,
        request_id: row.get(2)?,
        disposition,
        authority_confirmation_ref: row.get(4)?,
        detail: row.get(5)?,
        recorded_at: row.get(6)?,
        source_host_identity: row.get(7)?,
    })
}

fn require_non_empty(value: &str, field: &str) -> Result<(), MemoryError> {
    if value.trim().is_empty() {
        return Err(MemoryError::InvalidArg(format!(
            "{field} must be non-empty"
        )));
    }
    Ok(())
}

/// Public-safe free text must be exactly what every reader sees: control
/// characters (including NUL, which SQLite text functions truncate at) are
/// refused rather than stored.
fn require_no_control(value: &str, field: &str) -> Result<(), MemoryError> {
    if value.chars().any(|c| c.is_control()) {
        return Err(MemoryError::InvalidArg(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

fn latest_advertised_capabilities(
    conn: &Connection,
    attachment_id: &str,
) -> Result<Option<String>, MemoryError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT capabilities_json FROM harness_session_capability_advertisements
             WHERE attachment_id = ?1 ORDER BY advertisement_seq DESC LIMIT 1",
            params![attachment_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let parsed: HarnessSessionAttachmentCapabilities =
        serde_json::from_str(&raw).map_err(|error| {
            MemoryError::WorkClaimIncompatibleState(format!(
                "capability advertisement is not the closed canonical shape: {error}"
            ))
        })?;
    if parsed.canonical_json()? != raw {
        return Err(MemoryError::WorkClaimIncompatibleState(
            "capability advertisement is not canonical JSON".to_string(),
        ));
    }
    Ok(Some(raw))
}

/// Record a host-owned capability advertisement for the attachment. The
/// latest advertisement becomes the capability gate's source of truth for
/// later intervention requests; the closed capability vocabulary is enforced
/// and the stored JSON must be the canonical shape.
pub fn advertise_harness_session_capabilities(
    conn: &mut Connection,
    selector: &HarnessSessionAttachmentSelector,
    capabilities: &HarnessSessionAttachmentCapabilities,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<(i64, String), MemoryError> {
    let capabilities_json = capabilities.canonical_json()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attachment = crate::db::harness_session_events::require_attachment_for_interventions(
        &tx,
        selector,
        host,
        admission_receipt_ref,
    )?;
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(advertisement_seq), 0) FROM harness_session_capability_advertisements
         WHERE attachment_id = ?1",
        params![attachment.attachment_id],
        |row| row.get::<_, i64>(0),
    )? + 1;
    let now = normalize_utc_iso_or_now("");
    tx.execute(
        "INSERT INTO harness_session_capability_advertisements (
            attachment_id, advertisement_seq, capabilities_json,
            source_host_identity, advertised_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            attachment.attachment_id,
            seq,
            capabilities_json,
            attachment.host_identity,
            now,
        ],
    )?;
    tx.commit()?;
    Ok((seq, capabilities_json))
}

/// Record a typed intervention request receipt. Zero mutation on any
/// refusal, typed `unsupported_by_lifecycle_owner` when the advertised
/// capability or admitted policy does not cover the kind.
pub fn request_harness_session_intervention(
    conn: &mut Connection,
    selector: &HarnessSessionAttachmentSelector,
    input: &NewHarnessSessionIntervention,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<HarnessSessionInterventionRequestReceipt, MemoryError> {
    require_non_empty(&input.request_id, "request_id")?;
    require_no_control(&input.request_id, "request_id")?;
    if input.request_id.chars().count() > 128 {
        return Err(MemoryError::InvalidArg(
            "request_id must be at most 128 characters".to_string(),
        ));
    }
    require_non_empty(&input.reason, "reason")?;
    require_no_control(&input.reason, "reason")?;
    if input.reason.chars().count() > 1000 {
        return Err(MemoryError::InvalidArg(
            "reason must be at most 1000 characters".to_string(),
        ));
    }
    if input.expected_session_revision < 0 {
        return Err(MemoryError::InvalidArg(
            "expected_session_revision must be non-negative".to_string(),
        ));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attachment = crate::db::harness_session_events::require_attachment_for_interventions(
        &tx,
        selector,
        host,
        admission_receipt_ref,
    )?;
    let attachment_id = attachment.attachment_id.clone();

    // Idempotent replay of an issued receipt: immutable history, no gate
    // re-run, no new row.
    let existing: Option<HarnessSessionIntervention> = tx
        .query_row(
            &format!(
                "SELECT {INTERVENTION_COLUMNS} FROM harness_session_interventions
                 WHERE attachment_id = ?1 AND request_id = ?2"
            ),
            params![attachment_id, input.request_id],
            row_to_intervention,
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing.kind != input.kind
            || existing.reason != input.reason
            || existing.expected_session_revision != input.expected_session_revision
        {
            return Err(MemoryError::WorkClaimConflict(format!(
                "intervention request {} replays with different content",
                input.request_id
            )));
        }
        let state = projection_from_row(&tx, &attachment_id);
        let capability_source = existing.capability_source;
        tx.commit()?;
        return Ok(HarnessSessionInterventionRequestReceipt {
            intervention: existing,
            admission: HarnessSessionInterventionAdmission::Replayed,
            capability_source,
            state,
        });
    }

    let state_row = load_state_row_for_interventions(&tx, &attachment_id)?;
    let canonical = state_row.as_ref().and_then(|row| row.canonical_state);
    let canonical_revision = state_row
        .as_ref()
        .map(|row| row.canonical_revision)
        .unwrap_or(0);

    // Canonical state gate: nothing to pause/prompt/resume on a terminal or
    // reconciling session; a disappeared session only admits status probes
    // and cancel authority.
    if input.kind.requires_live_session() {
        let admissible = match canonical {
            // No disappearance receipt exists: the session is attached and
            // merely has not reported a fact yet.
            None => true,
            Some(HarnessSessionCanonicalState::UnknownOrphaned) => {
                input.kind.admitted_while_orphaned()
            }
            Some(state) => !state.is_terminal(),
        };
        if !admissible {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "harness session is {:?}; '{}' cannot be requested",
                canonical,
                input.kind.as_str()
            )));
        }
    }

    // Capability gate on the latest advertisement, falling back to the
    // declared attachment set.
    let (capabilities_json, capability_source) =
        match latest_advertised_capabilities(&tx, &attachment_id)? {
            Some(json) => (json, CapabilitySource::Advertised),
            None => (
                attachment.capabilities_json.clone(),
                CapabilitySource::Declared,
            ),
        };
    let advertised: HarnessSessionAttachmentCapabilities = serde_json::from_str(&capabilities_json)
        .map_err(|error| {
            MemoryError::WorkClaimIncompatibleState(format!(
                "capability advertisement is not canonical: {error}"
            ))
        })?;
    if !capability_enabled(&advertised, input.kind.capability()) {
        return Err(MemoryError::UnsupportedByLifecycleOwner {
            attachment_id,
            intervention_kind: input.kind.as_str().to_string(),
        });
    }

    // Policy gate from the admitted AgentIdentity grant.
    if !input.kind.policy_admits(&attachment.tool_profile) {
        return Err(MemoryError::UnsupportedByLifecycleOwner {
            attachment_id,
            intervention_kind: input.kind.as_str().to_string(),
        });
    }

    // Revision compare-and-swap against the canonical high-water.
    if input.expected_session_revision != canonical_revision {
        return Err(MemoryError::WorkClaimConflict(format!(
            "harness session revision is {canonical_revision}, expected {}",
            input.expected_session_revision
        )));
    }

    let now = normalize_utc_iso_or_now("");
    tx.execute(
        "INSERT INTO harness_session_interventions (
            attachment_id, request_id, kind, reason, expected_session_revision,
            capability_source, requested_by, requested_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            attachment_id,
            input.request_id,
            input.kind.as_str(),
            input.reason,
            input.expected_session_revision,
            capability_source.as_str(),
            host.host_identity,
            now,
        ],
    )?;
    let intervention_row_id = tx.last_insert_rowid();
    let projection = projection_from_row(&tx, &attachment_id);
    tx.commit()?;
    Ok(HarnessSessionInterventionRequestReceipt {
        intervention: HarnessSessionIntervention {
            intervention_row_id,
            attachment_id,
            request_id: input.request_id.clone(),
            kind: input.kind,
            reason: input.reason.clone(),
            expected_session_revision: input.expected_session_revision,
            capability_source,
            requested_by: host.host_identity.clone(),
            requested_at: now,
        },
        admission: HarnessSessionInterventionAdmission::Created,
        capability_source,
        state: projection,
    })
}

fn capability_enabled(capabilities: &HarnessSessionAttachmentCapabilities, name: &str) -> bool {
    match name {
        "observe" => capabilities.observe,
        "wait" => capabilities.wait,
        "prompt" => capabilities.prompt,
        "cancel" => capabilities.cancel,
        "resume" => capabilities.resume,
        "load" => capabilities.load,
        "events" => capabilities.events,
        "artifacts" => capabilities.artifacts,
        _ => false,
    }
}

fn projection_from_row(conn: &Connection, attachment_id: &str) -> HarnessSessionStateProjection {
    let row = load_state_row_for_interventions(conn, attachment_id)
        .expect("canonical state row parses: written only through the typed spine");
    match row {
        Some(row) => HarnessSessionStateProjection {
            attachment_id: attachment_id.to_string(),
            canonical_state: row.canonical_state,
            canonical_revision: row.canonical_revision,
            cleanup_recorded: row.cleanup_recorded,
            conflicting_terminal: row.conflicting_terminal_digest.is_some(),
            last_event_id: row.last_event_id,
            updated_at: row.updated_at,
        },
        None => HarnessSessionStateProjection {
            attachment_id: attachment_id.to_string(),
            canonical_state: None,
            canonical_revision: 0,
            cleanup_recorded: false,
            conflicting_terminal: false,
            last_event_id: None,
            updated_at: None,
        },
    }
}

/// Record the host's authoritative result for an issued intervention
/// request. The result never mutates canonical session state; a `cancelled`
/// lifecycle fact still has to arrive through the event spine bound to the
/// confirmation reference recorded here. Idempotent per
/// `(attachment_id, request_id)`.
pub fn record_harness_session_intervention_result(
    conn: &mut Connection,
    selector: &HarnessSessionAttachmentSelector,
    input: &NewHarnessSessionInterventionResult,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<HarnessSessionInterventionResultReceipt, MemoryError> {
    require_non_empty(&input.request_id, "request_id")?;
    if let Some(confirmation) = &input.authority_confirmation_ref {
        require_non_empty(confirmation, "authority_confirmation_ref")?;
        require_no_control(confirmation, "authority_confirmation_ref")?;
        if confirmation.chars().count() > 128 {
            return Err(MemoryError::InvalidArg(
                "authority_confirmation_ref must be at most 128 characters".to_string(),
            ));
        }
    }
    if let Some(detail) = &input.detail {
        if detail.is_empty() || detail.chars().count() > 2000 {
            return Err(MemoryError::InvalidArg(
                "detail must be non-empty and at most 2000 characters".to_string(),
            ));
        }
        require_no_control(detail, "detail")?;
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attachment = crate::db::harness_session_events::require_attachment_for_interventions(
        &tx,
        selector,
        host,
        admission_receipt_ref,
    )?;
    let attachment_id = attachment.attachment_id.clone();

    let request: HarnessSessionIntervention = tx
        .query_row(
            &format!(
                "SELECT {INTERVENTION_COLUMNS} FROM harness_session_interventions
                 WHERE attachment_id = ?1 AND request_id = ?2"
            ),
            params![attachment_id, input.request_id],
            row_to_intervention,
        )
        .optional()?
        .ok_or_else(|| {
            MemoryError::NotFound(format!(
                "intervention request {} was not issued; refusing to record a result for an unissued request",
                input.request_id
            ))
        })?;

    // The hard line: an accepted cancel must carry the harness's own
    // confirmation reference, or nothing is recorded.
    if request.kind == HarnessSessionInterventionKind::RequestCancel
        && input.disposition == HarnessSessionInterventionDisposition::Accepted
        && input
            .authority_confirmation_ref
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err(MemoryError::WorkClaimIncompatibleState(
            "accepted request_cancel result requires the authoritative harness confirmation reference; refusing to fake a cancellation"
                .to_string(),
        ));
    }

    let existing: Option<HarnessSessionInterventionResult> = tx
        .query_row(
            "SELECT result_row_id, attachment_id, request_id, disposition,
                    authority_confirmation_ref, detail, recorded_at, source_host_identity
             FROM harness_session_intervention_results
             WHERE attachment_id = ?1 AND request_id = ?2",
            params![attachment_id, input.request_id],
            row_to_result,
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing.disposition != input.disposition
            || existing.authority_confirmation_ref != input.authority_confirmation_ref
            || existing.detail != input.detail
        {
            return Err(MemoryError::WorkClaimConflict(format!(
                "intervention result for request {} replays with different content",
                input.request_id
            )));
        }
        let projection = projection_from_row(&tx, &attachment_id);
        tx.commit()?;
        return Ok(HarnessSessionInterventionResultReceipt {
            result: existing,
            request_kind: request.kind,
            admission: HarnessSessionInterventionAdmission::Replayed,
            state: projection,
        });
    }

    let now = normalize_utc_iso_or_now("");
    tx.execute(
        "INSERT INTO harness_session_intervention_results (
            attachment_id, request_id, disposition, authority_confirmation_ref,
            detail, recorded_at, source_host_identity
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            attachment_id,
            input.request_id,
            input.disposition.as_str(),
            input.authority_confirmation_ref,
            input.detail,
            now,
            attachment.host_identity,
        ],
    )?;
    let result_row_id = tx.last_insert_rowid();
    let projection = projection_from_row(&tx, &attachment_id);
    tx.commit()?;
    Ok(HarnessSessionInterventionResultReceipt {
        result: HarnessSessionInterventionResult {
            result_row_id,
            attachment_id,
            request_id: input.request_id.clone(),
            disposition: input.disposition,
            authority_confirmation_ref: input.authority_confirmation_ref.clone(),
            detail: input.detail.clone(),
            recorded_at: now,
            source_host_identity: attachment.host_identity,
        },
        request_kind: request.kind,
        admission: HarnessSessionInterventionAdmission::Created,
        state: projection,
    })
}

/// Read one intervention request with its result, if any. Observation only.
pub fn get_harness_session_intervention(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    request_id: &str,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<
    Option<(
        HarnessSessionIntervention,
        Option<HarnessSessionInterventionResult>,
    )>,
    MemoryError,
> {
    let attachment: Option<HarnessSessionAttachment> =
        crate::db::harness_session_events::resolve_attachment_for_interventions(
            conn,
            selector,
            host,
            admission_receipt_ref,
        )?;
    let Some(attachment) = attachment else {
        return Ok(None);
    };
    let request: Option<HarnessSessionIntervention> = conn
        .query_row(
            &format!(
                "SELECT {INTERVENTION_COLUMNS} FROM harness_session_interventions
                 WHERE attachment_id = ?1 AND request_id = ?2"
            ),
            params![attachment.attachment_id, request_id],
            row_to_intervention,
        )
        .optional()?;
    let Some(request) = request else {
        return Ok(None);
    };
    let result: Option<HarnessSessionInterventionResult> = conn
        .query_row(
            "SELECT result_row_id, attachment_id, request_id, disposition,
                    authority_confirmation_ref, detail, recorded_at, source_host_identity
             FROM harness_session_intervention_results
             WHERE attachment_id = ?1 AND request_id = ?2",
            params![attachment.attachment_id, request_id],
            row_to_result,
        )
        .optional()?;
    Ok(Some((request, result)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::harness_session_attachments::{
        attach_harness_session, authorization_digest_for_test, test_attachment_input,
        HarnessSessionAttachmentAdmission, HarnessSessionAttachmentSelector,
    };
    use crate::db::harness_session_events::{
        ingest_harness_session_event, mark_harness_session_connection,
        HarnessSessionConnectionFact, HarnessSessionEventKind, HarnessSessionTerminalOutcome,
        NewHarnessSessionEvent,
    };
    use crate::db::schema::init_schema;
    use crate::db::session_claims::{
        insert_agent_identity, insert_work_claim, AgentIdentity, NewWorkClaim, WorkClaimMode,
    };
    use rusqlite::Connection;

    const GRANT_DELEGATE: &str =
        r#"{"acp":{"tool_profiles":["delegate"],"capability_classes":["tachi"]}}"#;
    const GRANT_OBSERVE: &str =
        r#"{"acp":{"tool_profiles":["observe"],"capability_classes":["tachi"]}}"#;

    fn host() -> HarnessSessionHostAdmission {
        HarnessSessionHostAdmission {
            host_identity: "host-1".into(),
            connection_id: "connection-1".into(),
        }
    }

    /// Seed host identity, worker identity with `grant`, admission, claim,
    /// and the attachment. The grant decides both capability advertisement
    /// defaults and the policy profile.
    fn seeded_with_grant(
        grant: &str,
        idempotency_key: &str,
    ) -> (Connection, HarnessSessionAttachmentSelector) {
        crate::db::enable_simple_auto_extension().unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        insert_agent_identity(
            &conn,
            &AgentIdentity {
                agent_identity_id: "host-1".into(),
                display_name: None,
                seat: None,
                capability_json: None,
                created_at: String::new(),
            },
        )
        .unwrap();
        insert_agent_identity(
            &conn,
            &AgentIdentity {
                agent_identity_id: "agent-1".into(),
                display_name: None,
                seat: None,
                capability_json: Some(grant.into()),
                created_at: String::new(),
            },
        )
        .unwrap();
        conn.execute(
            "INSERT INTO identity_admissions
             (admission_id, agent_identity_id, connection_id, state, created_at)
             VALUES ('admission-1', 'host-1', 'connection-1', 'self_asserted', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        insert_work_claim(
            &mut conn,
            &NewWorkClaim {
                claim_id: "claim-1".into(),
                agent_identity_id: "agent-1".into(),
                session_client: Some("connection-1".into()),
                issue_ref: None,
                flow_id: None,
                dispatch_id: None,
                branch: "branch".into(),
                worktree_path: "/tmp/intervention-claim".into(),
                declared_file_scope: "[\"src/lib.rs\"]".into(),
                role: "executor".into(),
                mode: WorkClaimMode::Writable,
                expected_head: "head".into(),
                lease_expires_at: "2030-01-01T00:00:00Z".into(),
                created_at: String::new(),
            },
        )
        .unwrap();
        let mut input = test_attachment_input(idempotency_key);
        if grant == GRANT_OBSERVE {
            input.tool_profile = "observe".into();
            input.policy_digest = authorization_digest_for_test(grant, "observe", "tachi");
        } else {
            input.policy_digest = authorization_digest_for_test(grant, "delegate", "tachi");
        }
        let receipt = attach_harness_session(&mut conn, &input, &host(), 30 * 60).unwrap();
        assert_eq!(
            receipt.admission,
            HarnessSessionAttachmentAdmission::Created
        );
        let selector = HarnessSessionAttachmentSelector::AttachmentId(
            receipt.attachment.attachment_id.clone(),
        );
        (conn, selector)
    }

    fn request(
        request_id: &str,
        kind: HarnessSessionInterventionKind,
        revision: i64,
    ) -> NewHarnessSessionIntervention {
        NewHarnessSessionIntervention {
            request_id: request_id.into(),
            kind,
            reason: "operator asked for a bounded intervention".into(),
            expected_session_revision: revision,
        }
    }

    fn intervention_rows(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM harness_session_interventions",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn fact(
        event_id: &str,
        kind: HarnessSessionEventKind,
        revision: i64,
    ) -> NewHarnessSessionEvent {
        NewHarnessSessionEvent {
            event_id: event_id.into(),
            kind,
            outcome: None,
            source_revision: revision,
            authority_confirmation_ref: None,
            summary: Some("public-safe".into()),
            payload_digest: None,
            occurred_at: "2026-08-29T00:00:00Z".into(),
        }
    }

    #[test]
    fn cancel_capable_attachment_accepts_request_and_replays_idempotently() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "cancel-attach");
        let first = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request("req-1", HarnessSessionInterventionKind::RequestCancel, 0),
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            first.admission,
            HarnessSessionInterventionAdmission::Created
        );
        assert_eq!(first.capability_source, CapabilitySource::Declared);
        assert_eq!(first.intervention.requested_by, "host-1");
        assert_eq!(intervention_rows(&conn), 1);

        let replay = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request("req-1", HarnessSessionInterventionKind::RequestCancel, 0),
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            replay.admission,
            HarnessSessionInterventionAdmission::Replayed
        );
        assert_eq!(intervention_rows(&conn), 1, "replay must not add a row");

        let mut conflicting = request("req-1", HarnessSessionInterventionKind::RequestResume, 0);
        conflicting.reason = "different ask under the same id".into();
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &conflicting,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            matches!(error, MemoryError::WorkClaimConflict(_)),
            "{error}"
        );
        assert_eq!(intervention_rows(&conn), 1);
    }

    #[test]
    fn unsupported_cancel_is_typed_refusal_with_zero_state_mutation() {
        let (mut conn, selector) = seeded_with_grant(GRANT_OBSERVE, "observe-cancel-attach");
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-cancel",
                HarnessSessionInterventionKind::RequestCancel,
                0,
            ),
            &host(),
            "admission-1",
        )
        .unwrap_err();
        match error {
            MemoryError::UnsupportedByLifecycleOwner {
                attachment_id,
                intervention_kind,
            } => {
                assert_eq!(intervention_kind, "request_cancel");
                assert!(!attachment_id.is_empty());
            }
            other => panic!("expected UnsupportedByLifecycleOwner, got {other}"),
        }
        assert_eq!(
            intervention_rows(&conn),
            0,
            "unsupported refusal must be zero mutation"
        );
        // The observe policy also denies prompting even though the capability
        // set could advertise it.
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-prompt",
                HarnessSessionInterventionKind::PromptOrCorrect,
                0,
            ),
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(matches!(
            error,
            MemoryError::UnsupportedByLifecycleOwner { .. }
        ));
        assert_eq!(intervention_rows(&conn), 0);
        // Status probing stays admitted for observe-only profiles.
        let status = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-status",
                HarnessSessionInterventionKind::RequestStatus,
                0,
            ),
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            status.admission,
            HarnessSessionInterventionAdmission::Created
        );
        assert_eq!(intervention_rows(&conn), 1);
    }

    #[test]
    fn reason_and_detail_reject_control_characters_with_zero_mutation() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "nul-attach");
        let mut nul_reason = request("req-nul", HarnessSessionInterventionKind::RequestCancel, 0);
        nul_reason.reason = "visible\u{0000}smuggled tail".to_string();
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &nul_reason,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");
        assert_eq!(intervention_rows(&conn), 0);

        request_harness_session_intervention(
            &mut conn,
            &selector,
            &request("req-ok", HarnessSessionInterventionKind::RequestCancel, 0),
            &host(),
            "admission-1",
        )
        .unwrap();

        let mut newline_detail = NewHarnessSessionInterventionResult {
            request_id: "req-ok".into(),
            disposition: HarnessSessionInterventionDisposition::Accepted,
            authority_confirmation_ref: Some("conf-1".into()),
            detail: Some("line one\nline two".into()),
        };
        newline_detail.disposition = HarnessSessionInterventionDisposition::Accepted;
        let error = record_harness_session_intervention_result(
            &mut conn,
            &selector,
            &newline_detail,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");
        let results: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM harness_session_intervention_results",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(results, 0, "refusals never journal");
    }

    #[test]
    fn confirmation_reference_rejects_control_and_oversize_text() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "conf-attach");
        request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-cancel",
                HarnessSessionInterventionKind::RequestCancel,
                0,
            ),
            &host(),
            "admission-1",
        )
        .unwrap();

        // NUL in the confirmation reference is refused, never stored.
        let error = record_harness_session_intervention_result(
            &mut conn,
            &selector,
            &NewHarnessSessionInterventionResult {
                request_id: "req-cancel".into(),
                disposition: HarnessSessionInterventionDisposition::Accepted,
                authority_confirmation_ref: Some("conf\u{0000}hidden".into()),
                detail: None,
            },
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");

        // Oversize references are refused too.
        let error = record_harness_session_intervention_result(
            &mut conn,
            &selector,
            &NewHarnessSessionInterventionResult {
                request_id: "req-cancel".into(),
                disposition: HarnessSessionInterventionDisposition::Accepted,
                authority_confirmation_ref: Some("x".repeat(129)),
                detail: None,
            },
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("at most 128 characters"),
            "{error}"
        );

        let results: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM harness_session_intervention_results",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(results, 0, "refusals never journal");
    }

    #[test]
    fn request_ids_are_bounded_and_control_free() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "reqid-attach");
        let nul_request = request(
            "req\u{0000}hidden",
            HarnessSessionInterventionKind::RequestCancel,
            0,
        );
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &nul_request,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");

        let oversize_request = request(
            &"x".repeat(129),
            HarnessSessionInterventionKind::RequestCancel,
            0,
        );
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &oversize_request,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("at most 128 characters"),
            "{error}"
        );

        assert_eq!(intervention_rows(&conn), 0, "refusals never mint receipts");
    }

    #[test]
    fn stale_session_revision_is_a_typed_conflict_without_a_row() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "stale-attach");
        ingest_harness_session_event(
            &mut conn,
            &selector,
            &fact("ev-1", HarnessSessionEventKind::Started, 3),
            &host(),
            "admission-1",
        )
        .unwrap();
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request("req-1", HarnessSessionInterventionKind::RequestCancel, 0),
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            matches!(error, MemoryError::WorkClaimConflict(_)),
            "{error}"
        );
        assert!(error.to_string().contains("revision is 3"));
        assert_eq!(intervention_rows(&conn), 0);
        let fresh = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request("req-2", HarnessSessionInterventionKind::RequestCancel, 3),
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            fresh.admission,
            HarnessSessionInterventionAdmission::Created
        );
    }

    #[test]
    fn terminal_sessions_refuse_mutating_interventions_but_keep_status() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "terminal-attach");
        let done = NewHarnessSessionEvent {
            outcome: Some(HarnessSessionTerminalOutcome::Completed),
            ..fact("ev-1", HarnessSessionEventKind::Terminal, 1)
        };
        ingest_harness_session_event(&mut conn, &selector, &done, &host(), "admission-1").unwrap();
        for kind in [
            HarnessSessionInterventionKind::RequestPause,
            HarnessSessionInterventionKind::RequestCancel,
            HarnessSessionInterventionKind::RequestResume,
            HarnessSessionInterventionKind::PromptOrCorrect,
        ] {
            let error = request_harness_session_intervention(
                &mut conn,
                &selector,
                &request("req-mutating", kind, 1),
                &host(),
                "admission-1",
            )
            .unwrap_err();
            assert!(
                matches!(error, MemoryError::WorkClaimIncompatibleState(_)),
                "{}: {error}",
                kind.as_str()
            );
            assert_eq!(
                intervention_rows(&conn),
                0,
                "{} must not mint a receipt",
                kind.as_str()
            );
        }
        let status = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-status",
                HarnessSessionInterventionKind::RequestStatus,
                1,
            ),
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            status.admission,
            HarnessSessionInterventionAdmission::Created
        );
    }

    #[test]
    fn orphaned_sessions_admit_status_and_cancel_only() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "orphan-attach");
        mark_harness_session_connection(
            &mut conn,
            &selector,
            HarnessSessionConnectionFact::Disconnected,
            &host(),
            "admission-1",
        )
        .unwrap();
        for kind in [
            HarnessSessionInterventionKind::PromptOrCorrect,
            HarnessSessionInterventionKind::RequestPause,
            HarnessSessionInterventionKind::RequestResume,
        ] {
            let error = request_harness_session_intervention(
                &mut conn,
                &selector,
                &request("req-orphan", kind, 0),
                &host(),
                "admission-1",
            )
            .unwrap_err();
            assert!(
                matches!(error, MemoryError::WorkClaimIncompatibleState(_)),
                "{}",
                kind.as_str()
            );
            assert_eq!(intervention_rows(&conn), 0);
        }
        for (request_id, kind) in [
            (
                "req-orphan-status",
                HarnessSessionInterventionKind::RequestStatus,
            ),
            (
                "req-orphan-cancel",
                HarnessSessionInterventionKind::RequestCancel,
            ),
        ] {
            let receipt = request_harness_session_intervention(
                &mut conn,
                &selector,
                &request(request_id, kind, 0),
                &host(),
                "admission-1",
            )
            .unwrap();
            assert_eq!(
                receipt.admission,
                HarnessSessionInterventionAdmission::Created
            );
        }
    }

    #[test]
    fn accepted_cancel_requires_authoritative_confirmation_and_composes_the_spine() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "cancel-law-attach");
        ingest_harness_session_event(
            &mut conn,
            &selector,
            &fact("ev-1", HarnessSessionEventKind::Started, 1),
            &host(),
            "admission-1",
        )
        .unwrap();
        request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-cancel",
                HarnessSessionInterventionKind::RequestCancel,
                1,
            ),
            &host(),
            "admission-1",
        )
        .unwrap();

        // Without the harness confirmation reference nothing is recorded.
        let error = record_harness_session_intervention_result(
            &mut conn,
            &selector,
            &NewHarnessSessionInterventionResult {
                request_id: "req-cancel".into(),
                disposition: HarnessSessionInterventionDisposition::Accepted,
                authority_confirmation_ref: None,
                detail: None,
            },
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("refusing to fake a cancellation"),
            "{error}"
        );
        let results: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM harness_session_intervention_results",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(results, 0);

        // With the harness confirmation the result lands; the canonical
        // state does NOT move here — only the terminal fact can move it.
        let receipt = record_harness_session_intervention_result(
            &mut conn,
            &selector,
            &NewHarnessSessionInterventionResult {
                request_id: "req-cancel".into(),
                disposition: HarnessSessionInterventionDisposition::Accepted,
                authority_confirmation_ref: Some("harness-confirm-7".into()),
                detail: Some("harness acknowledged the cancel".into()),
            },
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            receipt.admission,
            HarnessSessionInterventionAdmission::Created
        );
        assert_eq!(
            receipt.request_kind,
            HarnessSessionInterventionKind::RequestCancel
        );
        assert_ne!(
            receipt.state.canonical_state,
            Some(HarnessSessionCanonicalState::Cancelled),
            "an intervention result is never the lifecycle state"
        );

        // The terminal fact binds the same confirmation; only then is
        // `cancelled` canonical.
        let terminal = NewHarnessSessionEvent {
            outcome: Some(HarnessSessionTerminalOutcome::Cancelled),
            authority_confirmation_ref: Some("harness-confirm-7".into()),
            ..fact("ev-2", HarnessSessionEventKind::Terminal, 2)
        };
        let done =
            ingest_harness_session_event(&mut conn, &selector, &terminal, &host(), "admission-1")
                .unwrap();
        assert_eq!(
            done.state.canonical_state,
            Some(HarnessSessionCanonicalState::Cancelled)
        );

        // Result replay is idempotent; a different result for the same
        // request conflicts.
        let replay = record_harness_session_intervention_result(
            &mut conn,
            &selector,
            &NewHarnessSessionInterventionResult {
                request_id: "req-cancel".into(),
                disposition: HarnessSessionInterventionDisposition::Accepted,
                authority_confirmation_ref: Some("harness-confirm-7".into()),
                detail: Some("harness acknowledged the cancel".into()),
            },
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            replay.admission,
            HarnessSessionInterventionAdmission::Replayed
        );
        assert_eq!(
            replay.request_kind,
            HarnessSessionInterventionKind::RequestCancel
        );
        let conflict = record_harness_session_intervention_result(
            &mut conn,
            &selector,
            &NewHarnessSessionInterventionResult {
                request_id: "req-cancel".into(),
                disposition: HarnessSessionInterventionDisposition::Refused,
                authority_confirmation_ref: None,
                detail: None,
            },
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(matches!(conflict, MemoryError::WorkClaimConflict(_)));

        // Results for unissued requests are refused.
        let unissued = record_harness_session_intervention_result(
            &mut conn,
            &selector,
            &NewHarnessSessionInterventionResult {
                request_id: "never-issued".into(),
                disposition: HarnessSessionInterventionDisposition::Accepted,
                authority_confirmation_ref: Some("whatever".into()),
                detail: None,
            },
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(matches!(unissued, MemoryError::NotFound(_)));
    }

    #[test]
    fn capability_advertisement_refreshes_the_gate_and_is_append_only() {
        let (mut conn, selector) = seeded_with_grant(GRANT_OBSERVE, "advertise-attach");
        // The attachment declared cancel: true by default in the test input,
        // but the observe policy still denies cancel. Advertise a narrower
        // capability set: the latest advertisement must win as gate input.
        let narrowed = HarnessSessionAttachmentCapabilities {
            observe: true,
            ..Default::default()
        };
        let (seq_one, json_one) = advertise_harness_session_capabilities(
            &mut conn,
            &selector,
            &narrowed,
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(seq_one, 1);
        assert_eq!(
            json_one,
            r#"{"observe":true,"wait":false,"prompt":false,"cancel":false,"resume":false,"load":false,"events":false,"artifacts":false}"#
        );
        // A wide set on top; the gate must consult seq 2.
        let wide = HarnessSessionAttachmentCapabilities {
            observe: true,
            cancel: true,
            ..Default::default()
        };
        let (seq_two, _) = advertise_harness_session_capabilities(
            &mut conn,
            &selector,
            &wide,
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(seq_two, 2);

        // Even with cancel advertised by the latest host advertisement, the
        // observe policy still denies the kind: both gates must admit.
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-after-ads",
                HarnessSessionInterventionKind::RequestCancel,
                0,
            ),
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(matches!(
            error,
            MemoryError::UnsupportedByLifecycleOwner { .. }
        ));

        // A delegate-profile attachment whose host advertised cancel:false
        // refuses cancel on the capability gate even though policy admits.
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "advertise-delegate");
        let narrowed = HarnessSessionAttachmentCapabilities {
            observe: true,
            ..Default::default()
        };
        advertise_harness_session_capabilities(
            &mut conn,
            &selector,
            &narrowed,
            &host(),
            "admission-1",
        )
        .unwrap();
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-cap-gate",
                HarnessSessionInterventionKind::RequestCancel,
                0,
            ),
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(matches!(
            error,
            MemoryError::UnsupportedByLifecycleOwner { .. }
        ));
        assert_eq!(intervention_rows(&conn), 0);
        // Status remains admitted via the advertised observe capability.
        let status = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-cap-status",
                HarnessSessionInterventionKind::RequestStatus,
                0,
            ),
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            status.admission,
            HarnessSessionInterventionAdmission::Created
        );
        assert_eq!(status.capability_source, CapabilitySource::Advertised);
        let replay = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-cap-status",
                HarnessSessionInterventionKind::RequestStatus,
                0,
            ),
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            replay.admission,
            HarnessSessionInterventionAdmission::Replayed
        );
        assert_eq!(
            replay.capability_source,
            CapabilitySource::Advertised,
            "idempotent replay must preserve the original authorization provenance"
        );
    }

    #[test]
    fn foreign_capability_advertisements_are_closed_bounded_and_canonical() {
        let (mut conn, selector) = seeded_with_grant(GRANT_DELEGATE, "foreign-capability-json");
        let HarnessSessionAttachmentSelector::AttachmentId(attachment_id) = &selector else {
            panic!("attachment-id selector")
        };
        let with_unknown = r#"{"observe":true,"wait":true,"prompt":true,"cancel":true,"resume":true,"load":true,"events":true,"artifacts":true,"transcript":"unbounded"}"#;
        let error = conn
            .execute(
                "INSERT INTO harness_session_capability_advertisements
                 (attachment_id, advertisement_seq, capabilities_json, source_host_identity, advertised_at)
                 VALUES (?1, 1, ?2, 'host-1', '2026-08-30T00:00:00Z')",
                params![attachment_id, with_unknown],
            )
            .expect_err("unknown capability content must fail at storage");
        assert!(error.to_string().contains("CHECK constraint"), "{error}");

        for incomplete in ["{}", r#"{"observe":true}"#] {
            let error = conn
                .execute(
                    "INSERT INTO harness_session_capability_advertisements
                     (attachment_id, advertisement_seq, capabilities_json, source_host_identity, advertised_at)
                     VALUES (?1, 1, ?2, 'host-1', '2026-08-30T00:00:00Z')",
                    params![attachment_id, incomplete],
                )
                .expect_err("missing capability fields must fail at storage");
            assert!(error.to_string().contains("CHECK constraint"), "{error}");
        }

        let non_canonical = r#"{ "artifacts":true,"events":true,"load":true,"resume":true,"cancel":true,"prompt":true,"wait":true,"observe":true }"#;
        conn.execute(
            "INSERT INTO harness_session_capability_advertisements
             (attachment_id, advertisement_seq, capabilities_json, source_host_identity, advertised_at)
             VALUES (?1, 1, ?2, 'host-1', '2026-08-30T00:00:00Z')",
            params![attachment_id, non_canonical],
        )
        .expect("closed bounded foreign JSON reaches the read-side canonical gate");
        let error = request_harness_session_intervention(
            &mut conn,
            &selector,
            &request(
                "req-foreign-json",
                HarnessSessionInterventionKind::RequestStatus,
                0,
            ),
            &host(),
            "admission-1",
        )
        .expect_err("non-canonical advertisement must not authorize a request");
        assert!(error.to_string().contains("not canonical JSON"), "{error}");
        assert_eq!(intervention_rows(&conn), 0);
    }
}
