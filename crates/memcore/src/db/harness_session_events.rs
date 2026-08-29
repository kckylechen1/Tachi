//! Authoritative attached-session event spine (#1678).
//!
//! The host/harness owns spawn, session, wait, cancel, resume, and the
//! transcript. This module only *ingests* the authoritative
//! accepted/started/progress/input-required/terminal/cleanup facts the host
//! reports for an admitted [`crate::db::harness_session_attachments`]
//! attachment, maintains one canonical session-state projection per
//! attachment, and issues typed connection-fact receipts
//! (disappearance/reconnect). It never spawns, signals, or reaps a session,
//! never owns a lifecycle handle, and never stores a transcript: evidence is
//! bounded to a public-safe summary and an optional host-side artifact
//! digest.
//!
//! Event law (issue #1678 body):
//! - append-only, event-id/revision bound, replay-idempotent,
//!   source-attributed;
//! - out-of-order/stale events cannot regress canonical state;
//! - worker `submit` or process exit does not establish semantic acceptance;
//! - conflicting terminal facts produce `inconsistent_reconciling`, never a
//!   guessed success;
//! - session disappearance without a terminal receipt yields
//!   `unknown_orphaned`, not failed/completed;
//! - `cancelled` is never minted from an arbitrary string: the terminal
//!   fact's confirmation reference must match a RECORDED accepted
//!   `request_cancel` intervention result for the same attachment (the
//!   issue's receipt chain: request accepted -> real harness confirmation
//!   received -> terminal+cleanup receipts).

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::db::harness_session_attachments::{
    verify_existing_attachment, HarnessSessionAttachment, HarnessSessionAttachmentSelector,
    HarnessSessionAttachmentState, HarnessSessionHostAdmission,
};
use crate::db::normalize_utc_iso_or_now;
use crate::error::MemoryError;

/// Closed lifecycle fact vocabulary the host may report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessSessionEventKind {
    Accepted,
    Started,
    Progress,
    InputRequired,
    Terminal,
    Cleanup,
}

impl HarnessSessionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Started => "started",
            Self::Progress => "progress",
            Self::InputRequired => "input_required",
            Self::Terminal => "terminal",
            Self::Cleanup => "cleanup",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "accepted" => Ok(Self::Accepted),
            "started" => Ok(Self::Started),
            "progress" => Ok(Self::Progress),
            "input_required" => Ok(Self::InputRequired),
            "terminal" => Ok(Self::Terminal),
            "cleanup" => Ok(Self::Cleanup),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown harness session event kind '{other}'"
            ))),
        }
    }

    fn rank(self) -> i64 {
        match self {
            Self::Accepted => 0,
            Self::Started => 1,
            Self::Progress | Self::InputRequired => 2,
            Self::Terminal => 3,
            Self::Cleanup => 4,
        }
    }
}

/// Closed terminal outcome vocabulary. `cancelled` requires an authoritative
/// harness confirmation reference at the storage layer (a CHECK constraint
/// enforces the same law against foreign writers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessSessionTerminalOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl HarnessSessionTerminalOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown harness session terminal outcome '{other}'"
            ))),
        }
    }
}

/// One canonical session-state projection per attachment. This is the
/// materialized view of the append-only event spine; the events table remains
/// the receipt of record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessSessionCanonicalState {
    Accepted,
    Started,
    Progressing,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
    /// Two authoritative terminal facts disagree. Stuck by design: Tachi
    /// never guesses a winner; adjudication (#1623) owns resolution.
    InconsistentReconciling,
    /// The host reported the session gone and no terminal receipt exists.
    /// Recoverable by authoritative facts after reconnect.
    UnknownOrphaned,
}

impl HarnessSessionCanonicalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Started => "started",
            Self::Progressing => "progressing",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::InconsistentReconciling => "inconsistent_reconciling",
            Self::UnknownOrphaned => "unknown_orphaned",
        }
    }

    fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "accepted" => Ok(Self::Accepted),
            "started" => Ok(Self::Started),
            "progressing" => Ok(Self::Progressing),
            "input_required" => Ok(Self::InputRequired),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "inconsistent_reconciling" => Ok(Self::InconsistentReconciling),
            "unknown_orphaned" => Ok(Self::UnknownOrphaned),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown canonical harness session state '{other}'"
            ))),
        }
    }

    fn rank(self) -> i64 {
        match self {
            Self::UnknownOrphaned => -1,
            Self::Accepted => 0,
            Self::Started => 1,
            Self::Progressing | Self::InputRequired => 2,
            Self::Completed | Self::Failed | Self::Cancelled => 3,
            Self::InconsistentReconciling => 4,
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::InconsistentReconciling
        )
    }

    fn from_terminal(outcome: HarnessSessionTerminalOutcome) -> Self {
        match outcome {
            HarnessSessionTerminalOutcome::Completed => Self::Completed,
            HarnessSessionTerminalOutcome::Failed => Self::Failed,
            HarnessSessionTerminalOutcome::Cancelled => Self::Cancelled,
        }
    }
}

/// Input to the append-only event-ingest writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHarnessSessionEvent {
    pub event_id: String,
    pub kind: HarnessSessionEventKind,
    pub outcome: Option<HarnessSessionTerminalOutcome>,
    /// Host-side monotone sequence for the source stream. A higher value is
    /// never a license to regress canonical state; it only wins the
    /// high-water revision when the fact itself advances state.
    pub source_revision: i64,
    /// Authoritative harness confirmation binding. Required (enforced by
    /// storage CHECK and the writer) exactly when
    /// `outcome == Some(Cancelled)`.
    pub authority_confirmation_ref: Option<String>,
    /// Public-safe bounded summary. Never a transcript: the writer refuses
    /// oversize text and no raw-payload column exists.
    pub summary: Option<String>,
    /// Host-side digest of a bounded artifact; the artifact itself never
    /// enters Tachi.
    pub payload_digest: Option<String>,
    pub occurred_at: String,
}

/// A journaled authoritative fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSessionEvent {
    pub event_row_id: i64,
    pub attachment_id: String,
    pub event_id: String,
    pub kind: HarnessSessionEventKind,
    pub outcome: Option<HarnessSessionTerminalOutcome>,
    pub source_revision: i64,
    pub authority_confirmation_ref: Option<String>,
    pub summary: Option<String>,
    pub payload_digest: Option<String>,
    pub occurred_at: String,
    pub ingested_at: String,
    pub source_host_identity: String,
}

/// What the ingest did to canonical state for this fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessSessionEventDisposition {
    /// Canonical state advanced (or the high-water revision grew, or cleanup
    /// was recorded on an existing terminal).
    Advanced,
    /// Fact journaled; out-of-order/stale facts never regress canonical
    /// state.
    JournaledStale,
    /// Conflicting terminal fact: canonical moved to
    /// `inconsistent_reconciling`.
    JournaledTerminalConflict,
    /// Same-outcome terminal fact journaled; canonical terminal unchanged.
    JournaledRedundantTerminal,
}

impl HarnessSessionEventDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advanced => "advanced",
            Self::JournaledStale => "journaled_stale",
            Self::JournaledTerminalConflict => "journaled_terminal_conflict",
            Self::JournaledRedundantTerminal => "journaled_redundant_terminal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessSessionEventAdmission {
    Journaled,
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionEventReceipt {
    pub attachment_id: String,
    pub event_id: String,
    pub admission: HarnessSessionEventAdmission,
    pub disposition: HarnessSessionEventDisposition,
    pub state: HarnessSessionStateProjection,
}

/// Materialized canonical projection; `canonical_state == None` means no
/// authoritative fact has been ingested for this attachment yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSessionStateProjection {
    pub attachment_id: String,
    pub canonical_state: Option<HarnessSessionCanonicalState>,
    pub canonical_revision: i64,
    pub cleanup_recorded: bool,
    pub conflicting_terminal: bool,
    pub last_event_id: Option<String>,
    pub updated_at: Option<String>,
}

/// Host-reported connection facts. `Disconnected` maps the attachment to the
/// reserved `unknown` attachment state; `ReconnectFailed` to
/// `reconnect_failed`. Both are receipts of host claims, never process
/// observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessSessionConnectionFact {
    Disconnected,
    ReconnectFailed,
}

impl HarnessSessionConnectionFact {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::ReconnectFailed => "reconnect_failed",
        }
    }

    fn attachment_state(self) -> HarnessSessionAttachmentState {
        match self {
            Self::Disconnected => HarnessSessionAttachmentState::Unknown,
            Self::ReconnectFailed => HarnessSessionAttachmentState::ReconnectFailed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionConnectionReceipt {
    pub attachment: HarnessSessionAttachment,
    pub previous_attachment_state: HarnessSessionAttachmentState,
    pub changed: bool,
    pub state: HarnessSessionStateProjection,
}

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

const EVENT_COLUMNS: &str = "event_row_id, attachment_id, event_id, kind, outcome, \
    source_revision, authority_confirmation_ref, summary, payload_digest, \
    occurred_at, ingested_at, source_host_identity";

fn row_to_event(row: &rusqlite::Row<'_>) -> Result<HarnessSessionEvent, rusqlite::Error> {
    let kind_raw: String = row.get(3)?;
    let kind = HarnessSessionEventKind::parse(&kind_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })?;
    let outcome_raw: Option<String> = row.get(4)?;
    let outcome = outcome_raw
        .map(|raw| HarnessSessionTerminalOutcome::parse(&raw))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    error.to_string(),
                )),
            )
        })?;
    Ok(HarnessSessionEvent {
        event_row_id: row.get(0)?,
        attachment_id: row.get(1)?,
        event_id: row.get(2)?,
        kind,
        outcome,
        source_revision: row.get(5)?,
        authority_confirmation_ref: row.get(6)?,
        summary: row.get(7)?,
        payload_digest: row.get(8)?,
        occurred_at: row.get(9)?,
        ingested_at: row.get(10)?,
        source_host_identity: row.get(11)?,
    })
}

/// Canonical state row shared with the intervention module's receipts.
pub(crate) struct CanonicalStateRow {
    pub(crate) canonical_state: Option<HarnessSessionCanonicalState>,
    pub(crate) canonical_revision: i64,
    pub(crate) terminal_digest: Option<String>,
    pub(crate) conflicting_terminal_digest: Option<String>,
    pub(crate) cleanup_recorded: bool,
    pub(crate) last_event_id: Option<String>,
    pub(crate) pre_disconnect_rank: i64,
    pub(crate) updated_at: Option<String>,
}

pub(crate) fn load_state_row_for_interventions(
    conn: &Connection,
    attachment_id: &str,
) -> Result<Option<CanonicalStateRow>, MemoryError> {
    load_state_row(conn, attachment_id)
}

pub(crate) fn require_attachment_for_interventions(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<HarnessSessionAttachment, MemoryError> {
    require_attachment(conn, selector, host, admission_receipt_ref)
}

pub(crate) fn resolve_attachment_for_interventions(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<Option<HarnessSessionAttachment>, MemoryError> {
    resolve_attachment_for_host(conn, selector, host, admission_receipt_ref)
}

fn load_state_row(
    tx: &Connection,
    attachment_id: &str,
) -> Result<Option<CanonicalStateRow>, MemoryError> {
    let row = tx
        .query_row(
            "SELECT canonical_state, canonical_revision, terminal_digest,
                    conflicting_terminal_digest, cleanup_recorded, last_event_id,
                    pre_disconnect_rank, updated_at
             FROM harness_session_state WHERE attachment_id = ?1",
            params![attachment_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        canonical_raw,
        canonical_revision,
        terminal_digest,
        conflicting_terminal_digest,
        cleanup_recorded,
        last_event_id,
        pre_disconnect_rank,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(CanonicalStateRow {
        canonical_state: canonical_raw
            .map(|raw| HarnessSessionCanonicalState::parse(&raw))
            .transpose()?,
        canonical_revision,
        terminal_digest,
        conflicting_terminal_digest,
        cleanup_recorded: cleanup_recorded != 0,
        last_event_id,
        pre_disconnect_rank,
        updated_at,
    }))
}

fn projection_of(
    attachment_id: &str,
    row: Option<CanonicalStateRow>,
) -> HarnessSessionStateProjection {
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

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).expect("JSON string serialization"),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| {
                        format!(
                            "{}:{}",
                            serde_json::to_string(key).expect("JSON key serialization"),
                            canonical_json(&values[key])
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// Outcome-focused terminal digest: the conflict unit for
/// `inconsistent_reconciling` is the authoritative OUTCOME, so two
/// same-outcome facts under different event ids corroborate the recorded
/// terminal instead of conflicting with it. The full per-event binding
/// (event id, confirmation reference) remains on the event row itself.
fn terminal_digest(outcome: HarnessSessionTerminalOutcome) -> String {
    let fact = serde_json::json!({ "outcome": outcome.as_str() });
    sha256_hex(&canonical_json(&fact))
}

fn require_non_empty(value: &str, field: &str) -> Result<(), MemoryError> {
    if value.trim().is_empty() {
        return Err(MemoryError::InvalidArg(format!(
            "{field} must be non-empty"
        )));
    }
    Ok(())
}

/// Public-safe persistent text must be exactly what every reader sees:
/// control characters (NUL included, which SQLite text functions truncate
/// at) are refused rather than stored.
fn require_no_control(value: &str, field: &str) -> Result<(), MemoryError> {
    if value.chars().any(|c| c.is_control()) {
        return Err(MemoryError::InvalidArg(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

fn validate_new_event(input: &NewHarnessSessionEvent) -> Result<(), MemoryError> {
    require_non_empty(&input.event_id, "event_id")?;
    require_no_control(&input.event_id, "event_id")?;
    if input.event_id.chars().count() > 128 {
        return Err(MemoryError::InvalidArg(
            "event_id must be at most 128 characters".to_string(),
        ));
    }
    require_non_empty(&input.occurred_at, "occurred_at")?;
    require_no_control(&input.occurred_at, "occurred_at")?;
    if input.occurred_at.chars().count() > 64 {
        return Err(MemoryError::InvalidArg(
            "occurred_at must be at most 64 characters".to_string(),
        ));
    }
    if input.source_revision < 0 {
        return Err(MemoryError::InvalidArg(
            "source_revision must be non-negative".to_string(),
        ));
    }
    if let Some(summary) = &input.summary {
        if summary.is_empty() || summary.chars().count() > 2000 {
            return Err(MemoryError::InvalidArg(
                "summary must be non-empty and at most 2000 characters; it is bounded evidence, never a transcript"
                    .to_string(),
            ));
        }
        // Control characters (including NUL, which SQLite text functions
        // silently truncate at) are refused so the bounded public-safe text
        // stays exactly what every reader sees.
        if summary.chars().any(|c| c.is_control()) {
            return Err(MemoryError::InvalidArg(
                "summary must not contain control characters".to_string(),
            ));
        }
    }
    if let Some(digest) = &input.payload_digest {
        require_non_empty(digest, "payload_digest")?;
        // A digest is a fixed-width opaque fingerprint, never a content
        // channel: cap its width and forbid free text so a transcript
        // cannot be smuggled through this field.
        if digest.chars().count() > 128
            || !digest.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=' | '+' | '/' | ':')
            })
        {
            return Err(MemoryError::InvalidArg(
                "payload_digest must be at most 128 digest-safe ASCII characters (no whitespace or free text)"
                    .to_string(),
            ));
        }
    }
    if let Some(confirmation) = &input.authority_confirmation_ref {
        require_non_empty(confirmation, "authority_confirmation_ref")?;
        // A confirmation reference is a bounded receipt pointer, never a
        // content channel: no control characters (NUL included), no
        // oversize text.
        if confirmation.chars().any(|c| c.is_control()) {
            return Err(MemoryError::InvalidArg(
                "authority_confirmation_ref must not contain control characters".to_string(),
            ));
        }
        if confirmation.chars().count() > 128 {
            return Err(MemoryError::InvalidArg(
                "authority_confirmation_ref must be at most 128 characters".to_string(),
            ));
        }
    }
    match input.kind {
        HarnessSessionEventKind::Terminal => {
            let Some(outcome) = input.outcome else {
                return Err(MemoryError::InvalidArg(
                    "terminal events must carry an outcome".to_string(),
                ));
            };
            if outcome == HarnessSessionTerminalOutcome::Cancelled
                && input
                    .authority_confirmation_ref
                    .as_deref()
                    .is_none_or(str::is_empty)
            {
                return Err(MemoryError::WorkClaimIncompatibleState(
                    "terminal 'cancelled' requires an authoritative harness confirmation reference; refusing to mint 'cancelled'"
                        .to_string(),
                ));
            }
        }
        HarnessSessionEventKind::Cleanup => {
            if input.outcome.is_some() {
                return Err(MemoryError::InvalidArg(
                    "cleanup events must not carry an outcome".to_string(),
                ));
            }
        }
        _ => {
            if input.outcome.is_some() {
                return Err(MemoryError::InvalidArg(format!(
                    "{} events must not carry an outcome",
                    input.kind.as_str()
                )));
            }
        }
    }
    Ok(())
}

/// Host-bound attachment resolution WITHOUT claim-freshness or policy
/// re-verification. Facts must keep flowing after the WorkClaim is released
/// (a terminal fact often arrives after work ownership ends), so the spine
/// verifies only that the reporting connection is the attachment's admitted
/// host. A foreign host receives the same typed not-found shape whether or
/// not a durable row exists.
fn resolve_attachment_for_host(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<Option<HarnessSessionAttachment>, MemoryError> {
    if let HarnessSessionAttachmentSelector::NaturalKey {
        protocol_version, ..
    } = selector
    {
        if protocol_version != "1" {
            return Err(MemoryError::InvalidArg(
                "ACP negotiated protocol_version must be exactly 1".to_string(),
            ));
        }
    }
    crate::db::harness_session_attachments::find_attachment_for_host(
        conn,
        selector,
        host,
        admission_receipt_ref,
    )
}

fn require_attachment(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<HarnessSessionAttachment, MemoryError> {
    resolve_attachment_for_host(conn, selector, host, admission_receipt_ref)?.ok_or_else(|| {
        MemoryError::NotFound(
            "harness session spine was not found for the current host connection".to_string(),
        )
    })
}

fn insert_event(
    tx: &Connection,
    attachment_id: &str,
    input: &NewHarnessSessionEvent,
    now: &str,
    source_host_identity: &str,
) -> Result<HarnessSessionEvent, MemoryError> {
    tx.execute(
        "INSERT INTO harness_session_events (
            attachment_id, event_id, kind, outcome, source_revision,
            authority_confirmation_ref, summary, payload_digest,
            occurred_at, ingested_at, source_host_identity
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            attachment_id,
            input.event_id,
            input.kind.as_str(),
            input.outcome.map(HarnessSessionTerminalOutcome::as_str),
            input.source_revision,
            input.authority_confirmation_ref,
            input.summary,
            input.payload_digest,
            input.occurred_at,
            now,
            source_host_identity,
        ],
    )?;
    let event_row_id = tx.last_insert_rowid();
    Ok(HarnessSessionEvent {
        event_row_id,
        attachment_id: attachment_id.to_string(),
        event_id: input.event_id.clone(),
        kind: input.kind,
        outcome: input.outcome,
        source_revision: input.source_revision,
        authority_confirmation_ref: input.authority_confirmation_ref.clone(),
        summary: input.summary.clone(),
        payload_digest: input.payload_digest.clone(),
        occurred_at: input.occurred_at.clone(),
        ingested_at: now.to_string(),
        source_host_identity: source_host_identity.to_string(),
    })
}

/// The next canonical projection to materialize for an attachment.
struct StateUpsert {
    canonical: HarnessSessionCanonicalState,
    revision: i64,
    terminal_digest: Option<String>,
    conflicting_terminal_digest: Option<String>,
    cleanup_recorded: bool,
    /// Lifecycle rank retained across a disappearance marker: a
    /// post-disconnect fact may only advance at or beyond this rank, so a
    /// fresh replay cannot walk the lifecycle backward.
    pre_disconnect_rank: i64,
}

fn upsert_state_row(
    tx: &Connection,
    attachment_id: &str,
    upsert: &StateUpsert,
    last_event_id: &str,
    now: &str,
) -> Result<(), MemoryError> {
    tx.execute(
        "INSERT INTO harness_session_state (
            attachment_id, canonical_state, canonical_revision, terminal_digest,
            conflicting_terminal_digest, cleanup_recorded, last_event_id,
            pre_disconnect_rank, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(attachment_id) DO UPDATE SET
            canonical_state = excluded.canonical_state,
            canonical_revision = excluded.canonical_revision,
            terminal_digest = excluded.terminal_digest,
            conflicting_terminal_digest = excluded.conflicting_terminal_digest,
            cleanup_recorded = excluded.cleanup_recorded,
            last_event_id = excluded.last_event_id,
            pre_disconnect_rank = excluded.pre_disconnect_rank,
            updated_at = excluded.updated_at",
        params![
            attachment_id,
            upsert.canonical.as_str(),
            upsert.revision,
            upsert.terminal_digest,
            upsert.conflicting_terminal_digest,
            i64::from(upsert.cleanup_recorded),
            if last_event_id.is_empty() {
                None
            } else {
                Some(last_event_id)
            },
            upsert.pre_disconnect_rank,
            now,
        ],
    )?;
    Ok(())
}

fn find_event(
    tx: &Connection,
    attachment_id: &str,
    event_id: &str,
) -> Result<Option<HarnessSessionEvent>, MemoryError> {
    Ok(tx
        .query_row(
            &format!(
                "SELECT {EVENT_COLUMNS} FROM harness_session_events
                 WHERE attachment_id = ?1 AND event_id = ?2"
            ),
            params![attachment_id, event_id],
            row_to_event,
        )
        .optional()?)
}

fn same_material(event: &HarnessSessionEvent, input: &NewHarnessSessionEvent) -> bool {
    event.kind == input.kind
        && event.outcome == input.outcome
        && event.source_revision == input.source_revision
        && event.authority_confirmation_ref == input.authority_confirmation_ref
        && event.summary == input.summary
        && event.payload_digest == input.payload_digest
        && event.occurred_at == input.occurred_at
}

/// The cancelled-mint hard line: a terminal `cancelled` fact's confirmation
/// reference must match a RECORDED accepted `request_cancel` intervention
/// result for this attachment. A caller-provided arbitrary string never
/// mints `cancelled` — neither on journaling nor on replay.
fn require_bound_cancel_confirmation(
    tx: &Connection,
    attachment_id: &str,
    input: &NewHarnessSessionEvent,
) -> Result<(), MemoryError> {
    let bound: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM harness_session_intervention_results r
             JOIN harness_session_interventions i
               ON i.attachment_id = r.attachment_id
              AND i.request_id = r.request_id
             WHERE r.attachment_id = ?1
               AND i.kind = 'request_cancel'
               AND r.disposition = 'accepted'
               AND r.authority_confirmation_ref = ?2
             LIMIT 1",
            params![
                attachment_id,
                input.authority_confirmation_ref.as_deref().unwrap_or("")
            ],
            |row| row.get(0),
        )
        .optional()?;
    if bound.is_none() {
        return Err(MemoryError::WorkClaimIncompatibleState(
            "terminal 'cancelled' confirmation reference does not match a recorded accepted request_cancel intervention result; refusing to mint 'cancelled'"
                .to_string(),
        ));
    }
    Ok(())
}

/// Ingest one authoritative host fact. Append-only, replay-idempotent on
/// `(attachment_id, event_id)`, and never regressive: everything runs in one
/// IMMEDIATE transaction so out-of-order arrivals journal without moving
/// canonical state backward and conflicting terminals reconcile explicitly.
///
/// A `Replayed` receipt re-reports the current projection: the fact is
/// already part of the canonical spine, so its disposition repeats
/// [`HarnessSessionEventDisposition::Advanced`] with no new journal row.
pub fn ingest_harness_session_event(
    conn: &mut Connection,
    selector: &HarnessSessionAttachmentSelector,
    input: &NewHarnessSessionEvent,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<HarnessSessionEventReceipt, MemoryError> {
    validate_new_event(input)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attachment = require_attachment(&tx, selector, host, admission_receipt_ref)?;
    let attachment_id = attachment.attachment_id.clone();
    let source_host_identity = attachment.host_identity.clone();

    // The cancelled-binding law runs BEFORE the replay fast-path, so even an
    // exact replay of an unbound `cancelled` fact (e.g. journaled by an
    // older kernel) refuses instead of re-confirming a minted outcome.
    if input.kind == HarnessSessionEventKind::Terminal
        && input.outcome == Some(HarnessSessionTerminalOutcome::Cancelled)
    {
        require_bound_cancel_confirmation(&tx, &attachment_id, input)?;
    }

    if let Some(existing) = find_event(&tx, &attachment_id, &input.event_id)? {
        if !same_material(&existing, input) {
            return Err(MemoryError::WorkClaimConflict(format!(
                "harness session event {} replays with different content",
                input.event_id
            )));
        }
        let projection = projection_of(&attachment_id, load_state_row(&tx, &attachment_id)?);
        tx.commit()?;
        return Ok(HarnessSessionEventReceipt {
            attachment_id,
            event_id: existing.event_id,
            admission: HarnessSessionEventAdmission::Replayed,
            disposition: HarnessSessionEventDisposition::Advanced,
            state: projection,
        });
    }

    let now = normalize_utc_iso_or_now("");
    let existing_row = load_state_row(&tx, &attachment_id)?;
    let current_state = existing_row.as_ref().and_then(|row| row.canonical_state);
    let current_rank = current_state
        .map(HarnessSessionCanonicalState::rank)
        .unwrap_or(-1);
    let mut canonical_revision = existing_row
        .as_ref()
        .map(|row| row.canonical_revision)
        .unwrap_or(0);
    let mut canonical = current_state;
    let mut terminal_digest_to_store = existing_row
        .as_ref()
        .and_then(|row| row.terminal_digest.clone());
    let mut conflicting_to_store = existing_row
        .as_ref()
        .and_then(|row| row.conflicting_terminal_digest.clone());
    let mut cleanup_recorded = existing_row
        .as_ref()
        .is_some_and(|row| row.cleanup_recorded);

    // A disappearance receipt keeps the revision high-water AND the
    // lifecycle rank the session had reached. Only a fact at least as fresh
    // AND at least as advanced may lift unknown_orphaned; this guard sits
    // ABOVE the terminal branch so a stale terminal cannot resurrect state
    // either.
    let pre_disconnect_rank = existing_row
        .as_ref()
        .map(|row| row.pre_disconnect_rank)
        .unwrap_or(-1);
    if current_rank < 0
        && (input.source_revision < canonical_revision || input.kind.rank() < pre_disconnect_rank)
    {
        let now = normalize_utc_iso_or_now("");
        insert_event(&tx, &attachment_id, input, &now, &source_host_identity)?;
        let projection = projection_of(&attachment_id, load_state_row(&tx, &attachment_id)?);
        tx.commit()?;
        return Ok(HarnessSessionEventReceipt {
            attachment_id,
            event_id: input.event_id.clone(),
            admission: HarnessSessionEventAdmission::Journaled,
            disposition: HarnessSessionEventDisposition::JournaledStale,
            state: projection,
        });
    }

    let disposition = if input.kind == HarnessSessionEventKind::Terminal {
        let outcome = input
            .outcome
            .expect("validated terminal carries an outcome");
        let digest = terminal_digest(outcome);
        match &terminal_digest_to_store {
            Some(recorded_digest) => {
                canonical_revision = canonical_revision.max(input.source_revision);
                if recorded_digest == &digest {
                    // Same authoritative outcome under a second event id; the
                    // recorded terminal stands.
                    HarnessSessionEventDisposition::JournaledRedundantTerminal
                } else {
                    // Conflicting authoritative terminal facts never resolve
                    // to a guessed success.
                    canonical = Some(HarnessSessionCanonicalState::InconsistentReconciling);
                    conflicting_to_store = Some(digest);
                    HarnessSessionEventDisposition::JournaledTerminalConflict
                }
            }
            None => {
                terminal_digest_to_store = Some(digest);
                canonical = Some(HarnessSessionCanonicalState::from_terminal(outcome));
                HarnessSessionEventDisposition::Advanced
            }
        }
    } else {
        let event_rank = input.kind.rank();
        if event_rank > current_rank {
            if input.kind == HarnessSessionEventKind::Cleanup {
                match current_state
                    .filter(|state: &HarnessSessionCanonicalState| state.is_terminal())
                {
                    Some(terminal) => {
                        // Result disposition recorded on the canonical
                        // terminal; state itself stays.
                        cleanup_recorded = true;
                        canonical = Some(terminal);
                        HarnessSessionEventDisposition::Advanced
                    }
                    None => HarnessSessionEventDisposition::JournaledStale,
                }
            } else {
                canonical = Some(match input.kind {
                    HarnessSessionEventKind::Accepted => HarnessSessionCanonicalState::Accepted,
                    HarnessSessionEventKind::Started => HarnessSessionCanonicalState::Started,
                    HarnessSessionEventKind::Progress => HarnessSessionCanonicalState::Progressing,
                    HarnessSessionEventKind::InputRequired => {
                        HarnessSessionCanonicalState::InputRequired
                    }
                    HarnessSessionEventKind::Terminal | HarnessSessionEventKind::Cleanup => {
                        unreachable!("terminal/cleanup handled above")
                    }
                });
                HarnessSessionEventDisposition::Advanced
            }
        } else {
            // Equal or lower rank. The only legal equal-rank movement is the
            // progress <-> input_required refresh on a fresh revision;
            // everything else journals without regressing canonical state.
            let target = match input.kind {
                HarnessSessionEventKind::Progress => {
                    Some(HarnessSessionCanonicalState::Progressing)
                }
                HarnessSessionEventKind::InputRequired => {
                    Some(HarnessSessionCanonicalState::InputRequired)
                }
                _ => None,
            };
            let equal_rank_flip = target.is_some()
                && event_rank == current_rank
                && matches!(
                    canonical,
                    Some(HarnessSessionCanonicalState::Progressing)
                        | Some(HarnessSessionCanonicalState::InputRequired)
                )
                && input.source_revision >= canonical_revision
                && canonical != target;
            if equal_rank_flip {
                canonical = target;
                HarnessSessionEventDisposition::Advanced
            } else {
                HarnessSessionEventDisposition::JournaledStale
            }
        }
    };

    if input.source_revision > canonical_revision {
        canonical_revision = input.source_revision;
    }

    insert_event(&tx, &attachment_id, input, &now, &source_host_identity)?;
    if let Some(canonical) = canonical {
        upsert_state_row(
            &tx,
            &attachment_id,
            &StateUpsert {
                canonical,
                revision: canonical_revision,
                terminal_digest: terminal_digest_to_store,
                conflicting_terminal_digest: conflicting_to_store,
                cleanup_recorded,
                pre_disconnect_rank: existing_row
                    .as_ref()
                    .map(|row| row.pre_disconnect_rank)
                    .unwrap_or(-1),
            },
            &input.event_id,
            &now,
        )?;
    }
    let projection = projection_of(&attachment_id, load_state_row(&tx, &attachment_id)?);
    tx.commit()?;
    Ok(HarnessSessionEventReceipt {
        attachment_id,
        event_id: input.event_id.clone(),
        admission: HarnessSessionEventAdmission::Journaled,
        disposition,
        state: projection,
    })
}

/// Read-only canonical projection. The WorkClaim may be released; session
/// facts outlive work ownership.
pub fn get_harness_session_state(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<HarnessSessionStateProjection, MemoryError> {
    let attachment = require_attachment(conn, selector, host, admission_receipt_ref)?;
    let row = load_state_row(conn, &attachment.attachment_id)?;
    Ok(projection_of(&attachment.attachment_id, row))
}

/// Record a host-reported connection fact (disappearance or failed
/// reconnect). Moves the attachment to its reserved non-attached state and,
/// when no terminal receipt exists, canonical state to `unknown_orphaned` —
/// never to failed/completed. Idempotent on the same fact.
pub fn mark_harness_session_connection(
    conn: &mut Connection,
    selector: &HarnessSessionAttachmentSelector,
    fact: HarnessSessionConnectionFact,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
) -> Result<HarnessSessionConnectionReceipt, MemoryError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attachment = require_attachment(&tx, selector, host, admission_receipt_ref)?;
    let attachment_id = attachment.attachment_id.clone();
    let previous = attachment.state;
    let target = fact.attachment_state();
    let mut changed = false;
    if previous != target {
        // Legal attachment-state flow for connection facts: attached -> either
        // non-attached state; the two non-attached states may swap as the
        // host retries; only reconnect_session may restore `attached`.
        let legal = previous == HarnessSessionAttachmentState::Attached
            || (previous == HarnessSessionAttachmentState::Unknown
                && target == HarnessSessionAttachmentState::ReconnectFailed)
            || (previous == HarnessSessionAttachmentState::ReconnectFailed
                && target == HarnessSessionAttachmentState::Unknown);
        if !legal {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "attachment connection fact '{}' cannot follow state '{}'",
                fact.as_str(),
                previous.as_str()
            )));
        }
        tx.execute(
            "UPDATE harness_session_attachments
             SET state = ?1, updated_at = ?2 WHERE attachment_id = ?3",
            params![target.as_str(), normalize_utc_iso_or_now(""), attachment_id],
        )?;
        changed = true;
    }

    // Disappearance without a terminal receipt yields unknown/orphaned, never
    // failed/completed. A recorded terminal is authoritative and stays.
    let existing_row = load_state_row(&tx, &attachment_id)?;
    let now = normalize_utc_iso_or_now("");
    match &existing_row {
        Some(row) => {
            if !row
                .canonical_state
                .is_some_and(HarnessSessionCanonicalState::is_terminal)
            {
                upsert_state_row(
                    &tx,
                    &attachment_id,
                    &StateUpsert {
                        canonical: HarnessSessionCanonicalState::UnknownOrphaned,
                        revision: row.canonical_revision,
                        terminal_digest: row.terminal_digest.clone(),
                        conflicting_terminal_digest: row.conflicting_terminal_digest.clone(),
                        cleanup_recorded: row.cleanup_recorded,
                        // Retain the lifecycle rank the session had reached
                        // so a post-disconnect fact cannot walk it backward.
                        // A duplicate disconnect must not clobber the first
                        // one's rank with the marker's own rank (-1).
                        pre_disconnect_rank: row
                            .canonical_state
                            .filter(|state| *state != HarnessSessionCanonicalState::UnknownOrphaned)
                            .map(HarnessSessionCanonicalState::rank)
                            .unwrap_or(-1)
                            .max(row.pre_disconnect_rank),
                    },
                    row.last_event_id.as_deref().unwrap_or(""),
                    &now,
                )?;
            }
        }
        None => {
            upsert_state_row(
                &tx,
                &attachment_id,
                &StateUpsert {
                    canonical: HarnessSessionCanonicalState::UnknownOrphaned,
                    revision: 0,
                    terminal_digest: None,
                    conflicting_terminal_digest: None,
                    cleanup_recorded: false,
                    pre_disconnect_rank: -1,
                },
                "",
                &now,
            )?;
        }
    }
    tx.commit()?;

    let updated_attachment =
        crate::db::harness_session_attachments::find_attachment_by_id(conn, &attachment_id)?
            .expect("attachment row just read in the same process");
    let projection = projection_of(&attachment_id, load_state_row(conn, &attachment_id)?);
    Ok(HarnessSessionConnectionReceipt {
        attachment: updated_attachment,
        previous_attachment_state: previous,
        changed,
        state: projection,
    })
}

/// Host-owned reconnect receipt: the admitted host re-establishes its
/// binding and the attachment returns to `attached` after full
/// re-admission (fresh WorkClaim, current policy digest). Canonical session
/// state is untouched — recovery comes from authoritative facts, and the
/// host resumes its event replay from the returned revision.
pub fn reconnect_harness_session(
    conn: &mut Connection,
    selector: &HarnessSessionAttachmentSelector,
    host: &HarnessSessionHostAdmission,
    admission_receipt_ref: &str,
    ttl_seconds: i64,
) -> Result<HarnessSessionReconnectReceipt, MemoryError> {
    if ttl_seconds < 0 {
        return Err(MemoryError::InvalidArg(
            "WorkClaim freshness TTL must be non-negative".to_string(),
        ));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attachment = require_attachment(&tx, selector, host, admission_receipt_ref)?;
    let attachment_id = attachment.attachment_id.clone();
    // Re-admission: same verification bar as attach replay — current host
    // admission, fresh active WorkClaim, unchanged policy digest.
    verify_existing_attachment(&tx, &attachment, host, admission_receipt_ref, ttl_seconds)?;
    let previous = attachment.state;
    let mut reconnected = false;
    if previous != HarnessSessionAttachmentState::Attached {
        tx.execute(
            "UPDATE harness_session_attachments
             SET state = 'attached', updated_at = ?1 WHERE attachment_id = ?2",
            params![normalize_utc_iso_or_now(""), attachment_id],
        )?;
        reconnected = true;
    }
    tx.commit()?;

    let updated_attachment =
        crate::db::harness_session_attachments::find_attachment_by_id(conn, &attachment_id)?
            .expect("attachment row just read in the same process");
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_schema;
    use crate::db::session_claims::{
        insert_agent_identity, insert_work_claim, AgentIdentity, NewWorkClaim, WorkClaimMode,
    };

    const GRANT_DELEGATE: &str =
        r#"{"acp":{"tool_profiles":["delegate"],"capability_classes":["tachi"]}}"#;

    fn policy_digest_for(json: &str) -> String {
        crate::db::harness_session_attachments::authorization_digest_for_test(
            json, "delegate", "tachi",
        )
    }

    /// Seed an admitted attachment exactly like the v33 writer would create
    /// it, returning the connection plus the attachment's natural key.
    fn seeded() -> (Connection, HarnessSessionAttachmentSelector) {
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
                capability_json: Some(GRANT_DELEGATE.into()),
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
                worktree_path: "/tmp/spine-claim".into(),
                declared_file_scope: "[\"src/lib.rs\"]".into(),
                role: "executor".into(),
                mode: WorkClaimMode::Writable,
                expected_head: "head".into(),
                lease_expires_at: "2030-01-01T00:00:00Z".into(),
                created_at: String::new(),
            },
        )
        .unwrap();
        let mut input =
            crate::db::harness_session_attachments::test_attachment_input("attachment-input");
        input.policy_digest = policy_digest_for(GRANT_DELEGATE);
        let receipt = crate::db::harness_session_attachments::attach_harness_session(
            &mut conn,
            &input,
            &HarnessSessionHostAdmission {
                host_identity: "host-1".into(),
                connection_id: "connection-1".into(),
            },
            30 * 60,
        )
        .unwrap();
        assert_eq!(
            receipt.admission,
            crate::db::harness_session_attachments::HarnessSessionAttachmentAdmission::Created
        );
        let selector = HarnessSessionAttachmentSelector::AttachmentId(
            receipt.attachment.attachment_id.clone(),
        );
        (conn, selector)
    }

    fn host() -> HarnessSessionHostAdmission {
        HarnessSessionHostAdmission {
            host_identity: "host-1".into(),
            connection_id: "connection-1".into(),
        }
    }

    fn event(
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
            summary: Some(format!("public-safe summary for {event_id}")),
            payload_digest: None,
            occurred_at: "2026-08-29T00:00:00Z".into(),
        }
    }

    fn terminal(
        event_id: &str,
        outcome: HarnessSessionTerminalOutcome,
        revision: i64,
        confirmation: Option<&str>,
    ) -> NewHarnessSessionEvent {
        NewHarnessSessionEvent {
            event_id: event_id.into(),
            kind: HarnessSessionEventKind::Terminal,
            outcome: Some(outcome),
            source_revision: revision,
            authority_confirmation_ref: confirmation.map(str::to_string),
            summary: Some("public-safe terminal summary".into()),
            payload_digest: None,
            occurred_at: "2026-08-29T00:01:00Z".into(),
        }
    }

    fn ingest(
        conn: &mut Connection,
        selector: &HarnessSessionAttachmentSelector,
        input: &NewHarnessSessionEvent,
    ) -> HarnessSessionEventReceipt {
        ingest_harness_session_event(conn, selector, input, &host(), "admission-1").unwrap()
    }

    fn event_row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM harness_session_events", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn replayed_facts_build_one_canonical_spine_without_duplicate_rows() {
        let (mut conn, selector) = seeded();
        let first = ingest(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Accepted, 1),
        );
        let started = ingest(
            &mut conn,
            &selector,
            &event("ev-2", HarnessSessionEventKind::Started, 2),
        );
        let progress = ingest(
            &mut conn,
            &selector,
            &event("ev-3", HarnessSessionEventKind::Progress, 3),
        );
        assert_eq!(
            first.state.canonical_state,
            Some(HarnessSessionCanonicalState::Accepted)
        );
        assert_eq!(
            started.state.canonical_state,
            Some(HarnessSessionCanonicalState::Started)
        );
        assert_eq!(
            progress.state.canonical_state,
            Some(HarnessSessionCanonicalState::Progressing)
        );
        assert_eq!(progress.state.canonical_revision, 3);
        assert_eq!(event_row_count(&conn), 3);

        let replay = ingest(
            &mut conn,
            &selector,
            &event("ev-3", HarnessSessionEventKind::Progress, 3),
        );
        assert_eq!(replay.admission, HarnessSessionEventAdmission::Replayed);
        assert_eq!(event_row_count(&conn), 3, "replay must not add a row");
        assert_eq!(
            replay.state.canonical_state,
            Some(HarnessSessionCanonicalState::Progressing)
        );

        let mut forged = event("ev-3", HarnessSessionEventKind::Progress, 4);
        forged.summary = Some("different content".into());
        let error =
            ingest_harness_session_event(&mut conn, &selector, &forged, &host(), "admission-1")
                .unwrap_err();
        assert!(
            matches!(error, MemoryError::WorkClaimConflict(_)),
            "{error}"
        );
        assert_eq!(event_row_count(&conn), 3);
    }

    #[test]
    fn out_of_order_and_stale_facts_cannot_regress_canonical_state() {
        let (mut conn, selector) = seeded();
        ingest(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Accepted, 1),
        );
        ingest(
            &mut conn,
            &selector,
            &event("ev-2", HarnessSessionEventKind::Started, 2),
        );
        let done = ingest(
            &mut conn,
            &selector,
            &terminal("ev-5", HarnessSessionTerminalOutcome::Completed, 5, None),
        );
        assert_eq!(
            done.state.canonical_state,
            Some(HarnessSessionCanonicalState::Completed)
        );

        // A late-arriving progress fact from before the terminal.
        let stale = ingest(
            &mut conn,
            &selector,
            &event("ev-3", HarnessSessionEventKind::Progress, 3),
        );
        assert_eq!(
            stale.disposition,
            HarnessSessionEventDisposition::JournaledStale
        );
        assert_eq!(
            stale.state.canonical_state,
            Some(HarnessSessionCanonicalState::Completed),
            "canonical state must not regress"
        );
        // A re-ranked accepted fact with a fresh revision is still not a
        // regression license.
        let fresh_rank = ingest(
            &mut conn,
            &selector,
            &event("ev-6", HarnessSessionEventKind::Accepted, 6),
        );
        assert_eq!(
            fresh_rank.disposition,
            HarnessSessionEventDisposition::JournaledStale
        );
        assert_eq!(
            fresh_rank.state.canonical_state,
            Some(HarnessSessionCanonicalState::Completed)
        );
        assert_eq!(
            fresh_rank.state.canonical_revision, 6,
            "high-water revision still grows"
        );
        // Cleanup after the terminal records the result disposition.
        let cleanup = ingest(
            &mut conn,
            &selector,
            &event("ev-7", HarnessSessionEventKind::Cleanup, 7),
        );
        assert_eq!(
            cleanup.disposition,
            HarnessSessionEventDisposition::Advanced
        );
        assert!(cleanup.state.cleanup_recorded);
        assert_eq!(
            cleanup.state.canonical_state,
            Some(HarnessSessionCanonicalState::Completed)
        );
    }

    #[test]
    fn conflicting_terminal_facts_reconcile_to_inconsistent_never_a_guessed_success() {
        let (mut conn, selector) = seeded();
        ingest(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Started, 1),
        );
        let completed = ingest(
            &mut conn,
            &selector,
            &terminal("ev-2", HarnessSessionTerminalOutcome::Completed, 2, None),
        );
        assert_eq!(
            completed.state.canonical_state,
            Some(HarnessSessionCanonicalState::Completed)
        );

        let failed = ingest(
            &mut conn,
            &selector,
            &terminal("ev-3", HarnessSessionTerminalOutcome::Failed, 3, None),
        );
        assert_eq!(
            failed.disposition,
            HarnessSessionEventDisposition::JournaledTerminalConflict
        );
        assert_eq!(
            failed.state.canonical_state,
            Some(HarnessSessionCanonicalState::InconsistentReconciling)
        );
        assert!(failed.state.conflicting_terminal);

        // A third same-outcome-as-original terminal does not unstick the
        // reconciliation; adjudication owns resolution.
        let again = ingest(
            &mut conn,
            &selector,
            &terminal("ev-4", HarnessSessionTerminalOutcome::Completed, 4, None),
        );
        assert_eq!(
            again.disposition,
            HarnessSessionEventDisposition::JournaledRedundantTerminal
        );
        assert_eq!(
            again.state.canonical_state,
            Some(HarnessSessionCanonicalState::InconsistentReconciling)
        );
    }

    #[test]
    fn cancelled_without_authoritative_confirmation_is_refused_before_insert() {
        let (mut conn, selector) = seeded();
        ingest(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Started, 1),
        );
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &terminal("ev-2", HarnessSessionTerminalOutcome::Cancelled, 2, None),
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("refusing to mint 'cancelled'"),
            "{error}"
        );
        assert_eq!(
            event_row_count(&conn),
            1,
            "refusal must not journal the fact"
        );
        let state = get_harness_session_state(&conn, &selector, &host(), "admission-1").unwrap();
        assert_eq!(
            state.canonical_state,
            Some(HarnessSessionCanonicalState::Started)
        );
    }

    #[test]
    fn disappearance_yields_unknown_orphaned_and_reconnect_recovers_with_resume_revision() {
        let (mut conn, selector) = seeded();
        ingest(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Started, 4),
        );
        let gone = mark_harness_session_connection(
            &mut conn,
            &selector,
            HarnessSessionConnectionFact::Disconnected,
            &host(),
            "admission-1",
        )
        .unwrap();
        assert!(gone.changed);
        assert_eq!(
            gone.previous_attachment_state,
            HarnessSessionAttachmentState::Attached
        );
        assert_eq!(
            gone.attachment.state,
            HarnessSessionAttachmentState::Unknown
        );
        assert_eq!(
            gone.state.canonical_state,
            Some(HarnessSessionCanonicalState::UnknownOrphaned),
            "disappearance without a terminal receipt must yield unknown/orphaned"
        );

        // Reconnect re-admits the binding (fresh claim + policy) and returns
        // the resume revision; canonical state is untouched.
        let back = reconnect_harness_session(&mut conn, &selector, &host(), "admission-1", 30 * 60)
            .unwrap();
        assert!(back.reconnected);
        assert_eq!(
            back.attachment.state,
            HarnessSessionAttachmentState::Attached
        );
        assert_eq!(back.resume_from_revision, 4);
        assert_eq!(
            back.state.canonical_state,
            Some(HarnessSessionCanonicalState::UnknownOrphaned)
        );

        // Authoritative facts after reconnect recover the spine.
        let done = ingest(
            &mut conn,
            &selector,
            &terminal("ev-2", HarnessSessionTerminalOutcome::Completed, 5, None),
        );
        assert_eq!(
            done.state.canonical_state,
            Some(HarnessSessionCanonicalState::Completed)
        );

        // A second disconnect after a recorded terminal does not regress
        // canonical state to unknown_orphaned.
        let late = mark_harness_session_connection(
            &mut conn,
            &selector,
            HarnessSessionConnectionFact::ReconnectFailed,
            &host(),
            "admission-1",
        )
        .unwrap();
        assert_eq!(
            late.attachment.state,
            HarnessSessionAttachmentState::ReconnectFailed
        );
        assert_eq!(
            late.state.canonical_state,
            Some(HarnessSessionCanonicalState::Completed)
        );
    }

    #[test]
    fn submit_and_exit_are_not_session_facts_and_adjudication_is_absent_from_the_spine() {
        for kind in ["submit", "exit", "accepted_by_worker"] {
            let error = HarnessSessionEventKind::parse(kind)
                .expect_err("worker-submit/process-exit vocabulary is not a session fact");
            assert!(error
                .to_string()
                .contains("unknown harness session event kind"));
        }
        let (conn, selector) = seeded();
        let state = get_harness_session_state(&conn, &selector, &host(), "admission-1").unwrap();
        assert_eq!(state.canonical_state, None, "no facts: no canonical claim");
        // The projection carries no usefulness/judgment field at all.
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("judg"));
        assert!(!json.contains("adjudicat"));
        assert!(!json.contains("usefulness"));
    }

    #[test]
    fn session_facts_flow_after_the_work_claim_is_released() {
        let (mut conn, selector) = seeded();
        conn.execute(
            "UPDATE session_claims SET state = 'released', transition_version = transition_version + 1
             WHERE claim_id = 'claim-1'",
            [],
        )
        .unwrap();
        // The binding-only resolver keeps the spine alive after work
        // ownership ends; a terminal fact still lands.
        let done = ingest(
            &mut conn,
            &selector,
            &terminal("ev-1", HarnessSessionTerminalOutcome::Failed, 1, None),
        );
        assert_eq!(
            done.state.canonical_state,
            Some(HarnessSessionCanonicalState::Failed)
        );
        // While reconnect (an admission surface) refuses: the claim is gone.
        let error =
            reconnect_harness_session(&mut conn, &selector, &host(), "admission-1", 30 * 60)
                .unwrap_err();
        assert!(error.to_string().contains("not active"), "{error}");
    }

    #[test]
    fn cancelled_terminal_binds_to_a_recorded_accepted_cancel_result() {
        let (mut conn, selector) = seeded();
        ingest(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Started, 1),
        );

        // An arbitrary confirmation string with no recorded result behind it
        // can never mint `cancelled` (codex R2 finding 1).
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &terminal(
                "ev-2",
                HarnessSessionTerminalOutcome::Cancelled,
                2,
                Some("x"),
            ),
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match a recorded accepted request_cancel"),
            "{error}"
        );
        assert_eq!(event_row_count(&conn), 1);

        // Record the receipt chain: issued request_cancel + accepted result
        // carrying the harness confirmation.
        conn.execute(
            "INSERT INTO harness_session_interventions (
                attachment_id, request_id, kind, reason, expected_session_revision,
                requested_by, requested_at
             ) SELECT attachment_id, 'req-cancel', 'request_cancel', 'operator stop', 1,
                'host-1', '2026-08-29T00:00:00Z' FROM harness_session_attachments",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO harness_session_intervention_results (
                attachment_id, request_id, disposition, authority_confirmation_ref,
                recorded_at, source_host_identity
             ) SELECT attachment_id, 'req-cancel', 'accepted', 'x',
                '2026-08-29T00:01:00Z', 'host-1' FROM harness_session_attachments",
            [],
        )
        .unwrap();

        let done = ingest(
            &mut conn,
            &selector,
            &terminal(
                "ev-3",
                HarnessSessionTerminalOutcome::Cancelled,
                3,
                Some("x"),
            ),
        );
        assert_eq!(
            done.state.canonical_state,
            Some(HarnessSessionCanonicalState::Cancelled)
        );
        assert_eq!(event_row_count(&conn), 2);
    }

    #[test]
    fn stale_fact_after_disconnection_cannot_lift_unknown_orphaned() {
        let (mut conn, selector) = seeded();
        ingest(
            &mut conn,
            &selector,
            &event("ev-5", HarnessSessionEventKind::Progress, 5),
        );
        mark_harness_session_connection(
            &mut conn,
            &selector,
            HarnessSessionConnectionFact::Disconnected,
            &host(),
            "admission-1",
        )
        .unwrap();

        // Codex R2 finding 2: a stale replay from before the disappearance
        // must not clear unknown_orphaned, even though its rank is higher
        // than the disappearance marker.
        let stale = ingest(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Accepted, 1),
        );
        assert_eq!(
            stale.disposition,
            HarnessSessionEventDisposition::JournaledStale
        );
        assert_eq!(
            stale.state.canonical_state,
            Some(HarnessSessionCanonicalState::UnknownOrphaned),
            "unknown_orphaned must survive a stale replay"
        );

        // A FRESH fact below the retained lifecycle rank also cannot lift
        // the marker: the Progressing@5 -> disconnect -> Started@6 path
        // must journal stale instead of regressing to started (codex R3).
        // A duplicate disconnect must not clobber the retained rank with the
        // marker's own rank (codex R4).
        mark_harness_session_connection(
            &mut conn,
            &selector,
            HarnessSessionConnectionFact::Disconnected,
            &host(),
            "admission-1",
        )
        .unwrap();

        let fresh_low = ingest(
            &mut conn,
            &selector,
            &event("ev-6", HarnessSessionEventKind::Started, 6),
        );
        assert_eq!(
            fresh_low.disposition,
            HarnessSessionEventDisposition::JournaledStale
        );
        assert_eq!(
            fresh_low.state.canonical_state,
            Some(HarnessSessionCanonicalState::UnknownOrphaned)
        );

        // Only a fact at least as fresh AND at least as advanced lifts it.
        reconnect_harness_session(&mut conn, &selector, &host(), "admission-1", 30 * 60).unwrap();
        let fresh = ingest(
            &mut conn,
            &selector,
            &event("ev-7", HarnessSessionEventKind::Progress, 7),
        );
        assert_eq!(fresh.disposition, HarnessSessionEventDisposition::Advanced);
        assert_eq!(
            fresh.state.canonical_state,
            Some(HarnessSessionCanonicalState::Progressing)
        );
    }

    #[test]
    fn replay_of_an_unbound_cancelled_fact_refuses_instead_of_reconfirming() {
        let (mut conn, selector) = seeded();
        ingest(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Started, 1),
        );
        // Simulate a pre-binding-law row (as an older kernel could have
        // journaled): the unbound cancelled fact is already in the ledger.
        conn.execute(
            "INSERT INTO harness_session_events (
                attachment_id, event_id, kind, outcome, source_revision,
                authority_confirmation_ref, occurred_at, ingested_at, source_host_identity
             ) SELECT attachment_id, 'ev-cancel-old', 'terminal', 'cancelled', 2, 'x',
                '2026-08-29T00:01:00Z', '2026-08-29T00:01:00Z', 'host-1'
             FROM harness_session_attachments",
            [],
        )
        .unwrap();

        // An exact replay must hit the cancelled-binding law BEFORE the
        // replay fast-path: typed refusal, never a Replayed receipt.
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &terminal(
                "ev-cancel-old",
                HarnessSessionTerminalOutcome::Cancelled,
                2,
                Some("x"),
            ),
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match a recorded accepted request_cancel"),
            "{error}"
        );
    }

    #[test]
    fn stale_terminal_after_disconnection_cannot_lift_unknown_orphaned() {
        let (mut conn, selector) = seeded();
        ingest(
            &mut conn,
            &selector,
            &event("ev-5", HarnessSessionEventKind::Progress, 5),
        );
        mark_harness_session_connection(
            &mut conn,
            &selector,
            HarnessSessionConnectionFact::Disconnected,
            &host(),
            "admission-1",
        )
        .unwrap();

        // A stale TERMINAL replay from before the disappearance must not
        // resurrect a completed outcome (codex R2 finding 2).
        let stale = ingest(
            &mut conn,
            &selector,
            &terminal("ev-1", HarnessSessionTerminalOutcome::Completed, 1, None),
        );
        assert_eq!(
            stale.disposition,
            HarnessSessionEventDisposition::JournaledStale
        );
        assert_eq!(
            stale.state.canonical_state,
            Some(HarnessSessionCanonicalState::UnknownOrphaned),
            "unknown_orphaned must survive a stale terminal replay"
        );

        // A fresh terminal still lifts it.
        let fresh = ingest(
            &mut conn,
            &selector,
            &terminal("ev-6", HarnessSessionTerminalOutcome::Completed, 6, None),
        );
        assert_eq!(fresh.disposition, HarnessSessionEventDisposition::Advanced);
        assert_eq!(
            fresh.state.canonical_state,
            Some(HarnessSessionCanonicalState::Completed)
        );
    }

    #[test]
    fn digest_check_constraint_rejects_free_text_from_foreign_writers() {
        let (conn, _selector) = seeded();
        let attachment_id: String = conn
            .query_row(
                "SELECT attachment_id FROM harness_session_attachments LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        for smuggled in [
            "free text with spaces",
            "quotes \" inside",
            "unicode \u{00e9}",
        ] {
            let error = conn
                .execute(
                    "INSERT INTO harness_session_events (
                        attachment_id, event_id, kind, source_revision,
                        payload_digest, occurred_at, ingested_at, source_host_identity
                     ) VALUES (?1, 'ev-smuggled', 'progress', 1, ?2,
                               '2026-08-29T00:00:00Z', '2026-08-29T00:00:00Z', 'host-1')",
                    rusqlite::params![attachment_id, smuggled],
                )
                .expect_err("free text must violate the digest CHECK");
            assert!(
                matches!(error, rusqlite::Error::SqliteFailure(_, _)),
                "{smuggled}: {error}"
            );
        }
        // A legitimate digest shape passes the same CHECK.
        conn.execute(
            "INSERT INTO harness_session_events (
                attachment_id, event_id, kind, source_revision,
                payload_digest, occurred_at, ingested_at, source_host_identity
             ) VALUES (?1, 'ev-digest-ok', 'progress', 1, 'sha256:abcdef0123456789',
                       '2026-08-29T00:00:00Z', '2026-08-29T00:00:00Z', 'host-1')",
            rusqlite::params![attachment_id],
        )
        .expect("prefixed hex digest is digest-safe");
    }

    #[test]
    fn writer_rejects_nul_and_control_characters_in_summary_and_digest() {
        let (mut conn, selector) = seeded();
        let mut nul_summary = event("ev-nul", HarnessSessionEventKind::Progress, 1);
        nul_summary.summary = Some("visible\u{0000}smuggled tail".to_string());
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &nul_summary,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");

        let mut control_summary = event("ev-ctl", HarnessSessionEventKind::Progress, 1);
        control_summary.summary = Some("line one\nline two".to_string());
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &control_summary,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");

        let mut nul_digest = event("ev-nul-digest", HarnessSessionEventKind::Progress, 1);
        nul_digest.payload_digest = Some("sha256:abc\u{0000} free text".to_string());
        let error =
            ingest_harness_session_event(&mut conn, &selector, &nul_digest, &host(), "admission-1")
                .unwrap_err();
        assert!(error.to_string().contains("digest-safe ASCII"), "{error}");
        assert_eq!(event_row_count(&conn), 0, "refusals never journal");
    }

    #[test]
    fn event_ids_and_occurred_at_are_bounded_and_control_free() {
        let (mut conn, selector) = seeded();

        let mut control_id = event("ev-ctl", HarnessSessionEventKind::Progress, 1);
        control_id.event_id = "id\u{0000}hidden".to_string();
        let error =
            ingest_harness_session_event(&mut conn, &selector, &control_id, &host(), "admission-1")
                .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");

        let mut oversize_id = event("ev-long", HarnessSessionEventKind::Progress, 1);
        oversize_id.event_id = "x".repeat(129);
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &oversize_id,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("at most 128 characters"),
            "{error}"
        );

        let mut oversize_time = event("ev-time", HarnessSessionEventKind::Progress, 1);
        oversize_time.occurred_at = "x".repeat(65);
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &oversize_time,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("at most 64 characters"),
            "{error}"
        );

        // Control characters in the occurrence timestamp are refused too:
        // removing only the occurred_at control guard must turn this test
        // red (codex R8).
        let mut control_time = event("ev-time-ctl", HarnessSessionEventKind::Progress, 1);
        control_time.occurred_at = "2026-08-29T00:00:00Z\u{0000}tail".to_string();
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &control_time,
            &host(),
            "admission-1",
        )
        .unwrap_err();
        assert!(error.to_string().contains("control characters"), "{error}");

        assert_eq!(event_row_count(&conn), 0, "refusals never journal");
    }

    #[test]
    fn foreign_host_receives_typed_not_found_without_row_disclosure() {
        let (mut conn, selector) = seeded();
        insert_agent_identity(
            &conn,
            &AgentIdentity {
                agent_identity_id: "host-2".into(),
                display_name: None,
                seat: None,
                capability_json: None,
                created_at: String::new(),
            },
        )
        .unwrap();
        conn.execute(
            "INSERT INTO identity_admissions
             (admission_id, agent_identity_id, connection_id, state, created_at)
             VALUES ('admission-2', 'host-2', 'connection-2', 'self_asserted', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let foreign = HarnessSessionHostAdmission {
            host_identity: "host-2".into(),
            connection_id: "connection-2".into(),
        };
        let error = ingest_harness_session_event(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Accepted, 1),
            &foreign,
            "admission-2",
        )
        .unwrap_err();
        assert!(matches!(error, MemoryError::NotFound(_)), "{error}");
        assert_eq!(event_row_count(&conn), 0);
        let wrong_receipt = ingest_harness_session_event(
            &mut conn,
            &selector,
            &event("ev-1", HarnessSessionEventKind::Accepted, 1),
            &host(),
            "admission-other",
        );
        assert!(matches!(wrong_receipt, Err(MemoryError::NotFound(_))));
        assert_eq!(event_row_count(&conn), 0);
    }
}
