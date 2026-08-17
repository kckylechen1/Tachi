//! Atomic, Rust-only mutation boundary for replacements that claim a
//! supersession edge and then perform dependent writes.
//!
//! This is intentionally not a generic SQL or arbitrary transaction API.
//! Callers can only claim an unset supersession edge, upsert a memory, archive
//! a claimed source, add a graph edge, or save a derived item. A failed claim
//! or any later mutation error drops the `BEGIN IMMEDIATE` transaction and
//! rolls every earlier mutation back.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;

use crate::{
    db,
    error::MemoryError,
    types::{ExpectedMemoryState, MemoryEdge, MemoryEntry},
    MemoryStore,
};

/// tachi#1645 (#1635 finding 2): bound the `superseded_by` chain walk
/// `claim_immutable_supersession` performs before installing a new edge. A
/// legitimate lineage should never need anywhere close to this many hops;
/// hitting the cap is treated as a refusal (see
/// `refuse_supersession_cycle`), not an unbounded scan.
const MAX_SUPERSESSION_CHAIN_WALK: u32 = 32;

/// Route token stamped on every [`SupersessionReceipt`] from
/// [`ImmutableSupersessionTransaction::claim_immutable_supersession`].
pub const SUPERSESSION_ROUTE_IMMUTABLE_CLAIM: &str = "immutable_claim_v1";
/// Durable receipt event kind for checked supersession claims (tachi#1671).
pub const SUPERSESSION_RECEIPT_EVENT_TYPE: &str = "memory.supersession.receipt.v1";
/// Write-once hard-state namespace that exists in every memcore profile.
pub const SUPERSESSION_RECEIPT_NAMESPACE: &str = "memory_supersession_receipts";

/// The typed reason a checked supersession claim refused before writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupersessionErrorKind {
    SelfSupersession,
    SourceMissing,
    TargetMissing,
    SourceArchived,
    SourceProtected,
    TargetIneligible,
    SourceDrift,
    TargetDrift,
    CycleDetected,
    TraversalCapExceeded,
    CompetingTarget,
    PriorReceiptMissing,
    CasLost,
}

impl fmt::Display for SupersessionErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let token = match self {
            Self::SelfSupersession => "self_supersession",
            Self::SourceMissing => "source_missing",
            Self::TargetMissing => "target_missing",
            Self::SourceArchived => "source_archived",
            Self::SourceProtected => "source_protected",
            Self::TargetIneligible => "target_ineligible",
            Self::SourceDrift => "source_drift",
            Self::TargetDrift => "target_drift",
            Self::CycleDetected => "cycle_detected",
            Self::TraversalCapExceeded => "traversal_cap_exceeded",
            Self::CompetingTarget => "competing_target",
            Self::PriorReceiptMissing => "prior_receipt_missing",
            Self::CasLost => "cas_lost",
        };
        formatter.write_str(token)
    }
}

/// Typed supersession refusal. It converts to [`MemoryError`] only at legacy
/// boundaries; the primitive itself exposes structured reason tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersessionError {
    pub kind: SupersessionErrorKind,
    pub source_id: String,
    pub target_id: String,
    pub detail: String,
}

impl SupersessionError {
    fn new(
        kind: SupersessionErrorKind,
        source_id: &str,
        target_id: &str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for SupersessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "immutable supersession refused ({}): {} -> {}: {}",
            self.kind, self.source_id, self.target_id, self.detail
        )
    }
}

impl std::error::Error for SupersessionError {}

impl From<SupersessionError> for MemoryError {
    fn from(error: SupersessionError) -> Self {
        MemoryError::InvalidArg(error.to_string())
    }
}

/// Explicit result of attempting to install a checked supersession edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupersessionCommitResult {
    Applied,
    PriorIdempotent,
}

impl SupersessionCommitResult {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::PriorIdempotent => "prior_idempotent",
        }
    }
}

/// Caller-bound expected state for source/target rows.
#[derive(Debug, Clone)]
pub struct SupersessionExpectedState {
    pub source: ExpectedMemoryState,
    pub target: Option<ExpectedMemoryState>,
}

impl SupersessionExpectedState {
    pub fn active_unsuperseded(source: &MemoryEntry, target: Option<&MemoryEntry>) -> Self {
        Self {
            source: ExpectedMemoryState::from_entry(source, None),
            target: target.map(|entry| ExpectedMemoryState::from_entry(entry, None)),
        }
    }
}

/// Durable facts for one successful immutable claim (tachi#1671).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersessionReceipt {
    pub receipt_id: String,
    pub source_id: String,
    pub target_id: String,
    pub source_revision_before: i64,
    pub source_revision_after: i64,
    pub target_revision_before: Option<i64>,
    pub target_revision_after: Option<i64>,
    pub source_archived_before: bool,
    pub source_archived_after: bool,
    pub source_superseded_by_before: Option<String>,
    pub source_superseded_by_after: Option<String>,
    pub source_valid_until_before: Option<String>,
    pub source_valid_until_after: Option<String>,
    pub source_path_before: String,
    pub source_text_digest_before: String,
    pub target_path_before: Option<String>,
    pub target_text_digest_before: Option<String>,
    /// [`None`] on a generic store; private partitions carry their admitted id.
    pub partition_id: Option<String>,
    pub route: String,
    pub policy_version: String,
    pub dependent_write_disposition: String,
    pub commit_result: SupersessionCommitResult,
    pub durable: bool,
}

impl SupersessionReceipt {
    pub fn id_for(route: &str, policy_version: &str, source_id: &str, target_id: &str) -> String {
        receipt_id(route, policy_version, source_id, target_id)
    }
}

/// Transaction-local result of one semantic supersession claim attempt.
///
/// A replayed same-edge claim returns [`SupersessionCommitResult::PriorIdempotent`]
/// here while carrying the original stored [`SupersessionReceipt`] unchanged.
/// The durable receipt is write-once state; replay/no-op status belongs to the
/// attempt envelope, not to a mutated clone of the stored receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersessionClaimOutcome {
    pub result: SupersessionCommitResult,
    pub receipt: SupersessionReceipt,
}

impl SupersessionClaimOutcome {
    pub fn is_applied(&self) -> bool {
        self.result == SupersessionCommitResult::Applied
    }

    pub fn is_prior_idempotent(&self) -> bool {
        self.result == SupersessionCommitResult::PriorIdempotent
    }
}

#[derive(Debug, Clone)]
pub struct SupersessionClaimOptions<'a> {
    pub route: &'a str,
    pub policy_version: &'a str,
    pub expected: Option<&'a SupersessionExpectedState>,
    pub require_materialized_target: bool,
    pub archive_source: bool,
    pub enforce_lifecycle_source_protection: bool,
    pub partition_id: Option<String>,
}

/// Narrow mutation handle passed only inside
/// [`MemoryStore::with_immutable_supersession_transaction`].
pub struct ImmutableSupersessionTransaction<'tx> {
    tx: Transaction<'tx>,
    vec_available: bool,
    reserved_reference_write: db::ReservedReferenceWriteFlag,
    partition_id: Option<String>,
    pending_receipts: Vec<SupersessionReceipt>,
    claimed_source_ids: HashSet<String>,
}

fn text_digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn receipt_id(route: &str, policy_version: &str, source_id: &str, target_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(route.as_bytes());
    hasher.update(b"\0");
    hasher.update(policy_version.as_bytes());
    hasher.update(b"\0");
    hasher.update(source_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(target_id.as_bytes());
    format!("supersession:{:x}", hasher.finalize())
}

fn read_entry_and_supersession(
    tx: &Connection,
    id: &str,
) -> Result<Option<(MemoryEntry, Option<String>)>, MemoryError> {
    let ids = vec![id.to_string()];
    let mut entries = db::fetch_by_ids(tx, &ids, true)?;
    let Some(entry) = entries.remove(id) else {
        return Ok(None);
    };
    let superseded_by = tx
        .query_row(
            "SELECT superseded_by FROM memories WHERE id = ?1",
            [id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(Some((entry, superseded_by)))
}

fn target_is_retired(entry: &MemoryEntry, superseded_by: Option<&str>) -> bool {
    entry.archived || superseded_by.is_some()
}

fn refuse_supersession_cycle_within_tx(
    tx: &Connection,
    source_id: &str,
    target_id: &str,
) -> Result<(), SupersessionError> {
    let mut current = target_id.to_string();
    for _ in 0..MAX_SUPERSESSION_CHAIN_WALK {
        let next: Option<String> = tx
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = ?1",
                [current.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|error| {
                SupersessionError::new(
                    SupersessionErrorKind::TraversalCapExceeded,
                    source_id,
                    target_id,
                    error.to_string(),
                )
            })?
            .flatten();
        match next {
            None => return Ok(()),
            Some(next_id) if next_id == source_id => {
                return Err(SupersessionError::new(
                    SupersessionErrorKind::CycleDetected,
                    source_id,
                    target_id,
                    format!("would close a superseded_by cycle back to {source_id}"),
                ));
            }
            Some(next_id) => current = next_id,
        }
    }
    Err(SupersessionError::new(
        SupersessionErrorKind::TraversalCapExceeded,
        source_id,
        target_id,
        format!("superseded_by chain did not terminate within {MAX_SUPERSESSION_CHAIN_WALK} hops"),
    ))
}

fn load_supersession_receipt(
    tx: &Connection,
    receipt_id: &str,
) -> Result<Option<SupersessionReceipt>, MemoryError> {
    let value_json = tx
        .query_row(
            "SELECT value_json FROM hard_state WHERE namespace = ?1 AND key = ?2",
            params![SUPERSESSION_RECEIPT_NAMESPACE, receipt_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value_json
        .map(|value| serde_json::from_str(&value).map_err(MemoryError::from))
        .transpose()
}

fn supersession_event_semantic_fields(
    receipt: &SupersessionReceipt,
) -> Result<Vec<String>, MemoryError> {
    Ok(vec![
        String::new(),
        "memcore".to_string(),
        String::new(),
        "memory".to_string(),
        String::new(),
        "memcore".to_string(),
        SUPERSESSION_RECEIPT_EVENT_TYPE.to_string(),
        "structural".to_string(),
        serde_json::to_string(&vec![
            "memories.superseded_by",
            "memories.valid_until",
            "memories.revision",
            "dependent.transaction",
        ])?,
        serde_json::to_string(&vec!["supersession_receipt"])?,
        serde_json::to_string(receipt)?,
        serde_json::to_string(&serde_json::json!({
            "producer": "memcore::immutable_supersession",
            "route": receipt.route,
            "policy_version": receipt.policy_version,
        }))?,
    ])
}

fn load_supersession_event_semantic_fields(
    tx: &Connection,
    receipt_id: &str,
) -> Result<Option<Vec<String>>, MemoryError> {
    tx.query_row(
        "SELECT source_repo, adapter, project, domain, session_id, actor,
                event_type, authority, effects, projection_hints,
                payload_json, provenance_json
         FROM tachi_events WHERE id = ?1",
        [receipt_id],
        |row| {
            let mut fields = Vec::with_capacity(12);
            for index in 0..12 {
                fields.push(row.get::<_, String>(index)?);
            }
            Ok(fields)
        },
    )
    .optional()
    .map_err(MemoryError::from)
}

pub(crate) fn replace_supersession_event_receipt_after_seal(
    tx: &Connection,
    before: &SupersessionReceipt,
    after: &SupersessionReceipt,
) -> Result<(), MemoryError> {
    let has_events = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'tachi_events')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !has_events {
        return Ok(());
    }
    let expected_before = supersession_event_semantic_fields(before)?;
    if load_supersession_event_semantic_fields(tx, &before.receipt_id)?.as_ref()
        != Some(&expected_before)
    {
        return Err(MemoryError::Internal(format!(
            "supersession event identity conflict before seal: {}",
            before.receipt_id
        )));
    }
    let changed = tx.execute(
        "UPDATE tachi_events SET payload_json = ?1 WHERE id = ?2 AND payload_json = ?3",
        params![
            serde_json::to_string(after)?,
            before.receipt_id,
            serde_json::to_string(before)?,
        ],
    )?;
    if changed != 1
        || load_supersession_event_semantic_fields(tx, &after.receipt_id)?.as_ref()
            != Some(&supersession_event_semantic_fields(after)?)
    {
        return Err(MemoryError::Internal(format!(
            "supersession event durability transition failed: {}",
            after.receipt_id
        )));
    }
    Ok(())
}

fn persist_supersession_receipt(
    tx: &Connection,
    receipt: &mut SupersessionReceipt,
) -> Result<(), MemoryError> {
    // A generic database commit is itself the durable authority. A private
    // partition is still only an in-memory working image here; its snapshot
    // copy flips this bit only when constructing the successfully sealed
    // envelope.
    receipt.durable = receipt.partition_id.is_none();
    let value_json = serde_json::to_string(receipt)?;
    let created_at = db::now_utc_iso();
    let changed = tx.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(namespace, key) DO NOTHING",
        params![
            SUPERSESSION_RECEIPT_NAMESPACE,
            receipt.receipt_id,
            value_json,
            created_at,
        ],
    )?;
    if changed == 0 {
        let existing = load_supersession_receipt(tx, &receipt.receipt_id)?.ok_or_else(|| {
            MemoryError::Internal(format!(
                "supersession receipt conflict without stored row: {}",
                receipt.receipt_id
            ))
        })?;
        if existing != *receipt {
            return Err(MemoryError::Internal(format!(
                "supersession receipt identity conflict: {}",
                receipt.receipt_id
            )));
        }
    }

    let has_events = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'tachi_events')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !has_events {
        return Ok(());
    }
    let event_fields = supersession_event_semantic_fields(receipt)?;
    let event_changed = tx.execute(
        "INSERT INTO tachi_events (
            id, source_repo, adapter, project, domain, session_id, actor,
            event_type, authority, effects, projection_hints,
            payload_json, provenance_json, created_at
         )
         VALUES (?1, '', 'memcore', '', 'memory', '', 'memcore',
                 ?2, 'structural', ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO NOTHING",
        params![
            receipt.receipt_id,
            SUPERSESSION_RECEIPT_EVENT_TYPE,
            event_fields[8],
            event_fields[9],
            event_fields[10],
            event_fields[11],
            created_at,
        ],
    )?;
    if event_changed == 0
        && load_supersession_event_semantic_fields(tx, &receipt.receipt_id)?.as_ref()
            != Some(&event_fields)
    {
        return Err(MemoryError::Internal(format!(
            "supersession event identity conflict: {}",
            receipt.receipt_id
        )));
    }
    Ok(())
}

pub(crate) fn finalize_supersession_receipt_within_tx(
    tx: &Connection,
    receipt: &mut SupersessionReceipt,
    dependent_write_disposition: &str,
) -> Result<(), MemoryError> {
    let Some((source_after, source_superseded_by_after)) =
        read_entry_and_supersession(tx, &receipt.source_id)?
    else {
        return Err(MemoryError::Internal(format!(
            "supersession receipt source disappeared before commit: {}",
            receipt.source_id
        )));
    };
    let Some((target_after, target_superseded_by_after)) =
        read_entry_and_supersession(tx, &receipt.target_id)?
    else {
        return Err(MemoryError::InvalidArg(format!(
            "supersession receipt target was not materialized before commit: {}",
            receipt.target_id
        )));
    };
    if target_is_retired(&target_after, target_superseded_by_after.as_deref()) {
        return Err(MemoryError::InvalidArg(format!(
            "supersession receipt target became ineligible before commit: {}",
            receipt.target_id
        )));
    }
    if source_superseded_by_after.as_deref() != Some(receipt.target_id.as_str())
        || (receipt.source_archived_after && !source_after.archived)
        || source_after.path != receipt.source_path_before
        || text_digest(&source_after.text) != receipt.source_text_digest_before
    {
        return Err(MemoryError::InvalidArg(format!(
            "supersession receipt source changed after claim: {}",
            receipt.source_id
        )));
    }
    receipt.source_revision_after = source_after.revision;
    receipt.source_archived_after = source_after.archived;
    receipt.source_superseded_by_after = source_superseded_by_after;
    receipt.source_valid_until_after = source_after.valid_until;
    receipt.target_revision_after = Some(target_after.revision);
    receipt.dependent_write_disposition = dependent_write_disposition.to_string();
    persist_supersession_receipt(tx, receipt)
}

pub(crate) fn claim_supersession_edge_within_tx(
    tx: &Connection,
    source_id: &str,
    target_id: &str,
    options: SupersessionClaimOptions<'_>,
) -> Result<SupersessionClaimOutcome, SupersessionError> {
    if source_id == target_id {
        return Err(SupersessionError::new(
            SupersessionErrorKind::SelfSupersession,
            source_id,
            target_id,
            "source and target must be distinct",
        ));
    }

    let (source_before, source_superseded_by_before) = read_entry_and_supersession(tx, source_id)
        .map_err(|error| {
            SupersessionError::new(
                SupersessionErrorKind::SourceMissing,
                source_id,
                target_id,
                error.to_string(),
            )
        })?
        .ok_or_else(|| {
            SupersessionError::new(
                SupersessionErrorKind::SourceMissing,
                source_id,
                target_id,
                "source row is not materialized in the admitted partition",
            )
        })?;

    if source_superseded_by_before.as_deref() == Some(target_id) {
        let id = receipt_id(options.route, options.policy_version, source_id, target_id);
        let prior = load_supersession_receipt(tx, &id)
            .map_err(|error| {
                SupersessionError::new(
                    SupersessionErrorKind::PriorReceiptMissing,
                    source_id,
                    target_id,
                    error.to_string(),
                )
            })?
            .ok_or_else(|| {
                SupersessionError::new(
                    SupersessionErrorKind::PriorReceiptMissing,
                    source_id,
                    target_id,
                    "same edge exists without its durable receipt",
                )
            })?;
        return Ok(SupersessionClaimOutcome {
            result: SupersessionCommitResult::PriorIdempotent,
            receipt: prior,
        });
    }

    if let Some(expected) = options.expected {
        if !expected
            .source
            .matches(&source_before, source_superseded_by_before.as_deref())
        {
            return Err(SupersessionError::new(
                SupersessionErrorKind::SourceDrift,
                source_id,
                target_id,
                "source state no longer matches the caller-bound snapshot",
            ));
        }
    }

    if source_before.archived {
        return Err(SupersessionError::new(
            SupersessionErrorKind::SourceArchived,
            source_id,
            target_id,
            "source is archived",
        ));
    }
    if options.enforce_lifecycle_source_protection {
        if let Some(reason) =
            crate::store::memory_lifecycle::lifecycle_protection_reason(&source_before)
        {
            return Err(SupersessionError::new(
                SupersessionErrorKind::SourceProtected,
                source_id,
                target_id,
                format!("source is lifecycle-protected ({reason})"),
            ));
        }
    }

    if let Some(existing_target) = source_superseded_by_before.as_deref() {
        return Err(SupersessionError::new(
            SupersessionErrorKind::CompetingTarget,
            source_id,
            target_id,
            format!("source already points at {existing_target}"),
        ));
    }

    let target_state = read_entry_and_supersession(tx, target_id).map_err(|error| {
        SupersessionError::new(
            SupersessionErrorKind::TargetMissing,
            source_id,
            target_id,
            error.to_string(),
        )
    })?;
    if target_state.is_none() && options.require_materialized_target {
        return Err(SupersessionError::new(
            SupersessionErrorKind::TargetMissing,
            source_id,
            target_id,
            "target row is not materialized in the admitted partition",
        ));
    }

    if let Some((target_before, target_superseded_by_before)) = target_state.as_ref() {
        if let Some(expected) = options
            .expected
            .and_then(|expected| expected.target.as_ref())
        {
            if !expected.matches(target_before, target_superseded_by_before.as_deref()) {
                return Err(SupersessionError::new(
                    SupersessionErrorKind::TargetDrift,
                    source_id,
                    target_id,
                    "target state no longer matches the caller-bound snapshot",
                ));
            }
        }
        refuse_supersession_cycle_within_tx(tx, source_id, target_id)?;
        if target_is_retired(target_before, target_superseded_by_before.as_deref()) {
            return Err(SupersessionError::new(
                SupersessionErrorKind::TargetIneligible,
                source_id,
                target_id,
                "target is archived or already superseded",
            ));
        }
    } else if options
        .expected
        .and_then(|expected| expected.target.as_ref())
        .is_some()
    {
        return Err(SupersessionError::new(
            SupersessionErrorKind::TargetDrift,
            source_id,
            target_id,
            "expected target disappeared",
        ));
    }

    if target_state.is_none() {
        refuse_supersession_cycle_within_tx(tx, source_id, target_id)?;
    }
    db::refuse_retired_sticky_row_within_tx(tx, source_id, "superseded").map_err(|error| {
        SupersessionError::new(
            SupersessionErrorKind::SourceDrift,
            source_id,
            target_id,
            error.to_string(),
        )
    })?;
    db::refuse_retired_sticky_row_within_tx(tx, target_id, "used as a supersession target")
        .map_err(|error| {
            SupersessionError::new(
                SupersessionErrorKind::TargetDrift,
                source_id,
                target_id,
                error.to_string(),
            )
        })?;

    let now = db::now_utc_iso();
    let changed = tx
        .execute(
            "UPDATE memories
             SET superseded_by = ?1, archived = CASE WHEN ?2 THEN 1 ELSE archived END,
                 updated_at = ?3, revision = revision + 1,
                 valid_until = COALESCE(valid_until, ?3)
             WHERE id = ?4 AND revision = ?5 AND archived = ?6 AND superseded_by IS NULL",
            params![
                target_id,
                options.archive_source,
                now,
                source_id,
                source_before.revision,
                source_before.archived,
            ],
        )
        .map_err(|error| {
            SupersessionError::new(
                SupersessionErrorKind::CasLost,
                source_id,
                target_id,
                error.to_string(),
            )
        })?;
    if changed == 0 {
        return Err(SupersessionError::new(
            SupersessionErrorKind::CasLost,
            source_id,
            target_id,
            "source CAS lost",
        ));
    }
    let (source_after, source_superseded_by_after) = read_entry_and_supersession(tx, source_id)
        .map_err(|error| {
            SupersessionError::new(
                SupersessionErrorKind::CasLost,
                source_id,
                target_id,
                error.to_string(),
            )
        })?
        .ok_or_else(|| {
            SupersessionError::new(
                SupersessionErrorKind::CasLost,
                source_id,
                target_id,
                "source disappeared after update",
            )
        })?;
    let target_after = read_entry_and_supersession(tx, target_id).map_err(|error| {
        SupersessionError::new(
            SupersessionErrorKind::TargetMissing,
            source_id,
            target_id,
            error.to_string(),
        )
    })?;
    let (target_revision_before, target_path_before, target_text_digest_before) = target_state
        .as_ref()
        .map(|(entry, _)| {
            (
                Some(entry.revision),
                Some(entry.path.clone()),
                Some(text_digest(&entry.text)),
            )
        })
        .unwrap_or((None, None, None));
    let target_revision_after = target_after.as_ref().map(|(entry, _)| entry.revision);

    Ok(SupersessionClaimOutcome {
        result: SupersessionCommitResult::Applied,
        receipt: SupersessionReceipt {
            receipt_id: receipt_id(options.route, options.policy_version, source_id, target_id),
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            source_revision_before: source_before.revision,
            source_revision_after: source_after.revision,
            target_revision_before,
            target_revision_after,
            source_archived_before: source_before.archived,
            source_archived_after: source_after.archived,
            source_superseded_by_before,
            source_superseded_by_after,
            source_valid_until_before: source_before.valid_until,
            source_valid_until_after: source_after.valid_until,
            source_path_before: source_before.path,
            source_text_digest_before: text_digest(&source_before.text),
            target_path_before,
            target_text_digest_before,
            partition_id: options.partition_id,
            route: options.route.to_string(),
            policy_version: options.policy_version.to_string(),
            dependent_write_disposition: "pending_transaction".to_string(),
            commit_result: SupersessionCommitResult::Applied,
            durable: false,
        },
    })
}

impl<'tx> ImmutableSupersessionTransaction<'tx> {
    fn validate_memory_write(entry: &MemoryEntry) -> Result<(), MemoryError> {
        crate::path_router::validate_retired_sticky_write(&entry.path, &entry.category)
            .map_err(|error| MemoryError::InvalidArg(error.to_string()))
    }

    fn finalize_pending_receipts(&mut self) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        for receipt in &mut self.pending_receipts {
            finalize_supersession_receipt_within_tx(&self.tx, receipt, "transaction_committed")?;
        }
        Ok(())
    }

    fn refuse_claimed_source_rewrite(&self, source_id: &str) -> Result<(), MemoryError> {
        if self.claimed_source_ids.contains(source_id) {
            return Err(MemoryError::InvalidArg(format!(
                "claimed supersession source is immutable in this transaction: {source_id}"
            )));
        }
        Ok(())
    }

    /// Claim `source_id -> target_id` exactly once.
    ///
    /// A same-edge replay returns [`SupersessionCommitResult::PriorIdempotent`];
    /// a conflicting edge fails loudly. Callers can therefore skip duplicate
    /// side effects without receiving a provisional receipt that overstates
    /// durability before this transaction commits.
    ///
    /// tachi#1645 (#1635 findings 1+2) caller audit: every non-test caller of
    /// this method as of this change points `target_id` at either (a) a row
    /// that does not exist yet and gets materialized later in the SAME
    /// `BEGIN IMMEDIATE` transaction, or (b) a row just freshly
    /// inserted/verified active earlier in the same transaction — never at
    /// an already-retired row a caller *intends* to keep superseding onto.
    /// `refuse_ineligible_supersession_target`/`refuse_supersession_cycle`
    /// were therefore safe to make load-bearing here with no caller-side
    /// STOP:
    /// - `wiki_ops/ingest.rs:729` (`persist_wiki_ingest_entry`) — target is
    ///   `replacement_entry.id`, upserted AFTER the claim loop, in-flight (b
    ///   above, materialize-later case).
    /// - `facade_memory_ops/consolidate_ops.rs:486,548`
    ///   (`apply_lifecycle_action`'s "supersede"/"merge_into"/
    ///   "near_dup_merge" arms) — target is caller-supplied and, pre-#1645,
    ///   was NEVER eligibility-checked; this IS the gap findings 1/2 close,
    ///   not a caller that needs special-casing.
    /// - `memory_search_ops/save_memory/persist.rs:351`
    ///   (wiki-projection dedup) — target is `winner_id`, either the entry
    ///   just upserted in this same transaction or the pre-existing active
    ///   winner `list_all_wiki_duplicate_candidates` resolved; `candidate`s
    ///   being folded in are filtered `candidate.id != winner_id`.
    /// - `foundry_runtime_ops/daily_distill/persist.rs`
    ///   (`claim_distilled_sources`) uses the stricter checked claim; its
    ///   target is already materialized by the same transaction.
    ///
    /// Two adjacent modules do NOT call this method at all, so findings 1/2
    /// do not reach them: `foundry_runtime_ops/wiki_evolver.rs` (REM draft
    /// occupancy) enforces its own `memory_is_active_unsuperseded` checks
    /// without ever installing a `superseded_by` edge here, and
    /// `store/rem.rs` uses raw SQL state checks plus the unguarded
    /// `MemoryStore::supersede_memory` — same path the read-side
    /// `stored_supersession_cycle_still_fails_content_free_end_to_end` test
    /// (memcore `recall_coverage_tests.rs`) seeds its cycle through, which is
    /// why that test is untouched and unaffected by the cycle guard added
    /// here.
    pub fn claim_immutable_supersession(
        &mut self,
        source_id: &str,
        target_id: &str,
    ) -> Result<SupersessionCommitResult, MemoryError> {
        self.try_claim_immutable_supersession(source_id, target_id)
            .map_err(Into::into)
    }

    pub fn try_claim_immutable_supersession(
        &mut self,
        source_id: &str,
        target_id: &str,
    ) -> Result<SupersessionCommitResult, SupersessionError> {
        let _authorization = db::authorize_reserved_reference_write(&self.reserved_reference_write)
            .map_err(|error| {
                SupersessionError::new(
                    SupersessionErrorKind::SourceDrift,
                    source_id,
                    target_id,
                    error.to_string(),
                )
            })?;
        // Cycle guard first: it gives the more specific diagnosis when a
        // target is already-superseded *and* the chain closes back onto
        // `source_id` (see `claim_immutable_supersession_permits_a_two_hop_cycle_finding`'s
        // successor test below). Eligibility second: it catches every other
        // way a target can be retired (archived, or superseded by something
        // that does NOT lead back to `source_id`).
        //
        // tachi#1671 route decision: this is the semantic mutation route.
        // Raw `db::supersede_memory*` seams are test-support gated; production
        // callers cannot bypass this semantic route. Partition identity is the
        // handle's admitted stamp (#1668), never `MemoryEntry::scope` or a path
        // label.
        let receipt = claim_supersession_edge_within_tx(
            &self.tx,
            source_id,
            target_id,
            SupersessionClaimOptions {
                route: SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
                policy_version: SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
                expected: None,
                require_materialized_target: false,
                archive_source: false,
                enforce_lifecycle_source_protection: false,
                partition_id: self.partition_id.clone(),
            },
        )?;
        let result = receipt.result;
        if result == SupersessionCommitResult::Applied {
            self.claimed_source_ids
                .insert(receipt.receipt.source_id.clone());
            self.pending_receipts.push(receipt.receipt);
        }
        Ok(result)
    }

    pub fn claim_checked_immutable_supersession(
        &mut self,
        source_id: &str,
        target_id: &str,
        expected: &SupersessionExpectedState,
        route: &str,
        policy_version: &str,
        enforce_lifecycle_source_protection: bool,
    ) -> Result<SupersessionCommitResult, MemoryError> {
        let _authorization = db::authorize_reserved_reference_write(&self.reserved_reference_write)
            .map_err(|error| {
                SupersessionError::new(
                    SupersessionErrorKind::SourceDrift,
                    source_id,
                    target_id,
                    error.to_string(),
                )
            })?;
        let outcome = claim_supersession_edge_within_tx(
            &self.tx,
            source_id,
            target_id,
            SupersessionClaimOptions {
                route,
                policy_version,
                expected: Some(expected),
                require_materialized_target: true,
                archive_source: false,
                enforce_lifecycle_source_protection,
                partition_id: self.partition_id.clone(),
            },
        )
        .map_err(MemoryError::from)?;
        let result = outcome.result;
        if outcome.is_applied() {
            self.claimed_source_ids
                .insert(outcome.receipt.source_id.clone());
            self.pending_receipts.push(outcome.receipt);
        }
        Ok(result)
    }

    pub fn claim_and_archive_immutable_supersession(
        &mut self,
        source_id: &str,
        target_id: &str,
        expected: Option<&SupersessionExpectedState>,
        route: &str,
        policy_version: &str,
    ) -> Result<SupersessionClaimOutcome, MemoryError> {
        let _authorization = db::authorize_reserved_reference_write(&self.reserved_reference_write)
            .map_err(|error| {
                SupersessionError::new(
                    SupersessionErrorKind::SourceDrift,
                    source_id,
                    target_id,
                    error.to_string(),
                )
            })?;
        let receipt = claim_supersession_edge_within_tx(
            &self.tx,
            source_id,
            target_id,
            SupersessionClaimOptions {
                route,
                policy_version,
                expected,
                require_materialized_target: true,
                archive_source: true,
                enforce_lifecycle_source_protection: true,
                partition_id: self.partition_id.clone(),
            },
        )
        .map_err(MemoryError::from)?;
        if receipt.is_applied() {
            self.claimed_source_ids
                .insert(receipt.receipt.source_id.clone());
            self.pending_receipts.push(receipt.receipt.clone());
        }
        Ok(receipt)
    }

    /// Finalize and durably persist the exact receipt returned by a semantic
    /// route after all promised dependent writes have succeeded in this same
    /// transaction. Callers invoke this only for an `Applied` attempt; a
    /// prior-idempotent attempt already refers to the original durable receipt
    /// and must perform no dependent write.
    pub fn finalize_supersession_receipt(
        &mut self,
        receipt: &mut SupersessionReceipt,
        dependent_write_disposition: &str,
    ) -> Result<(), MemoryError> {
        debug_assert_eq!(receipt.commit_result, SupersessionCommitResult::Applied);
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        finalize_supersession_receipt_within_tx(&self.tx, receipt, dependent_write_disposition)?;
        self.pending_receipts
            .retain(|pending| pending.receipt_id != receipt.receipt_id);
        Ok(())
    }

    /// Persist an entry inside the replacement transaction.
    pub fn upsert(&mut self, entry: &MemoryEntry) -> Result<(), MemoryError> {
        Self::validate_memory_write(entry)?;
        self.refuse_claimed_source_rewrite(&entry.id)?;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::upsert_within_tx(&self.tx, entry, self.vec_available, None).map(|_| ())
    }

    /// Claim a caller-stable memory id without rewriting an existing winner.
    ///
    /// The existence check and insert run under this transaction's
    /// `BEGIN IMMEDIATE` writer lock. An `Existing` result performs no main,
    /// FTS, vector, graph, derived, archive, or supersession mutation; callers
    /// must return from the operation before invoking any other method.
    pub fn insert_if_absent(
        &mut self,
        entry: &MemoryEntry,
    ) -> Result<db::InsertMemoryResult, MemoryError> {
        Self::validate_memory_write(entry)?;
        self.refuse_claimed_source_rewrite(&entry.id)?;
        if crate::namespace::is_reserved_wiki_rem_id(&entry.id) {
            return Err(MemoryError::InvalidArg(format!(
                "id '{}' is in the reserved 'wiki-rem:' namespace; use insert_rem_operation_if_absent",
                entry.id
            )));
        }
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::insert_if_absent_within_tx(&self.tx, entry, self.vec_available)
    }

    /// Persist a canonical weekly Wiki REM operation inside the same source-
    /// claim transaction. This is the only insert seam that accepts the
    /// reserved `wiki-rem:` id namespace and its producer-owned metadata.
    pub fn insert_rem_operation_if_absent(
        &mut self,
        entry: &MemoryEntry,
    ) -> Result<db::InsertMemoryResult, MemoryError> {
        Self::validate_memory_write(entry)?;
        self.refuse_claimed_source_rewrite(&entry.id)?;
        let rem_string = |key: &str| {
            entry
                .metadata
                .pointer(&format!("/rem/{key}"))
                .and_then(Value::as_str)
        };
        if !entry.id.starts_with("wiki-rem:")
            || entry.source != "wiki"
            || !entry.path.starts_with("/wiki/drafts/")
            || rem_string("producer") != Some("weekly_wiki_evolver")
            || rem_string("operation_id") != Some(entry.id.as_str())
            || rem_string("operation_status") != Some("pending_sources")
        {
            return Err(MemoryError::InvalidArg(format!(
                "invalid canonical Wiki REM operation entry: {}",
                entry.id
            )));
        }
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::insert_rem_operation_if_absent_within_tx(&self.tx, entry, self.vec_available)
    }

    /// Read a memory from the same transaction, including archived rows.
    pub fn get_memory(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        let ids = vec![id.to_string()];
        let mut entries = db::fetch_by_ids(&self.tx, &ids, true)?;
        Ok(entries.remove(id))
    }

    /// Check that a deterministic-id occupant is still an active canonical
    /// row. `get_memory` intentionally includes archived rows and therefore
    /// cannot answer the supersession half of this invariant by itself.
    pub fn memory_is_active_unsuperseded(&self, id: &str) -> Result<bool, MemoryError> {
        let count = self.tx.query_row(
            "SELECT COUNT(*) FROM memories WHERE id = ?1 AND archived = 0 AND superseded_by IS NULL",
            [id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count == 1)
    }

    /// Persist an entry with server-authorized, shape-validated reference
    /// appends inside the same replacement transaction.
    pub fn upsert_with_validated_reference_mutations(
        &mut self,
        entry: &MemoryEntry,
        metadata_patch: &Map<String, Value>,
        mutations: &[db::ValidatedReferenceMutation],
    ) -> Result<(), MemoryError> {
        Self::validate_memory_write(entry)?;
        self.refuse_claimed_source_rewrite(&entry.id)?;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        // A replacement transaction has already selected its canonical target.
        // Generic Jaccard merging here could insert that target as somebody
        // else's loser after the predecessor claim succeeded, leaving no active
        // winner for the requested projection.
        db::upsert_with_validated_reference_mutations_within_tx_and_metadata_removals(
            &self.tx,
            entry,
            self.vec_available,
            None,
            metadata_patch,
            &[],
            mutations,
            db::NearDuplicatePolicy::NonSemantic,
        )
        .map(|_| ())
    }

    /// Claim one physical REM source in the shared Wiki coordination store.
    ///
    /// Same-draft replay is accepted only when the canonical serialized source
    /// identity also matches. A competing draft or deterministic-key occupant
    /// aborts the surrounding draft transaction.
    pub fn claim_rem_source(
        &mut self,
        source_key: &str,
        source_identity: &str,
        draft_id: &str,
        claimed_at: &str,
    ) -> Result<(), MemoryError> {
        self.tx.execute(
            "INSERT INTO rem_source_claims (source_key, source_identity, draft_id, claimed_at) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(source_key) DO NOTHING",
            rusqlite::params![source_key, source_identity, draft_id, claimed_at],
        )?;
        let occupant = self.tx.query_row(
            "SELECT source_identity, draft_id FROM rem_source_claims WHERE source_key = ?1",
            [source_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        if occupant != (source_identity.to_string(), draft_id.to_string()) {
            return Err(MemoryError::InvalidArg(format!(
                "REM source claim conflict for {source_key}: owned by {}",
                occupant.1
            )));
        }
        Ok(())
    }

    /// Validate that a draft owns exactly the expected REM source claim ledger.
    ///
    /// The comparison is against `(source_key, source_identity)` rows sorted by
    /// the database's canonical key order, so a missing source, extra source,
    /// or identity drift fails before the caller treats the draft as complete.
    pub fn validate_rem_source_claims_for_draft(
        &self,
        draft_id: &str,
        expected_claims: &[(String, String)],
    ) -> Result<(), MemoryError> {
        let mut stmt = self.tx.prepare(
            "SELECT source_key, source_identity FROM rem_source_claims WHERE draft_id = ?1 \
             ORDER BY source_key ASC, source_identity ASC",
        )?;
        let actual_claims = stmt
            .query_map([draft_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut expected_claims = expected_claims.to_vec();
        expected_claims.sort();
        if actual_claims != expected_claims {
            return Err(MemoryError::InvalidArg(format!(
                "REM source claim ledger mismatch for {draft_id}: expected {} claims, found {}",
                expected_claims.len(),
                actual_claims.len()
            )));
        }
        Ok(())
    }

    /// Persist an entry while applying trusted metadata removals and validated
    /// reference mutations inside this transaction.
    ///
    /// This is the transactional counterpart of `MemoryStore`'s ordinary save
    /// seam. It exists so a domain projection can make the canonical row and
    /// its dependent graph/lifecycle mutations one commit boundary without
    /// exposing the raw SQLite transaction.
    pub fn upsert_with_validated_reference_mutations_and_metadata_removals(
        &mut self,
        entry: &MemoryEntry,
        idless_identity: Option<&str>,
        metadata_patch: &Map<String, Value>,
        metadata_removals: &[&str],
        mutations: &[db::ValidatedReferenceMutation],
        policy: db::NearDuplicatePolicy,
    ) -> Result<(db::IdlessUpsertResult, Value), MemoryError> {
        Self::validate_memory_write(entry)?;
        self.refuse_claimed_source_rewrite(&entry.id)?;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::upsert_with_validated_reference_mutations_within_tx_and_metadata_removals(
            &self.tx,
            entry,
            self.vec_available,
            idless_identity,
            metadata_patch,
            metadata_removals,
            mutations,
            policy,
        )
    }

    /// Read active Wiki/Guide candidates from the same writer snapshot used
    /// for a projection mutation.
    pub fn list_all_wiki_duplicate_candidates(
        &self,
        path: &str,
        topic: &str,
        parent_path: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_wiki_duplicate_candidates(&self.tx, path, topic, parent_path, None)
    }

    /// Read the active Wiki/Guide winner for an exact path from this
    /// transaction's writer snapshot.
    pub fn find_active_wiki_entry_by_path(
        &self,
        path: &str,
    ) -> Result<Option<MemoryEntry>, MemoryError> {
        db::find_active_wiki_entry_by_path(&self.tx, path)
    }

    /// Read every active predecessor covered by Wiki ingest's legacy
    /// replacement identity from the same writer snapshot as the mutation.
    pub fn list_active_wiki_ingest_predecessors(
        &self,
        path: &str,
        topic: &str,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_active_wiki_ingest_predecessors(&self.tx, path, topic)
    }

    /// Archive a source after its supersession claim has succeeded.
    pub fn archive_claimed_source(&mut self, source_id: &str) -> Result<(), MemoryError> {
        if !self
            .pending_receipts
            .iter()
            .any(|receipt| receipt.source_id == source_id)
        {
            return Err(MemoryError::InvalidArg(format!(
                "archive claimed source refused: no applied supersession claim for {source_id} in this transaction"
            )));
        }
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let already_archived = self.tx.query_row(
            "SELECT archived FROM memories WHERE id = ?1",
            [source_id],
            |row| row.get::<_, bool>(0),
        )?;
        if already_archived {
            return Ok(());
        }
        if !db::archive_memory_within_tx(&self.tx, source_id)? {
            return Err(MemoryError::InvalidArg(format!(
                "archive claimed source failed for {source_id}"
            )));
        }
        Ok(())
    }

    /// Write a provenance edge inside the replacement transaction.
    pub fn add_edge(&mut self, edge: &MemoryEdge) -> Result<(), MemoryError> {
        db::add_edge(&self.tx, edge)
    }

    /// [`Self::add_edge`] plus an explicit authority classification
    /// (tachi#1646) for the appended `edge_observations` row.
    pub fn add_edge_with_provenance(
        &mut self,
        edge: &MemoryEdge,
        provenance: &db::EdgeProvenance,
    ) -> Result<(), MemoryError> {
        db::add_edge_with_provenance(&self.tx, edge, provenance)
    }

    /// Record a durable outbox event for an object this transaction has
    /// already written (tachi#1643).
    ///
    /// This is how a multi-write replacement gets #1630's atomicity guarantee
    /// without collapsing into
    /// [`MemoryStore::commit_with_outbox_event`], which owns its own
    /// transaction and therefore cannot be nested inside this one: the event
    /// commits with the supersession claim, the archive, the edges and the
    /// projections, or none of them do.
    ///
    /// The payload digest is computed from the object as this transaction now
    /// holds it, so ordering matters — call this *after* the write whose
    /// result the event should announce. An `object_id` this transaction has
    /// not written is a typed [`MemoryError::NotFound`], which is the same
    /// refusal that makes "no event without its object" enforceable at the
    /// simple seam.
    ///
    /// No reserved-reference authorization is taken here: the enclosing
    /// operation already holds it, that guard is a non-reentrant
    /// compare-and-swap, and the outbox table is not a reserved-reference
    /// surface.
    pub fn enqueue_outbox_event(
        &mut self,
        object_id: &str,
        event: &crate::store::outbox::OutboxEventMeta,
    ) -> Result<db::OutboxEventRow, MemoryError> {
        crate::store::outbox::enqueue_outbox_event_within_tx(&self.tx, object_id, event)
    }

    /// Save a caller-stable derived item inside the replacement transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn save_derived_with_id(
        &mut self,
        id: &str,
        text: &str,
        path: &str,
        summary: &str,
        importance: f64,
        source: &str,
        scope: &str,
        metadata: &serde_json::Value,
    ) -> Result<(), MemoryError> {
        self.refuse_claimed_source_rewrite(id)?;
        crate::path_router::validate_retired_sticky_write(path, "other")
            .map_err(|error| MemoryError::InvalidArg(error.to_string()))?;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::save_derived_with_id(
            &self.tx, id, text, path, summary, importance, source, scope, metadata,
        )
    }
}

impl MemoryStore {
    /// Run one replacement operation inside a `BEGIN IMMEDIATE` transaction.
    ///
    /// The closure receives no connection or SQL execution surface. It can
    /// only use [`ImmutableSupersessionTransaction`]'s fixed mutation methods.
    /// Returning an error, including a false immutable-edge claim, rolls the
    /// whole operation back.
    pub fn with_immutable_supersession_transaction<T>(
        &mut self,
        mut operation: impl FnMut(&mut ImmutableSupersessionTransaction<'_>) -> Result<T, MemoryError>,
    ) -> Result<T, MemoryError> {
        let db_label = self.db_label.clone();
        let reserved_reference_write = self.reserved_reference_write.clone();
        db::retry_memory_locked("immutable_supersession", &db_label, || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut replacement = ImmutableSupersessionTransaction {
                tx,
                vec_available: self.vec_available,
                reserved_reference_write: reserved_reference_write.clone(),
                partition_id: self
                    .admitted_partition
                    .as_ref()
                    .map(|part| part.partition_id.clone()),
                pending_receipts: Vec::new(),
                claimed_source_ids: HashSet::new(),
            };
            let result = operation(&mut replacement)?;
            replacement.finalize_pending_receipts()?;
            replacement.tx.commit()?;
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::types::Value as SqlValue;
    use serde_json::json;

    fn fixture_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/".to_string(),
            summary: String::new(),
            text: format!("body for {id}"),
            importance: 0.7,
            timestamp: "2026-08-04T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "fixture".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    /// Capture every user table as a comparable value snapshot. A retirement
    /// refusal must not merely leave the target row looking unchanged: it
    /// must leave all SQLite bytes represented by the ordinary store tables
    /// unchanged, including FTS/vector projections and metadata tables.
    fn database_snapshot(store: &MemoryStore) -> Vec<(String, Vec<Vec<String>>)> {
        let conn = store.connection();
        let table_names = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .expect("prepare table snapshot")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("list snapshot tables")
            .collect::<Result<Vec<_>, _>>()
            .expect("read snapshot tables");

        table_names
            .into_iter()
            .map(|table| {
                let quoted = format!("\"{}\"", table.replace('"', "\"\""));
                let mut stmt = conn
                    .prepare(&format!("SELECT * FROM {quoted}"))
                    .expect("prepare table contents snapshot");
                let column_count = stmt.column_count();
                let rows = stmt
                    .query_map([], |row| {
                        (0..column_count)
                            .map(|column| {
                                row.get::<_, SqlValue>(column)
                                    .map(|value| format!("{value:?}"))
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .expect("read table contents snapshot")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("collect table contents snapshot");
                (table, rows)
            })
            .collect()
    }

    fn retired_entry(path: &str, category: &str, id: &str) -> MemoryEntry {
        let mut entry = fixture_entry(id);
        entry.path = path.to_string();
        entry.category = category.to_string();
        entry
    }

    fn retire_existing_fixture(store: &MemoryStore, id: &str) {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize raw legacy sticky fixture");
        store
            .connection()
            .execute(
                "UPDATE memories SET path='/sticky/legacy', category='sticky' WHERE id=?1",
                [id],
            )
            .expect("turn ordinary seed into raw legacy sticky fixture");
    }

    fn assert_transaction_refuses_without_writes<F>(label: &str, entry: MemoryEntry, operation: F)
    where
        F: Fn(&mut ImmutableSupersessionTransaction<'_>, &MemoryEntry) -> Result<(), MemoryError>,
    {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let before = database_snapshot(&store);
        let before_changes = store.connection().total_changes();
        let error = store
            .with_immutable_supersession_transaction(|tx| operation(tx, &entry))
            .expect_err(label);
        assert!(
            error.to_string().contains("tachi_a2a"),
            "{label} must name its successor: {error}"
        );
        assert_eq!(
            store.connection().total_changes(),
            before_changes,
            "{label} must execute zero SQLite changes"
        );
        assert_eq!(
            database_snapshot(&store),
            before,
            "{label} refusal must leave every ordinary store table byte-identical"
        );
    }

    fn assert_transaction_family_refuses<F>(label: &str, operation: F)
    where
        F: Copy
            + Fn(&mut ImmutableSupersessionTransaction<'_>, &MemoryEntry) -> Result<(), MemoryError>,
    {
        for (variant, entry) in [
            (
                "retired path",
                retired_entry("//STICKY///legacy/", "fact", "retired-path"),
            ),
            (
                "retired category",
                retired_entry("/ordinary", " Sticky ", "retired-category"),
            ),
        ] {
            assert_transaction_refuses_without_writes(
                &format!("{label} must reject {variant}"),
                entry,
                operation,
            );
        }
    }

    #[test]
    fn immutable_transaction_upsert_rejects_retired_path_and_category() {
        assert_transaction_family_refuses("transaction upsert", |tx, entry| tx.upsert(entry));
    }

    #[test]
    fn immutable_transaction_insert_if_absent_rejects_retired_path_and_category() {
        assert_transaction_family_refuses("transaction insert_if_absent", |tx, entry| {
            tx.insert_if_absent(entry).map(|_| ())
        });
    }

    #[test]
    fn immutable_transaction_insert_rem_rejects_retired_path_and_category() {
        assert_transaction_family_refuses(
            "transaction insert_rem_operation_if_absent",
            |tx, entry| tx.insert_rem_operation_if_absent(entry).map(|_| ()),
        );
    }

    #[test]
    fn immutable_transaction_validated_reference_upsert_rejects_retired_path_and_category() {
        assert_transaction_family_refuses("transaction validated reference upsert", |tx, entry| {
            tx.upsert_with_validated_reference_mutations(entry, &Map::new(), &[])
        });
    }

    #[test]
    fn immutable_transaction_validated_reference_upsert_with_removals_rejects_retired_path_and_category(
    ) {
        assert_transaction_family_refuses(
            "transaction validated reference upsert with removals",
            |tx, entry| {
                tx.upsert_with_validated_reference_mutations_and_metadata_removals(
                    entry,
                    None,
                    &Map::new(),
                    &[],
                    &[],
                    db::NearDuplicatePolicy::NonSemantic,
                )
                .map(|_| ())
            },
        );
    }

    #[test]
    fn immutable_transaction_memory_writers_accept_normal_path_and_category() {
        let normal_entry = || fixture_entry("normal-transaction-writer");

        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .with_immutable_supersession_transaction(|tx| tx.upsert(&normal_entry()))
            .expect("normal transaction upsert");

        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .with_immutable_supersession_transaction(|tx| {
                tx.insert_if_absent(&normal_entry()).map(|_| ())
            })
            .expect("normal transaction insert_if_absent");

        let mut rem = normal_entry();
        rem.id = "wiki-rem:normal".to_string();
        rem.source = "wiki".to_string();
        rem.path = "/wiki/drafts/normal".to_string();
        rem.metadata = json!({
            "rem": {
                "producer": "weekly_wiki_evolver",
                "operation_id": "wiki-rem:normal",
                "operation_status": "pending_sources"
            }
        });
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .with_immutable_supersession_transaction(|tx| {
                tx.insert_rem_operation_if_absent(&rem).map(|_| ())
            })
            .expect("normal transaction insert_rem_operation_if_absent");

        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .with_immutable_supersession_transaction(|tx| {
                tx.upsert_with_validated_reference_mutations(&normal_entry(), &Map::new(), &[])
            })
            .expect("normal validated reference upsert");

        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .with_immutable_supersession_transaction(|tx| {
                tx.upsert_with_validated_reference_mutations_and_metadata_removals(
                    &normal_entry(),
                    None,
                    &Map::new(),
                    &[],
                    &[],
                    db::NearDuplicatePolicy::NonSemantic,
                )
                .map(|_| ())
            })
            .expect("normal validated reference upsert with removals");
    }

    #[test]
    fn immutable_supersession_rejects_retired_source_or_target_before_any_side_effect() {
        for retired_id in ["source", "target"] {
            let mut store = MemoryStore::open_in_memory().expect("open memory store");
            store
                .insert_if_absent(&fixture_entry("source"))
                .expect("seed source");
            store
                .insert_if_absent(&fixture_entry("target"))
                .expect("seed target");
            retire_existing_fixture(&store, retired_id);
            let before = database_snapshot(&store);
            let before_changes = store.connection().total_changes();

            let error = store
                .with_immutable_supersession_transaction(|operation| {
                    operation.claim_immutable_supersession("source", "target")?;
                    operation.archive_claimed_source("source")
                })
                .expect_err("retired source or target must refuse the whole transaction");
            assert!(error.to_string().contains("tachi_a2a"), "{error}");
            assert_eq!(store.connection().total_changes(), before_changes);
            assert_eq!(
                database_snapshot(&store),
                before,
                "{retired_id} retirement refusal must precede row, edge, and projection writes"
            );
        }
    }

    /// tachi#1635 (#1632 conformance, item 1): the transaction wrapper's
    /// `claim_immutable_supersession` must refuse source == target the same
    /// way the test-support raw CAS does — but here a self-edge becomes a
    /// typed refusal rather than a silent `Ok(false)`.
    #[test]
    fn claim_immutable_supersession_refuses_self_supersession() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("self-loop"))
            .expect("seed self-loop candidate");

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("self-loop", "self-loop")
            })
            .expect_err("source == target must refuse");
        assert!(
            error.to_string().contains("self_supersession"),
            "unexpected error: {error}"
        );

        let unsuperseded = store
            .with_immutable_supersession_transaction(|operation| {
                operation.memory_is_active_unsuperseded("self-loop")
            })
            .expect("read back after refused self-supersession");
        assert!(
            unsuperseded,
            "a refused self-supersession must leave the row active and unsuperseded"
        );
    }

    /// tachi#1671: applying the identical edge again returns an explicit
    /// prior-idempotent result backed by the first transaction's durable
    /// receipt. A competing target remains a typed refusal.
    #[test]
    fn claim_immutable_supersession_same_edge_replay_returns_prior_result() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("replay-source"))
            .expect("seed source");
        store
            .insert_if_absent(&fixture_entry("replay-target"))
            .expect("seed target");

        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("replay-source", "replay-target")
            })
            .expect("first claim must install the edge");

        let result = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("replay-source", "replay-target")
            })
            .expect("identical replay must return an explicit prior result");
        assert_eq!(result, SupersessionCommitResult::PriorIdempotent);
    }

    #[test]
    fn receipt_namespace_refuses_ttl_backfill_and_reaping() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("ttl-source"))
            .expect("seed source");
        store
            .insert_if_absent(&fixture_entry("ttl-target"))
            .expect("seed target");
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("ttl-source", "ttl-target")
            })
            .expect("commit supersession receipt");

        assert_eq!(
            store
                .backfill_missing_expires_at(
                    SUPERSESSION_RECEIPT_NAMESPACE,
                    "2000-01-01T00:00:00Z",
                    None,
                )
                .expect("protected namespace backfill is a no-op"),
            0
        );
        assert_eq!(
            store
                .reap_expired_state("2100-01-01T00:00:00Z")
                .expect("reap hard state"),
            0
        );
        assert_eq!(
            store
                .with_immutable_supersession_transaction(|operation| {
                    operation.claim_immutable_supersession("ttl-source", "ttl-target")
                })
                .expect("receipt survives TTL maintenance"),
            SupersessionCommitResult::PriorIdempotent
        );
    }

    #[test]
    fn transaction_refuses_to_archive_a_source_it_did_not_claim() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        for id in ["claimed-source", "claimed-target", "unrelated-source"] {
            store
                .insert_if_absent(&fixture_entry(id))
                .expect("seed transaction fixture");
        }
        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("claimed-source", "claimed-target")?;
                operation.archive_claimed_source("unrelated-source")
            })
            .expect_err("unrelated archive must roll the claim back");
        assert!(error.to_string().contains("no applied supersession claim"));
        for id in ["claimed-source", "unrelated-source"] {
            let entry = store
                .get(id)
                .expect("read fixture")
                .expect("fixture remains");
            assert!(!entry.archived);
            assert_eq!(
                store.supersession_target(id).expect("read target"),
                Some(None)
            );
        }
    }

    #[test]
    fn transaction_refuses_a_claim_whose_target_never_materializes() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("dangling-source"))
            .expect("seed source");
        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("dangling-source", "missing-target")
            })
            .expect_err("commit must refuse a dangling target");
        assert!(error.to_string().contains("not materialized before commit"));
        assert_eq!(
            store
                .supersession_target("dangling-source")
                .expect("read source after rollback"),
            Some(None)
        );
    }

    #[test]
    fn semantic_replay_returns_the_exact_stored_receipt_without_rewriting_it() {
        const ROUTE: &str = "test_semantic_replay";
        const POLICY: &str = "test_policy_v1";

        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let source = fixture_entry("semantic-replay-source");
        let target = fixture_entry("semantic-replay-target");
        store.insert_if_absent(&source).expect("seed source");
        store.insert_if_absent(&target).expect("seed target");

        let first = store
            .with_immutable_supersession_transaction(|operation| {
                let outcome = operation.claim_and_archive_immutable_supersession(
                    &source.id, &target.id, None, ROUTE, POLICY,
                )?;
                assert_eq!(outcome.result, SupersessionCommitResult::Applied);
                let mut receipt = outcome.receipt;
                operation.finalize_supersession_receipt(
                    &mut receipt,
                    "test_semantic_replay_committed",
                )?;
                Ok(receipt)
            })
            .expect("first semantic claim");
        let receipt_id = SupersessionReceipt::id_for(ROUTE, POLICY, &source.id, &target.id);
        let (stored_json, stored_version) = store
            .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .expect("read first receipt")
            .expect("first receipt exists");
        let stored: SupersessionReceipt =
            serde_json::from_str(&stored_json).expect("decode first receipt");
        assert_eq!(stored_version, 1);
        assert_eq!(stored, first);

        let replay = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_and_archive_immutable_supersession(
                    &source.id, &target.id, None, ROUTE, POLICY,
                )
            })
            .expect("same-edge semantic replay");
        assert_eq!(replay.result, SupersessionCommitResult::PriorIdempotent);
        assert_eq!(
            replay.receipt, stored,
            "attempt-local replay status must not mutate the write-once receipt clone"
        );
        let (stored_after_json, stored_after_version) = store
            .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .expect("read receipt after replay")
            .expect("receipt remains present");
        assert_eq!(stored_after_version, stored_version);
        assert_eq!(stored_after_json, stored_json);
    }

    #[test]
    fn semantic_lifecycle_claim_refuses_wiki_and_durable_sources_without_writes() {
        let mut wiki = fixture_entry("protected-wiki-source");
        wiki.category = "WiKi".to_string();
        let mut durable = fixture_entry("protected-durable-source");
        durable.retention_policy = Some("durable".to_string());

        for source in [wiki, durable] {
            let mut store = MemoryStore::open_in_memory().expect("open memory store");
            let target = fixture_entry(&format!("{}-target", source.id));
            store
                .insert_if_absent(&source)
                .expect("seed protected source");
            store.insert_if_absent(&target).expect("seed target");
            let before = database_snapshot(&store);
            let receipt_id = SupersessionReceipt::id_for(
                "test_protected_semantic_route",
                "test_policy_v1",
                &source.id,
                &target.id,
            );

            let error = store
                .with_immutable_supersession_transaction(|operation| {
                    operation.claim_and_archive_immutable_supersession(
                        &source.id,
                        &target.id,
                        None,
                        "test_protected_semantic_route",
                        "test_policy_v1",
                    )
                })
                .expect_err("protected semantic source must refuse");
            assert!(error.to_string().contains("source_protected"), "{error}");
            assert_eq!(database_snapshot(&store), before);
            assert!(
                store
                    .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
                    .expect("read receipt namespace")
                    .is_none(),
                "a protected-source refusal must not leave durable evidence"
            );
        }
    }

    #[test]
    fn legacy_same_edge_without_receipt_refuses_instead_of_forging_prior_result() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("legacy-source"))
            .expect("seed source");
        store
            .insert_if_absent(&fixture_entry("legacy-target"))
            .expect("seed target");
        assert!(store
            .supersede_memory("legacy-source", "legacy-target")
            .expect("seed legacy raw edge"));

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("legacy-source", "legacy-target")
            })
            .expect_err("an edge without durable evidence is not an idempotent prior result");
        assert!(
            error.to_string().contains("prior_receipt_missing"),
            "{error}"
        );
    }

    #[test]
    fn checked_supersession_refuses_target_drift_without_writes() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let source = fixture_entry("drift-source");
        let target = fixture_entry("drift-target");
        store.insert_if_absent(&source).expect("seed source");
        store.insert_if_absent(&target).expect("seed target");
        let source_snapshot = store.get(&source.id).expect("read source").expect("source");
        let target_snapshot = store.get(&target.id).expect("read target").expect("target");
        let expected = SupersessionExpectedState::active_unsuperseded(
            &source_snapshot,
            Some(&target_snapshot),
        );

        let mut drifted = target.clone();
        drifted.summary = "target changed after planning".to_string();
        store.upsert(&drifted).expect("drift target");
        let before = database_snapshot(&store);

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_and_archive_immutable_supersession(
                    "drift-source",
                    "drift-target",
                    Some(&expected),
                    "test_checked_route",
                    "test_policy_v1",
                )
            })
            .expect_err("target drift must refuse");
        assert!(error.to_string().contains("target_drift"), "{error}");
        assert_eq!(database_snapshot(&store), before);
    }

    #[test]
    fn dependent_failure_rolls_back_edge_and_durable_receipt() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let source = fixture_entry("rollback-source");
        let target = fixture_entry("rollback-target");
        store.insert_if_absent(&source).expect("seed source");
        store.insert_if_absent(&target).expect("seed target");
        let source_snapshot = store.get(&source.id).expect("read source").expect("source");
        let target_snapshot = store.get(&target.id).expect("read target").expect("target");
        let expected = SupersessionExpectedState::active_unsuperseded(
            &source_snapshot,
            Some(&target_snapshot),
        );
        let receipt_id = SupersessionReceipt::id_for(
            "test_rollback_route",
            "test_policy_v1",
            &source.id,
            &target.id,
        );

        let error = store
            .with_immutable_supersession_transaction::<()>(|operation| {
                operation.claim_and_archive_immutable_supersession(
                    &source.id,
                    &target.id,
                    Some(&expected),
                    "test_rollback_route",
                    "test_policy_v1",
                )?;
                Err(MemoryError::Internal(
                    "injected dependent write failure".to_string(),
                ))
            })
            .expect_err("dependent failure must abort the transaction");
        assert!(error
            .to_string()
            .contains("injected dependent write failure"));
        assert_eq!(
            store.supersession_target(&source.id).expect("read edge"),
            Some(None)
        );
        assert!(
            store
                .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
                .expect("read receipt state")
                .is_none(),
            "rolled-back lifecycle mutation must not leave a receipt"
        );
    }

    #[test]
    fn claimed_source_cannot_be_rewritten_or_unarchived_before_commit() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let source = fixture_entry("claimed-source-rewrite");
        let target = fixture_entry("claimed-source-target");
        store.insert_if_absent(&source).expect("seed source");
        store.insert_if_absent(&target).expect("seed target");
        let receipt_id = SupersessionReceipt::id_for(
            "test_claimed_source_immutable",
            "test_policy_v1",
            &source.id,
            &target.id,
        );

        let error = store
            .with_immutable_supersession_transaction::<()>(|operation| {
                let outcome = operation.claim_and_archive_immutable_supersession(
                    &source.id,
                    &target.id,
                    None,
                    "test_claimed_source_immutable",
                    "test_policy_v1",
                )?;
                assert_eq!(outcome.result, SupersessionCommitResult::Applied);
                let mut rewritten = source.clone();
                rewritten.text = "rewrite after immutable claim".to_string();
                rewritten.archived = false;
                operation.upsert(&rewritten)
            })
            .expect_err("a claimed source must be immutable until commit");
        assert!(
            error
                .to_string()
                .contains("claimed supersession source is immutable"),
            "{error}"
        );
        assert_eq!(
            store.supersession_target(&source.id).expect("read source"),
            Some(None),
            "refusal must roll back the claim"
        );
        assert!(
            store
                .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
                .expect("read receipt")
                .is_none(),
            "refusal must not leave durable evidence"
        );
    }

    #[test]
    fn durable_supersession_receipt_is_write_once_and_exact() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let source = fixture_entry("receipt-source");
        let target = fixture_entry("receipt-target");
        store.insert_if_absent(&source).expect("seed source");
        store.insert_if_absent(&target).expect("seed target");
        let result = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession(&source.id, &target.id)
            })
            .expect("claim and persist receipt");
        assert_eq!(result, SupersessionCommitResult::Applied);
        let receipt_id = SupersessionReceipt::id_for(
            SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
            SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
            &source.id,
            &target.id,
        );
        let (receipt_json, version) = store
            .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .expect("read receipt")
            .expect("receipt exists");
        let receipt: SupersessionReceipt =
            serde_json::from_str(&receipt_json).expect("deserialize receipt");
        assert!(receipt.durable);
        assert_eq!(receipt.source_revision_before, source.revision);
        assert_eq!(receipt.target_revision_before, Some(target.revision));
        assert_eq!(receipt.commit_result, SupersessionCommitResult::Applied);
        assert_eq!(version, 1);
        assert!(
            store
                .set_state(
                    SUPERSESSION_RECEIPT_NAMESPACE,
                    &receipt_id,
                    r#"{"forged":true}"#,
                )
                .is_err(),
            "general state API must not overwrite committed supersession evidence"
        );
        assert!(
            store
                .insert_state_if_absent(
                    SUPERSESSION_RECEIPT_NAMESPACE,
                    "forged-receipt",
                    r#"{"forged":true}"#,
                )
                .is_err(),
            "general state API must not forge committed supersession evidence"
        );
        assert!(
            store
                .delete_state(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
                .is_err(),
            "general state API must not delete committed supersession evidence"
        );
        assert!(
            store
                .connection()
                .execute(
                    "UPDATE hard_state SET value_json = '{\"forged\":true}'
                     WHERE namespace = ?1 AND key = ?2",
                    params![SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id],
                )
                .is_err(),
            "default admin connection must not overwrite receipt authority"
        );
        assert!(
            store
                .connection()
                .execute("DELETE FROM tachi_events WHERE id = ?1", [&receipt_id],)
                .is_err(),
            "default admin connection must not delete the receipt event projection"
        );
    }

    #[test]
    fn supersession_event_collision_checks_all_authority_fields() {
        let mut source_store = MemoryStore::open_in_memory().expect("open source store");
        let source = fixture_entry("event-collision-source");
        let target = fixture_entry("event-collision-target");
        source_store.insert_if_absent(&source).expect("seed source");
        source_store.insert_if_absent(&target).expect("seed target");
        source_store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession(&source.id, &target.id)
            })
            .expect("produce canonical receipt");
        let receipt_id = SupersessionReceipt::id_for(
            SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
            SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
            &source.id,
            &target.id,
        );
        let (receipt_json, _) = source_store
            .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .expect("read receipt")
            .expect("receipt exists");
        let mut receipt: SupersessionReceipt =
            serde_json::from_str(&receipt_json).expect("decode receipt");

        let mut store = MemoryStore::open_in_memory().expect("open collision store");
        store
            .insert_tachi_event(&crate::types::TachiEventRecord {
                id: receipt_id.clone(),
                source_repo: String::new(),
                adapter: "hostile-adapter".to_string(),
                project: String::new(),
                domain: "memory".to_string(),
                session_id: String::new(),
                actor: "hostile-actor".to_string(),
                event_type: SUPERSESSION_RECEIPT_EVENT_TYPE.to_string(),
                authority: crate::types::AuthorityLevel::RawFact,
                effects: Vec::new(),
                projection_hints: Vec::new(),
                payload: serde_json::to_value(&receipt).expect("encode payload"),
                provenance: serde_json::json!({"producer": "hostile"}),
                created_at: db::now_utc_iso(),
            })
            .expect("seed same-payload hostile event");
        let authorization = store.reserved_reference_write.clone();
        let _authorization =
            db::authorize_reserved_reference_write(&authorization).expect("authorize typed write");
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("begin collision transaction");
        let error = persist_supersession_receipt(&tx, &mut receipt)
            .expect_err("same payload with different authority fields must refuse");
        assert!(
            error
                .to_string()
                .contains("supersession event identity conflict"),
            "{error}"
        );
        tx.rollback().expect("rollback collision fixture");
    }

    /// tachi#1645 (#1635 finding 1, item 2 — flipped from finding-pin to
    /// enforcement assertion): `claim_immutable_supersession` ->
    /// `refuse_ineligible_supersession_target` now reads the TARGET's
    /// `archived`/`superseded_by` state before ever reaching
    /// `db::supersede_memory`'s source-only CAS, so a caller that omits its
    /// own target check (e.g. `apply_lifecycle_action`'s "supersede"/
    /// "merge_into" arms in
    /// `crates/tachi-server/src/facade_memory_ops/consolidate_ops.rs`, which
    /// only call `refuse_if_protected` on the SOURCE) can no longer point a
    /// fresh source at an already-archived, already-superseded target.
    #[test]
    fn claim_immutable_supersession_refuses_ineligible_target() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("ineligible-target"))
            .expect("seed target");
        store
            .insert_if_absent(&fixture_entry("ineligible-target-canonical"))
            .expect("seed target's own canonical replacement");
        store
            .insert_if_absent(&fixture_entry("source-onto-dead-target"))
            .expect("seed source");

        // The target is already archived AND already superseded before the
        // claim under test — it is neither active nor eligible.
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession(
                    "ineligible-target",
                    "ineligible-target-canonical",
                )?;
                operation.archive_claimed_source("ineligible-target")
            })
            .expect("pre-condition: target becomes archived+superseded");

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation
                    .claim_immutable_supersession("source-onto-dead-target", "ineligible-target")
            })
            .expect_err("superseding onto an archived/already-superseded target must refuse");
        assert!(
            error.to_string().contains("target_ineligible"),
            "unexpected error: {error}"
        );

        let unsuperseded = store
            .with_immutable_supersession_transaction(|operation| {
                operation.memory_is_active_unsuperseded("source-onto-dead-target")
            })
            .expect("read back after refused claim");
        assert!(
            unsuperseded,
            "a refused claim onto an ineligible target must leave the would-be \
             source active and unsuperseded"
        );
    }

    /// tachi#1645 (#1635 finding 1): a target that does not exist yet is not
    /// "retired" — Wiki ingest's `persist_wiki_ingest_entry` claims each
    /// predecessor onto its brand-new replacement id BEFORE upserting that
    /// row in the same transaction, and the eligibility guard must not break
    /// that ordering.
    #[test]
    fn claim_immutable_supersession_permits_a_not_yet_materialized_target() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("predecessor"))
            .expect("seed predecessor");

        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("predecessor", "not-yet-inserted")?;
                operation.upsert(&fixture_entry("not-yet-inserted"))
            })
            .expect("claim onto a target materialized later in the same transaction");
    }

    /// tachi#1645 (#1635 finding 2, item 3 — flipped from finding-pin to
    /// enforcement assertion): `claim_immutable_supersession` now walks the
    /// proposed target's `superseded_by` chain before installing a new edge,
    /// so A -> B then B -> A refuses instead of both committing.
    #[test]
    fn claim_immutable_supersession_refuses_a_two_hop_cycle() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .insert_if_absent(&fixture_entry("cycle-a"))
            .expect("seed a");
        store
            .insert_if_absent(&fixture_entry("cycle-b"))
            .expect("seed b");

        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("cycle-a", "cycle-b")
            })
            .expect("A -> B installs");

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("cycle-b", "cycle-a")
            })
            .expect_err("B -> A after A -> B must refuse (cycle)");
        assert!(
            error.to_string().contains("cycle"),
            "unexpected error: {error}"
        );
    }

    /// tachi#1645 (#1635 finding 2): the chain walk must catch a cycle that
    /// closes more than one hop past the immediate target — not just the
    /// two-hop case a bare target-eligibility check would also happen to
    /// catch (an already-superseded immediate target is refused by
    /// `refuse_ineligible_supersession_target` regardless of whether it
    /// leads back to `source_id`). A -> B -> C, then C -> A must refuse
    /// specifically because A's chain (A -> B -> C) reaches back to C.
    #[test]
    fn claim_immutable_supersession_refuses_a_deeper_chain_cycle() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        for id in ["chain-a", "chain-b", "chain-c"] {
            store
                .insert_if_absent(&fixture_entry(id))
                .expect("seed chain node");
        }
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("chain-a", "chain-b")
            })
            .expect("A -> B installs");
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("chain-b", "chain-c")
            })
            .expect("B -> C installs");

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("chain-c", "chain-a")
            })
            .expect_err("C -> A must refuse: A's chain (A -> B -> C) closes back to C");
        assert!(
            error.to_string().contains("cycle"),
            "unexpected error: {error}"
        );
    }

    /// tachi#1645 (#1635 finding 2): a chain walk that does not terminate
    /// within [`MAX_SUPERSESSION_CHAIN_WALK`] hops refuses with a distinct,
    /// differently-worded error than the cycle-found case above — even
    /// though `probe` never appears anywhere in the chain (so this is NOT a
    /// cycle, just an implausibly long lineage the walk refuses to keep
    /// scanning past the cap).
    #[test]
    fn claim_immutable_supersession_refuses_when_chain_walk_exceeds_depth_cap() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let chain_len = MAX_SUPERSESSION_CHAIN_WALK as usize + 8;
        let node_id = |i: usize| format!("cap-chain-{i}");
        for i in 0..=chain_len {
            store
                .insert_if_absent(&fixture_entry(&node_id(i)))
                .expect("seed cap-chain node");
        }
        for i in 0..chain_len {
            store
                .with_immutable_supersession_transaction(|operation| {
                    operation.claim_immutable_supersession(&node_id(i), &node_id(i + 1))
                })
                .unwrap_or_else(|error| {
                    panic!("{} -> {} installs: {error}", node_id(i), node_id(i + 1))
                });
        }
        store
            .insert_if_absent(&fixture_entry("probe"))
            .expect("seed probe (never part of the chain)");

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("probe", &node_id(0))
            })
            .expect_err(
                "a chain longer than the depth cap must refuse even though it is not a cycle",
            );
        let message = error.to_string();
        assert!(
            message.contains("did not terminate within"),
            "unexpected error: {message}"
        );
        assert!(
            !message.contains("would close a superseded_by cycle"),
            "cap exhaustion must not be reported as a cycle-found error: {message}"
        );
    }

    /// tachi#1671 / #1668: `MemoryEntry::scope` is **not** a partition.
    /// Two rows on one generic store may still supersede each other even
    /// when their scope labels differ — that is intentional. Cross-partition
    /// refusal is proven by two [`crate::private_partition::PrivatePartition`]
    /// handles (separate sealed files) in
    /// `private_partition::tests::cross_partition_claim_cannot_see_foreign_rows`.
    #[test]
    fn scope_labels_are_not_partition_authority() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let mut source = fixture_entry("partition-a-source");
        source.scope = "project:alpha".to_string();
        let mut target = fixture_entry("partition-b-target");
        target.scope = "project:beta".to_string();
        store
            .insert_if_absent(&source)
            .expect("seed alpha-scope source");
        store
            .insert_if_absent(&target)
            .expect("seed beta-scope target");

        let result = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("partition-a-source", "partition-b-target")
            })
            .expect("same-store scope labels are not a trust boundary");
        assert_eq!(result, SupersessionCommitResult::Applied);
        let receipt_id = SupersessionReceipt::id_for(
            SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
            SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
            "partition-a-source",
            "partition-b-target",
        );
        let (receipt_json, _) = store
            .get_state_kv(SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .expect("read durable receipt state")
            .expect("receipt must be persisted in every memcore profile");
        let receipt: SupersessionReceipt =
            serde_json::from_str(&receipt_json).expect("deserialize durable receipt");
        assert_eq!(receipt.route, SUPERSESSION_ROUTE_IMMUTABLE_CLAIM);
        assert!(receipt.partition_id.is_none());
        assert!(receipt.source_revision_after >= 2);
        assert!(receipt.durable);
    }

    #[test]
    fn rem_source_claim_is_insert_once_and_replay_safe() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_rem_source(
                    "rem-source:one",
                    r#"{"store":"physical","id":"one"}"#,
                    "draft-a",
                    "2026-07-31T00:00:00Z",
                )
            })
            .expect("first claim");
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_rem_source(
                    "rem-source:one",
                    r#"{"store":"physical","id":"one"}"#,
                    "draft-a",
                    "2026-07-31T00:00:01Z",
                )
            })
            .expect("same operation replay");

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_rem_source(
                    "rem-source:one",
                    r#"{"store":"physical","id":"one"}"#,
                    "draft-b",
                    "2026-07-31T00:00:02Z",
                )
            })
            .expect_err("competing draft must not steal the source");
        assert!(error.to_string().contains("REM source claim conflict"));
        let occupant: String = store
            .connection()
            .query_row(
                "SELECT draft_id FROM rem_source_claims WHERE source_key = 'rem-source:one'",
                [],
                |row| row.get(0),
            )
            .expect("read claim occupant");
        assert_eq!(occupant, "draft-a");
    }

    #[test]
    fn rem_source_claim_ledger_validation_is_exact_and_sorted() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_rem_source(
                    "rem-source:b",
                    r#"{"store":"physical","id":"b","revision":2}"#,
                    "draft-a",
                    "2026-07-31T00:00:00Z",
                )?;
                operation.claim_rem_source(
                    "rem-source:a",
                    r#"{"store":"physical","id":"a","revision":1}"#,
                    "draft-a",
                    "2026-07-31T00:00:00Z",
                )?;
                operation.claim_rem_source(
                    "rem-source:c",
                    r#"{"store":"physical","id":"c","revision":3}"#,
                    "draft-b",
                    "2026-07-31T00:00:00Z",
                )?;
                operation.validate_rem_source_claims_for_draft(
                    "draft-a",
                    &[
                        (
                            "rem-source:b".to_string(),
                            r#"{"store":"physical","id":"b","revision":2}"#.to_string(),
                        ),
                        (
                            "rem-source:a".to_string(),
                            r#"{"store":"physical","id":"a","revision":1}"#.to_string(),
                        ),
                    ],
                )
            })
            .expect("sorted exact ledger validates");

        let wrong_identity = store
            .with_immutable_supersession_transaction(|operation| {
                operation.validate_rem_source_claims_for_draft(
                    "draft-a",
                    &[
                        (
                            "rem-source:a".to_string(),
                            r#"{"store":"physical","id":"a","revision":999}"#.to_string(),
                        ),
                        (
                            "rem-source:b".to_string(),
                            r#"{"store":"physical","id":"b","revision":2}"#.to_string(),
                        ),
                    ],
                )
            })
            .expect_err("identity drift must fail exact ledger validation");
        assert!(wrong_identity.to_string().contains("ledger mismatch"));

        let missing_claim = store
            .with_immutable_supersession_transaction(|operation| {
                operation.validate_rem_source_claims_for_draft(
                    "draft-a",
                    &[(
                        "rem-source:a".to_string(),
                        r#"{"store":"physical","id":"a","revision":1}"#.to_string(),
                    )],
                )
            })
            .expect_err("missing expected source must fail exact ledger validation");
        assert!(missing_claim.to_string().contains("ledger mismatch"));
    }
}
