//! Host-owned ACP session attachment admission and durable receipts (#1733).
//!
//! This module never owns an ACP process or sends an ACP lifecycle request. It
//! only verifies the caller's admission/claim evidence and records the exact
//! binding that a host may use when it performs `session/new`, `session/load`,
//! or `session/resume` itself.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::db::normalize_utc_iso_or_now;
use crate::error::MemoryError;

/// Closed lifecycle capability vocabulary accepted from an ACP host.
pub const ACP_SESSION_CAPABILITIES: &[&str] = &[
    "observe",
    "wait",
    "prompt",
    "cancel",
    "resume",
    "load",
    "events",
    "artifacts",
];

/// Canonical policy names accepted in an admitted AgentIdentity's `acp`
/// capability grant.  These are intentionally separate from host/provider
/// aliases: an alias can never mint an ACP attachment capability.
pub const ACP_TOOL_PROFILES: &[&str] = &[
    "observe",
    "remember",
    "coordinate",
    "operate",
    "standard",
    "delegate",
    "workflow",
];
pub const ACP_CAPABILITY_CLASSES: &[&str] = &["tachi", "memory", "standard", "delegate"];

/// Truth Tachi is allowed to persist for this attachment slice.  The latter
/// two states are reserved for a host-owned reconnect receipt; this module's
/// admission writer creates only `attached`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessSessionAttachmentState {
    Attached,
    ReconnectFailed,
    Unknown,
}

impl HarnessSessionAttachmentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::ReconnectFailed => "reconnect_failed",
            Self::Unknown => "unknown",
        }
    }

    fn parse(raw: &str) -> Result<Self, MemoryError> {
        match raw {
            "attached" => Ok(Self::Attached),
            "reconnect_failed" => Ok(Self::ReconnectFailed),
            "unknown" => Ok(Self::Unknown),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown ACP attachment state '{other}'"
            ))),
        }
    }
}

/// Closed capability set copied into the receipt. The JSON representation is
/// canonicalized by [`NewHarnessSessionAttachment::new`] before persistence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSessionAttachmentCapabilities {
    pub observe: bool,
    pub wait: bool,
    pub prompt: bool,
    pub cancel: bool,
    pub resume: bool,
    pub load: bool,
    pub events: bool,
    pub artifacts: bool,
}

/// The policy evidence selected from the persisted AgentIdentity grant.  The
/// digest covers both the canonical grant and the selected profile/class; the
/// raw capability JSON never enters the attachment receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionAttachmentAuthorization {
    pub policy_digest: String,
}

impl HarnessSessionAttachmentCapabilities {
    pub fn from_names(names: &[String]) -> Result<Self, MemoryError> {
        let mut capabilities = Self::default();
        for name in names {
            match name.trim() {
                "observe" => capabilities.observe = true,
                "wait" => capabilities.wait = true,
                "prompt" => capabilities.prompt = true,
                "cancel" => capabilities.cancel = true,
                "resume" => capabilities.resume = true,
                "load" => capabilities.load = true,
                "events" => capabilities.events = true,
                "artifacts" => capabilities.artifacts = true,
                "" => {
                    return Err(MemoryError::InvalidArg(
                        "session_capabilities cannot contain an empty value".to_string(),
                    ))
                }
                other => {
                    return Err(MemoryError::InvalidArg(format!(
                        "unsupported ACP session capability '{other}'"
                    )))
                }
            }
        }
        Ok(capabilities)
    }

    pub fn as_names(&self) -> Vec<&'static str> {
        [
            ("observe", self.observe),
            ("wait", self.wait),
            ("prompt", self.prompt),
            ("cancel", self.cancel),
            ("resume", self.resume),
            ("load", self.load),
            ("events", self.events),
            ("artifacts", self.artifacts),
        ]
        .into_iter()
        .filter_map(|(name, enabled)| enabled.then_some(name))
        .collect()
    }

    pub fn canonical_json(&self) -> Result<String, MemoryError> {
        Ok(serde_json::to_string(self)?)
    }
}

/// Input to the atomic attachment-admission writer. All fields are immutable
/// binding facts; the generated attachment id and timestamps are added by the
/// writer only after every admission check passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHarnessSessionAttachment {
    pub host_identity: String,
    pub protocol_version: String,
    pub adapter_connection_identity: String,
    pub remote_session_id: String,
    pub work_claim_id: String,
    pub expected_transition_version: i64,
    pub agent_identity_id: String,
    pub contract_digest: String,
    pub capabilities_json: String,
    pub tool_profile: String,
    pub capability_class: String,
    pub policy_digest: String,
    pub descriptor_digest: String,
    pub idempotency_key: String,
    pub admission_receipt_ref: String,
}

/// Stored attachment row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSessionAttachment {
    pub attachment_id: String,
    pub host_identity: String,
    pub protocol_version: String,
    pub adapter_connection_identity: String,
    pub remote_session_id: String,
    pub work_claim_id: String,
    pub expected_transition_version: i64,
    pub agent_identity_id: String,
    pub contract_digest: String,
    pub capabilities_json: String,
    pub tool_profile: String,
    pub capability_class: String,
    pub policy_digest: String,
    pub descriptor_digest: String,
    pub idempotency_key: String,
    pub admission_receipt_ref: String,
    pub state: HarnessSessionAttachmentState,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessSessionAttachmentAdmission {
    Created,
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSessionAttachmentReceipt {
    pub attachment: HarnessSessionAttachment,
    pub admission: HarnessSessionAttachmentAdmission,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessSessionAttachmentSelector {
    AttachmentId(String),
    NaturalKey {
        host_identity: String,
        protocol_version: String,
        adapter_connection_identity: String,
        remote_session_id: String,
    },
}

const SELECT_COLUMNS: &str = "attachment_id, host_identity, protocol_version, \
    adapter_connection_identity, remote_session_id, work_claim_id, \
    expected_transition_version, agent_identity_id, contract_digest, \
    capabilities_json, tool_profile, capability_class, policy_digest, \
    descriptor_digest, idempotency_key, admission_receipt_ref, state, \
    created_at, updated_at";

fn row_to_attachment(row: &rusqlite::Row<'_>) -> Result<HarnessSessionAttachment, rusqlite::Error> {
    let state_raw: String = row.get(16)?;
    let state = HarnessSessionAttachmentState::parse(&state_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            16,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })?;
    Ok(HarnessSessionAttachment {
        attachment_id: row.get(0)?,
        host_identity: row.get(1)?,
        protocol_version: row.get(2)?,
        adapter_connection_identity: row.get(3)?,
        remote_session_id: row.get(4)?,
        work_claim_id: row.get(5)?,
        expected_transition_version: row.get(6)?,
        agent_identity_id: row.get(7)?,
        contract_digest: row.get(8)?,
        capabilities_json: row.get(9)?,
        tool_profile: row.get(10)?,
        capability_class: row.get(11)?,
        policy_digest: row.get(12)?,
        descriptor_digest: row.get(13)?,
        idempotency_key: row.get(14)?,
        admission_receipt_ref: row.get(15)?,
        state,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
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

fn parse_grant_names(
    acp: &serde_json::Map<String, Value>,
    key: &str,
    allowed: &[&str],
) -> Result<Vec<String>, MemoryError> {
    let values = acp.get(key).and_then(Value::as_array).ok_or_else(|| {
        MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity acp.{key} grant must be an array"
        ))
    })?;
    let mut names = Vec::with_capacity(values.len());
    for value in values {
        let name = value.as_str().ok_or_else(|| {
            MemoryError::WorkClaimIncompatibleState(format!(
                "AgentIdentity acp.{key} grant entries must be strings"
            ))
        })?;
        if !allowed.contains(&name) {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "AgentIdentity acp.{key} grant contains unknown canonical name '{name}'"
            )));
        }
        if names.iter().any(|existing| existing == name) {
            return Err(MemoryError::WorkClaimIncompatibleState(format!(
                "AgentIdentity acp.{key} grant contains duplicate name '{name}'"
            )));
        }
        names.push(name.to_string());
    }
    names.sort_unstable();
    Ok(names)
}

fn authorization_from_capability_json(
    capability_json: Option<&str>,
    agent_identity_id: &str,
    tool_profile: &str,
    capability_class: &str,
) -> Result<HarnessSessionAttachmentAuthorization, MemoryError> {
    if !ACP_TOOL_PROFILES.contains(&tool_profile) {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "requested ACP tool profile '{tool_profile}' is not a canonical admitted profile"
        )));
    }
    if !ACP_CAPABILITY_CLASSES.contains(&capability_class) {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "requested ACP capability class '{capability_class}' is not a canonical admitted class"
        )));
    }
    let raw = capability_json.ok_or_else(|| {
        MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} has no ACP capability grant"
        ))
    })?;
    let root: Value = serde_json::from_str(raw).map_err(|error| {
        MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} has malformed capability_json: {error}"
        ))
    })?;
    let root_object = root.as_object().ok_or_else(|| {
        MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} capability_json must be an object"
        ))
    })?;
    let acp = root_object
        .get("acp")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            MemoryError::WorkClaimIncompatibleState(format!(
                "AgentIdentity {agent_identity_id} capability_json is missing object acp grant"
            ))
        })?;
    if acp
        .keys()
        .any(|key| key != "tool_profiles" && key != "capability_classes")
    {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} acp grant contains unknown fields"
        )));
    }
    let tool_profiles = parse_grant_names(acp, "tool_profiles", ACP_TOOL_PROFILES)?;
    let capability_classes = parse_grant_names(acp, "capability_classes", ACP_CAPABILITY_CLASSES)?;
    if !tool_profiles.iter().any(|name| name == tool_profile) {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} ACP tool profile '{tool_profile}' is not admitted"
        )));
    }
    if !capability_classes
        .iter()
        .any(|name| name == capability_class)
    {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "AgentIdentity {agent_identity_id} ACP capability class '{capability_class}' is not admitted"
        )));
    }
    let grant = serde_json::json!({
        "acp": {
            "tool_profiles": tool_profiles,
            "capability_classes": capability_classes,
        }
    });
    let decision = serde_json::json!({
        "grant": grant.clone(),
        "selected": {
            "tool_profile": tool_profile,
            "capability_class": capability_class,
        }
    });
    Ok(HarnessSessionAttachmentAuthorization {
        policy_digest: sha256_hex(&canonical_json(&decision)),
    })
}

/// Check the stored AgentIdentity ACP grant before a server materializes any
/// descriptor.  The attachment writer repeats this check inside its write
/// transaction, closing the read/write TOCTOU window.
pub fn authorize_harness_session_attachment(
    conn: &Connection,
    agent_identity_id: &str,
    tool_profile: &str,
    capability_class: &str,
) -> Result<HarnessSessionAttachmentAuthorization, MemoryError> {
    // Keep the two absence cases distinct: `None` from `optional()` means the
    // identity row is missing, while `Some(None)` means the persisted
    // capability_json column is SQL NULL.  Both refuse with the typed
    // no-grant error below; neither is allowed to surface rusqlite's raw NULL
    // conversion error.
    let capability_json: Option<Option<String>> = conn
        .query_row(
            "SELECT capability_json FROM agent_identities WHERE agent_identity_id = ?1",
            params![agent_identity_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    let Some(capability_json) = capability_json else {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "unknown agent identity {agent_identity_id}"
        )));
    };
    authorization_from_capability_json(
        capability_json.as_deref(),
        agent_identity_id,
        tool_profile,
        capability_class,
    )
}

fn validate_new_attachment(input: &NewHarnessSessionAttachment) -> Result<(), MemoryError> {
    for (field, value) in [
        ("host_identity", input.host_identity.as_str()),
        ("protocol_version", input.protocol_version.as_str()),
        (
            "adapter_connection_identity",
            input.adapter_connection_identity.as_str(),
        ),
        ("remote_session_id", input.remote_session_id.as_str()),
        ("work_claim_id", input.work_claim_id.as_str()),
        ("agent_identity_id", input.agent_identity_id.as_str()),
        ("contract_digest", input.contract_digest.as_str()),
        ("capabilities_json", input.capabilities_json.as_str()),
        ("tool_profile", input.tool_profile.as_str()),
        ("capability_class", input.capability_class.as_str()),
        ("policy_digest", input.policy_digest.as_str()),
        ("descriptor_digest", input.descriptor_digest.as_str()),
        ("idempotency_key", input.idempotency_key.as_str()),
        (
            "admission_receipt_ref",
            input.admission_receipt_ref.as_str(),
        ),
    ] {
        require_non_empty(value, field)?;
    }
    if input.expected_transition_version < 0 {
        return Err(MemoryError::InvalidArg(
            "expected_transition_version must be non-negative".to_string(),
        ));
    }
    let capabilities: HarnessSessionAttachmentCapabilities =
        serde_json::from_str(&input.capabilities_json).map_err(|error| {
            MemoryError::InvalidArg(format!("capabilities_json must be canonical JSON: {error}"))
        })?;
    if capabilities.canonical_json()? != input.capabilities_json {
        return Err(MemoryError::InvalidArg(
            "capabilities_json must use the canonical closed capability shape".to_string(),
        ));
    }
    Ok(())
}

fn same_binding(row: &HarnessSessionAttachment, input: &NewHarnessSessionAttachment) -> bool {
    row.host_identity == input.host_identity
        && row.protocol_version == input.protocol_version
        && row.adapter_connection_identity == input.adapter_connection_identity
        && row.remote_session_id == input.remote_session_id
        && row.work_claim_id == input.work_claim_id
        && row.expected_transition_version == input.expected_transition_version
        && row.agent_identity_id == input.agent_identity_id
        && row.contract_digest == input.contract_digest
        && row.capabilities_json == input.capabilities_json
        && row.tool_profile == input.tool_profile
        && row.capability_class == input.capability_class
        && row.policy_digest == input.policy_digest
        && row.descriptor_digest == input.descriptor_digest
        && row.idempotency_key == input.idempotency_key
        && row.admission_receipt_ref == input.admission_receipt_ref
}

fn find_by_idempotency_key(
    tx: &Transaction<'_>,
    idempotency_key: &str,
) -> Result<Option<HarnessSessionAttachment>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM harness_session_attachments WHERE idempotency_key = ?1"
    );
    Ok(tx
        .query_row(&sql, params![idempotency_key], row_to_attachment)
        .optional()?)
}

fn find_by_natural_key(
    tx: &Transaction<'_>,
    input: &NewHarnessSessionAttachment,
) -> Result<Option<HarnessSessionAttachment>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM harness_session_attachments
         WHERE host_identity = ?1 AND protocol_version = ?2
           AND adapter_connection_identity = ?3 AND remote_session_id = ?4"
    );
    Ok(tx
        .query_row(
            &sql,
            params![
                input.host_identity,
                input.protocol_version,
                input.adapter_connection_identity,
                input.remote_session_id,
            ],
            row_to_attachment,
        )
        .optional()?)
}

fn verify_admission_and_claim(
    tx: &Transaction<'_>,
    input: &NewHarnessSessionAttachment,
) -> Result<HarnessSessionAttachmentAuthorization, MemoryError> {
    // The admission reference may be either the durable admission id or the
    // connection id exposed by a host. In both forms the receipt must resolve
    // to the exact host connection and identity; an unavailable/rejected row
    // never reaches the attachment INSERT below.
    let admission: Option<(String, String, String)> = tx
        .query_row(
            "SELECT agent_identity_id, connection_id, state
             FROM identity_admissions
             WHERE (admission_id = ?1 OR connection_id = ?1)
             ORDER BY CASE WHEN admission_id = ?1 THEN 0 ELSE 1 END
             LIMIT 1",
            params![input.admission_receipt_ref],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((admitted_identity, connection_id, state)) = admission else {
        return Err(MemoryError::WorkClaimIncompatibleState(
            "ACP attachment admission receipt is missing".to_string(),
        ));
    };
    if connection_id != input.host_identity
        || admitted_identity != input.agent_identity_id
        || !matches!(state.as_str(), "self_asserted" | "verified")
    {
        return Err(MemoryError::WorkClaimTransitionRefused {
            reason: crate::error::WorkClaimTransitionReason::HolderMismatch,
            claim_id: input.work_claim_id.clone(),
            holder_identity_id: admitted_identity,
            caller_identity_id: input.agent_identity_id.clone(),
        });
    }

    // As in the read-only preflight above, preserve missing-row versus SQL
    // NULL so a stored null grant fails closed with the typed refusal.
    let capability_json: Option<Option<String>> = tx
        .query_row(
            "SELECT capability_json FROM agent_identities WHERE agent_identity_id = ?1",
            params![input.agent_identity_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    let Some(capability_json) = capability_json else {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "unknown agent identity {}",
            input.agent_identity_id
        )));
    };

    let claim: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT agent_identity_id, state, transition_version
             FROM session_claims WHERE claim_id = ?1",
            params![input.work_claim_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((claim_identity, state, transition_version)) = claim else {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "WorkClaim {} is unavailable",
            input.work_claim_id
        )));
    };
    if claim_identity != input.agent_identity_id {
        return Err(MemoryError::WorkClaimTransitionRefused {
            reason: crate::error::WorkClaimTransitionReason::HolderMismatch,
            claim_id: input.work_claim_id.clone(),
            holder_identity_id: claim_identity,
            caller_identity_id: input.agent_identity_id.clone(),
        });
    }
    if state != "active" {
        return Err(MemoryError::WorkClaimIncompatibleState(format!(
            "WorkClaim {} is {state}, not active",
            input.work_claim_id
        )));
    }
    if transition_version != input.expected_transition_version {
        return Err(MemoryError::WorkClaimConflict(format!(
            "WorkClaim {} transition revision is {}, expected {}",
            input.work_claim_id, transition_version, input.expected_transition_version
        )));
    }
    let authorization = authorization_from_capability_json(
        capability_json.as_deref(),
        &input.agent_identity_id,
        &input.tool_profile,
        &input.capability_class,
    )?;
    if authorization.policy_digest != input.policy_digest {
        return Err(MemoryError::WorkClaimConflict(
            "ACP attachment policy digest does not match the admitted AgentIdentity grant"
                .to_string(),
        ));
    }
    Ok(authorization)
}

/// Admit an attachment atomically. Every refusal above the INSERT occurs in
/// this transaction, so missing/stale/rejected/unavailable evidence leaves no
/// attachment row behind. Exact replay returns the original row without
/// updating timestamps or any other bytes.
pub fn attach_harness_session(
    conn: &mut Connection,
    input: &NewHarnessSessionAttachment,
) -> Result<HarnessSessionAttachmentReceipt, MemoryError> {
    validate_new_attachment(input)?;
    // Serialize natural-key/idempotency writers.  A deferred transaction lets
    // two writers both observe "no row" and turns the loser into a raw UNIQUE
    // error; IMMEDIATE makes the second writer re-read the committed row and
    // produce the typed replay/conflict result deterministically.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    if let Some(existing) = find_by_idempotency_key(&tx, &input.idempotency_key)? {
        if !same_binding(&existing, input) {
            return Err(MemoryError::WorkClaimConflict(format!(
                "ACP attachment idempotency key {} conflicts with its original binding",
                input.idempotency_key
            )));
        }

        // An idempotency key is not an admission grant: exact replay still
        // revalidates the current admission, identity policy, and exact active
        // WorkClaim revision before returning the immutable prior receipt.
        verify_admission_and_claim(&tx, input)?;
        tx.commit()?;
        return Ok(HarnessSessionAttachmentReceipt {
            attachment: existing,
            admission: HarnessSessionAttachmentAdmission::Replayed,
        });
    }

    // New attachments derive policy from durable identity/claim evidence in
    // the same write transaction before either a natural-key conflict or an
    // INSERT can disclose or mutate attachment state.
    verify_admission_and_claim(&tx, input)?;

    if let Some(existing) = find_by_natural_key(&tx, input)? {
        return Err(MemoryError::WorkClaimConflict(format!(
            "ACP session binding is already attached as {}",
            existing.attachment_id
        )));
    }

    let now = normalize_utc_iso_or_now("");
    let attachment_id = format!("attachment-{}", uuid::Uuid::new_v4());
    tx.execute(
        "INSERT INTO harness_session_attachments (
            attachment_id, host_identity, protocol_version,
            adapter_connection_identity, remote_session_id, work_claim_id,
            expected_transition_version, agent_identity_id, contract_digest,
            capabilities_json, tool_profile, capability_class, policy_digest,
            descriptor_digest, idempotency_key, admission_receipt_ref, state,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                   ?13, ?14, ?15, ?16, 'attached', ?17, ?17)",
        params![
            attachment_id,
            input.host_identity,
            input.protocol_version,
            input.adapter_connection_identity,
            input.remote_session_id,
            input.work_claim_id,
            input.expected_transition_version,
            input.agent_identity_id,
            input.contract_digest,
            input.capabilities_json,
            input.tool_profile,
            input.capability_class,
            input.policy_digest,
            input.descriptor_digest,
            input.idempotency_key,
            input.admission_receipt_ref,
            now,
        ],
    )?;
    let attachment = tx.query_row(
        &format!(
            "SELECT {SELECT_COLUMNS} FROM harness_session_attachments WHERE attachment_id = ?1"
        ),
        params![attachment_id],
        row_to_attachment,
    )?;
    tx.commit()?;
    Ok(HarnessSessionAttachmentReceipt {
        attachment,
        admission: HarnessSessionAttachmentAdmission::Created,
    })
}

/// Read-only attachment lookup. It never discovers or controls an ACP
/// process, and it never updates a receipt's timestamp/state.
pub fn get_harness_session_attachment(
    conn: &Connection,
    selector: &HarnessSessionAttachmentSelector,
) -> Result<Option<HarnessSessionAttachment>, MemoryError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM harness_session_attachments WHERE {}",
        match selector {
            HarnessSessionAttachmentSelector::AttachmentId(_) => "attachment_id = ?1",
            HarnessSessionAttachmentSelector::NaturalKey { .. } => {
                "host_identity = ?1 AND protocol_version = ?2
                 AND adapter_connection_identity = ?3 AND remote_session_id = ?4"
            }
        }
    );
    let result = match selector {
        HarnessSessionAttachmentSelector::AttachmentId(attachment_id) => conn
            .query_row(&sql, params![attachment_id], row_to_attachment)
            .optional()?,
        HarnessSessionAttachmentSelector::NaturalKey {
            host_identity,
            protocol_version,
            adapter_connection_identity,
            remote_session_id,
        } => conn
            .query_row(
                &sql,
                params![
                    host_identity,
                    protocol_version,
                    adapter_connection_identity,
                    remote_session_id
                ],
                row_to_attachment,
            )
            .optional()?,
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_schema;
    use crate::db::session_claims::{
        insert_agent_identity, insert_work_claim, AgentIdentity, NewWorkClaim, WorkClaimMode,
    };

    fn seed(conn: &mut Connection) -> NewHarnessSessionAttachment {
        insert_agent_identity(
            conn,
            &AgentIdentity {
                agent_identity_id: "agent-1".into(),
                display_name: None,
                seat: None,
                capability_json: Some(
                    r#"{"acp":{"tool_profiles":["standard"],"capability_classes":["tachi"]}}"#
                        .to_string(),
                ),
                created_at: String::new(),
            },
        )
        .unwrap();
        conn.execute(
            "INSERT INTO identity_admissions
             (admission_id, agent_identity_id, connection_id, state, created_at)
             VALUES ('admission-1', 'agent-1', 'host-1', 'self_asserted', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        insert_work_claim(
            conn,
            &NewWorkClaim {
                claim_id: "claim-1".into(),
                agent_identity_id: "agent-1".into(),
                session_client: Some("host-1".into()),
                issue_ref: None,
                flow_id: None,
                dispatch_id: None,
                branch: "branch".into(),
                worktree_path: "/tmp/claim".into(),
                declared_file_scope: "[\"src/lib.rs\"]".into(),
                role: "executor".into(),
                mode: WorkClaimMode::Writable,
                expected_head: "head".into(),
                lease_expires_at: "2030-01-01T00:00:00Z".into(),
                created_at: String::new(),
            },
        )
        .unwrap();
        let mut input = NewHarnessSessionAttachment {
            host_identity: "host-1".into(),
            protocol_version: "1".into(),
            adapter_connection_identity: "adapter-1".into(),
            remote_session_id: "remote-1".into(),
            work_claim_id: "claim-1".into(),
            expected_transition_version: 0,
            agent_identity_id: "agent-1".into(),
            contract_digest: "contract-digest".into(),
            capabilities_json: serde_json::to_string(&HarnessSessionAttachmentCapabilities {
                observe: true,
                resume: true,
                ..Default::default()
            })
            .unwrap(),
            tool_profile: "standard".into(),
            capability_class: "tachi".into(),
            policy_digest: "policy-digest".into(),
            descriptor_digest: "descriptor-digest".into(),
            idempotency_key: "idempotency-1".into(),
            admission_receipt_ref: "admission-1".into(),
        };
        input.policy_digest = authorization_from_capability_json(
            Some(r#"{"acp":{"tool_profiles":["standard"],"capability_classes":["tachi"]}}"#),
            "agent-1",
            "standard",
            "tachi",
        )
        .unwrap()
        .policy_digest;
        input
    }

    fn setup() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn attachment_row_bytes(conn: &Connection) -> String {
        conn.query_row(
            "SELECT json_object(
                'attachment_id', attachment_id,
                'host_identity', host_identity,
                'protocol_version', protocol_version,
                'adapter_connection_identity', adapter_connection_identity,
                'remote_session_id', remote_session_id,
                'work_claim_id', work_claim_id,
                'expected_transition_version', expected_transition_version,
                'agent_identity_id', agent_identity_id,
                'contract_digest', contract_digest,
                'capabilities_json', capabilities_json,
                'tool_profile', tool_profile,
                'capability_class', capability_class,
                'policy_digest', policy_digest,
                'descriptor_digest', descriptor_digest,
                'idempotency_key', idempotency_key,
                'admission_receipt_ref', admission_receipt_ref,
                'state', state,
                'created_at', created_at,
                'updated_at', updated_at
            ) FROM harness_session_attachments
             ORDER BY attachment_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn exact_replay_is_byte_stable_and_does_not_add_a_row() {
        let mut conn = setup();
        let input = seed(&mut conn);
        let first = attach_harness_session(&mut conn, &input).unwrap();
        let before = (
            conn.query_row(
                "SELECT COUNT(*) FROM harness_session_attachments",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            attachment_row_bytes(&conn),
        );
        let replay = attach_harness_session(&mut conn, &input).unwrap();
        let after = (
            conn.query_row(
                "SELECT COUNT(*) FROM harness_session_attachments",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            attachment_row_bytes(&conn),
        );
        assert_eq!(
            replay.admission,
            HarnessSessionAttachmentAdmission::Replayed
        );
        assert_eq!(
            replay.attachment.attachment_id,
            first.attachment.attachment_id
        );
        assert_eq!(before, after);
    }

    #[test]
    fn changed_binding_conflicts_without_mutating_original_row() {
        let mut conn = setup();
        let input = seed(&mut conn);
        let first = attach_harness_session(&mut conn, &input).unwrap();
        let before = attachment_row_bytes(&conn);
        let mut changed = input.clone();
        changed.remote_session_id = "remote-2".into();
        let error = attach_harness_session(&mut conn, &changed).unwrap_err();
        assert!(matches!(error, MemoryError::WorkClaimConflict(_)));
        let after = attachment_row_bytes(&conn);
        assert_eq!(before, after);
        assert_eq!(first.attachment.remote_session_id, "remote-1");
    }

    #[test]
    fn replay_revalidates_released_claim_after_initial_attachment() {
        let mut conn = setup();
        let input = seed(&mut conn);
        attach_harness_session(&mut conn, &input).unwrap();
        let before = attachment_row_bytes(&conn);
        conn.execute_batch(
            "UPDATE session_claims
             SET state = 'released', transition_version = transition_version + 1
             WHERE claim_id = 'claim-1';",
        )
        .unwrap();

        let error = attach_harness_session(&mut conn, &input).unwrap_err();
        assert!(error.to_string().contains("not active"), "{error}");
        assert_eq!(before, attachment_row_bytes(&conn));
    }

    #[test]
    fn replay_revalidates_revoked_identity_grant_after_initial_attachment() {
        let mut conn = setup();
        let input = seed(&mut conn);
        attach_harness_session(&mut conn, &input).unwrap();
        let before = attachment_row_bytes(&conn);
        conn.execute(
            "UPDATE agent_identities
             SET capability_json = NULL
             WHERE agent_identity_id = 'agent-1'",
            [],
        )
        .unwrap();

        let error = attach_harness_session(&mut conn, &input).unwrap_err();
        assert!(
            error.to_string().contains("no ACP capability grant"),
            "{error}"
        );
        assert_eq!(before, attachment_row_bytes(&conn));
    }

    #[test]
    fn stale_claim_refuses_before_insert() {
        let mut conn = setup();
        let mut input = seed(&mut conn);
        input.expected_transition_version = 1;
        let error = attach_harness_session(&mut conn, &input).unwrap_err();
        assert!(matches!(error, MemoryError::WorkClaimConflict(_)));
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM harness_session_attachments",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn changed_identity_grant_refuses_before_insert_when_policy_digest_drifts() {
        let mut conn = setup();
        let input = seed(&mut conn);
        conn.execute(
            "UPDATE agent_identities
             SET capability_json = ?1 WHERE agent_identity_id = 'agent-1'",
            [r#"{"acp":{"tool_profiles":["observe","standard"],"capability_classes":["tachi"]}}"#],
        )
        .unwrap();

        let error = attach_harness_session(&mut conn, &input).unwrap_err();
        assert!(error.to_string().contains("policy digest"));
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM harness_session_attachments",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn persisted_attachment_schema_is_provider_neutral_and_has_no_vendor_fields() {
        let conn = setup();
        let table_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'harness_session_attachments'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let schema = table_sql.to_ascii_lowercase();
        for forbidden in ["zeroclaw", "codex", "claude", "provider", "model"] {
            assert!(
                !schema.contains(forbidden),
                "provider-specific field {forbidden:?} leaked into the attachment schema"
            );
        }
    }
}
