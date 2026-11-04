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

mod near_duplicate;
mod receipts;
use receipts::load_supersession_receipt;
#[cfg(test)]
use receipts::persist_supersession_receipt;
pub(crate) use receipts::{
    finalize_supersession_receipt_within_tx, replace_supersession_event_receipt_after_seal,
};
mod typed_records;

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
const SUPERSESSION_ROUTE_IDLESS_NEAR_DUPLICATE: &str = "idless_near_duplicate_v1";
const SUPERSESSION_POLICY_IDLESS_NEAR_DUPLICATE: &str = "idless-near-duplicate-v1";
/// Durable receipt event kind for checked supersession claims (tachi#1671).
pub const SUPERSESSION_RECEIPT_EVENT_TYPE: &str = "memory.supersession.receipt.v1";
/// Write-once hard-state namespace that exists in every memcore profile.
pub const SUPERSESSION_RECEIPT_NAMESPACE: &str = "memory_supersession_receipts";

/// Non-semantic R12 writes and candidate pairs prepared inside the same
/// immutable transaction that must claim their supersessions.
#[derive(Debug)]
pub struct R12MemoryHygienePreparation {
    pub promoted: usize,
    pub derived_inserted: usize,
    pub edges_inserted: usize,
    pub covered_pairs: Vec<(String, String)>,
    pub duplicate_pairs: Vec<(String, String)>,
}

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
    pub mutation_timestamp: Option<&'a str>,
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

    let now = options
        .mutation_timestamp
        .map(str::to_owned)
        .unwrap_or_else(db::now_utc_iso);
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
                mutation_timestamp: None,
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
        Ok(self
            .claim_checked_immutable_supersession_with_timestamp(
                source_id,
                target_id,
                expected,
                route,
                policy_version,
                enforce_lifecycle_source_protection,
                None,
            )?
            .result)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn claim_checked_immutable_supersession_at(
        &mut self,
        source_id: &str,
        target_id: &str,
        expected: &SupersessionExpectedState,
        route: &str,
        policy_version: &str,
        enforce_lifecycle_source_protection: bool,
        mutation_timestamp: &str,
    ) -> Result<SupersessionClaimOutcome, MemoryError> {
        self.claim_checked_immutable_supersession_with_timestamp(
            source_id,
            target_id,
            expected,
            route,
            policy_version,
            enforce_lifecycle_source_protection,
            Some(mutation_timestamp),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn claim_checked_immutable_supersession_with_timestamp(
        &mut self,
        source_id: &str,
        target_id: &str,
        expected: &SupersessionExpectedState,
        route: &str,
        policy_version: &str,
        enforce_lifecycle_source_protection: bool,
        mutation_timestamp: Option<&str>,
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
                mutation_timestamp,
                partition_id: self.partition_id.clone(),
            },
        )
        .map_err(MemoryError::from)?;
        if outcome.is_applied() {
            self.claimed_source_ids
                .insert(outcome.receipt.source_id.clone());
            self.pending_receipts.push(outcome.receipt.clone());
        }
        Ok(outcome)
    }

    pub fn claim_and_archive_immutable_supersession(
        &mut self,
        source_id: &str,
        target_id: &str,
        expected: Option<&SupersessionExpectedState>,
        route: &str,
        policy_version: &str,
    ) -> Result<SupersessionClaimOutcome, MemoryError> {
        self.claim_and_archive_immutable_supersession_with_protection(
            source_id,
            target_id,
            expected,
            route,
            policy_version,
            true,
            None,
        )
    }

    /// Exact dedupe is itself the lifecycle authority for ordinary duplicate
    /// rows, including legacy Wiki-path rows that are not reserved `wiki-rem:`
    /// operations. It keeps the canonical source/target/CAS/receipt path while
    /// deliberately opting out of the generic lifecycle-protection gate.
    pub(crate) fn claim_and_archive_immutable_supersession_for_exact_dedupe(
        &mut self,
        source_id: &str,
        target_id: &str,
        expected: Option<&SupersessionExpectedState>,
        route: &str,
        policy_version: &str,
        mutation_timestamp: &str,
    ) -> Result<SupersessionClaimOutcome, MemoryError> {
        self.claim_and_archive_immutable_supersession_with_protection(
            source_id,
            target_id,
            expected,
            route,
            policy_version,
            false,
            Some(mutation_timestamp),
        )
    }

    /// R12 has its own query-bound safe-raw eligibility policy: durable raw
    /// rows are eligible, while pinned/permanent, used, important, and
    /// lifecycle-reserved rows are excluded before this seam is called. Keep
    /// that repair policy distinct from the ordinary lifecycle-source guard,
    /// while retaining the canonical target check, expected-state drift check,
    /// source CAS, and durable receipt finalization below.
    pub fn claim_and_archive_immutable_supersession_for_r12(
        &mut self,
        source_id: &str,
        target_id: &str,
        expected: Option<&SupersessionExpectedState>,
    ) -> Result<SupersessionClaimOutcome, MemoryError> {
        let source = self
            .get_memory(source_id)?
            .ok_or_else(|| MemoryError::NotFound(source_id.to_string()))?;
        let retention = source
            .retention_policy
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if source.archived
            || source.tier != "raw"
            || source.source == "foundry_distill"
            || source.access_count != 0
            || source.recall_count != 0
            || source.importance >= 0.85
            || matches!(retention, "pinned" | "permanent")
        {
            return Err(MemoryError::InvalidArg(format!(
                "R12 source eligibility refused: {source_id}"
            )));
        }
        self.claim_and_archive_immutable_supersession_with_protection(
            source_id,
            target_id,
            expected,
            "r12_memory_hygiene_v1",
            "r12-memory-hygiene-v1",
            false,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn claim_and_archive_immutable_supersession_with_protection(
        &mut self,
        source_id: &str,
        target_id: &str,
        expected: Option<&SupersessionExpectedState>,
        route: &str,
        policy_version: &str,
        enforce_lifecycle_source_protection: bool,
        mutation_timestamp: Option<&str>,
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
                enforce_lifecycle_source_protection,
                mutation_timestamp,
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
        let pending_index = self
            .pending_receipts
            .iter()
            .position(|pending| pending.receipt_id == receipt.receipt_id)
            .ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "supersession receipt is not pending in this transaction: {}",
                    receipt.receipt_id
                ))
            })?;
        if self.pending_receipts[pending_index] != *receipt {
            return Err(MemoryError::InvalidArg(format!(
                "supersession receipt diverged from the canonical pending receipt: {}",
                receipt.receipt_id
            )));
        }
        finalize_supersession_receipt_within_tx(
            &self.tx,
            &mut self.pending_receipts[pending_index],
            dependent_write_disposition,
        )?;
        *receipt = self.pending_receipts.remove(pending_index);
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
        if matches!(policy, db::NearDuplicatePolicy::AllowNearDuplicateMerge) {
            return Err(MemoryError::InvalidArg(
                "near-duplicate semantic writes must use the canonical id-less supersession seam"
                    .to_string(),
            ));
        }
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

    /// Prepare R12's promotion and projection writes and select every
    /// supersession pair from this transaction's snapshot. The caller must
    /// claim the returned pairs before allowing the enclosing immutable
    /// transaction to commit.
    pub fn prepare_r12_memory_hygiene(
        &mut self,
    ) -> Result<R12MemoryHygienePreparation, MemoryError> {
        let all_ids = {
            let mut statement = self.tx.prepare("SELECT id FROM memories ORDER BY id")?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut retired_sticky = HashSet::new();
        for id in all_ids {
            match db::refuse_retired_sticky_row_within_tx(
                &self.tx,
                &id,
                "mutated or projected by memory hygiene",
            ) {
                Ok(()) => {}
                Err(MemoryError::InvalidArg(_)) => {
                    retired_sticky.insert(id);
                }
                Err(error) => return Err(error),
            }
        }

        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let promotion_ids = {
            let mut statement = self.tx.prepare(
                "SELECT id FROM memories
                 WHERE COALESCE(archived,0)=0
                   AND source='foundry_distill'
                   AND tier='raw'
                 ORDER BY id",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut promoted = 0usize;
        for id in promotion_ids {
            if retired_sticky.contains(&id) {
                continue;
            }
            promoted += self.tx.execute(
                "UPDATE memories
                 SET tier='consolidated',
                     updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                     revision=revision+1,
                     metadata=json_set(
                       CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                       '$.tier_repair','r12_memory_hygiene'
                     )
                 WHERE id=?1
                   AND COALESCE(archived,0)=0
                   AND source='foundry_distill'
                   AND tier='raw'",
                [&id],
            )?;
        }

        let derived_ids = {
            let mut statement = self.tx.prepare(
                "SELECT m.id
                 FROM memories m
                 LEFT JOIN derived_items d ON d.id='derived:' || m.id
                 WHERE COALESCE(m.archived,0)=0
                   AND m.source='foundry_distill'
                   AND m.tier='consolidated'
                   AND d.id IS NULL
                 ORDER BY m.id",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut derived_inserted = 0usize;
        for id in derived_ids {
            if retired_sticky.contains(&id) {
                continue;
            }
            derived_inserted += self.tx.execute(
                "INSERT OR IGNORE INTO derived_items
                   (id, text, path, summary, importance, source, scope, metadata, created_at)
                 SELECT 'derived:' || m.id,
                        m.text,
                        m.path,
                        m.summary,
                        m.importance,
                        m.source,
                        m.scope,
                        json_set(
                          CASE WHEN json_valid(m.metadata) THEN m.metadata ELSE '{}' END,
                          '$.legacy_repair','r12_memory_hygiene'
                        ),
                        strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 FROM memories m
                 WHERE m.id=?1
                   AND COALESCE(m.archived,0)=0
                   AND m.source='foundry_distill'
                   AND m.tier='consolidated'",
                [&id],
            )?;
        }

        let missing_edges = {
            let mut statement = self.tx.prepare(
                "WITH source_ids AS (
                   SELECT d.id AS distill_id, json_each.value AS source_id
                   FROM memories d,
                        json_each(
                          CASE
                            WHEN json_valid(d.metadata)
                             AND json_type(d.metadata, '$.source_memory_ids')='array'
                            THEN json_extract(d.metadata, '$.source_memory_ids')
                            ELSE '[]'
                          END
                        )
                   WHERE COALESCE(d.archived,0)=0
                     AND d.source='foundry_distill'
                 )
                 SELECT source_ids.distill_id, source_ids.source_id
                 FROM source_ids
                 JOIN memories s ON s.id=source_ids.source_id
                 LEFT JOIN memory_edges e ON e.source_id=source_ids.distill_id
                                         AND e.target_id=source_ids.source_id
                                         AND e.relation='distilled_from'
                 WHERE e.source_id IS NULL
                 ORDER BY source_ids.distill_id, source_ids.source_id",
            )?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<(String, String)>, _>>()?;
            rows
        };
        let mut edges_inserted = 0usize;
        for (distill_id, source_id) in missing_edges {
            if retired_sticky.contains(&distill_id) || retired_sticky.contains(&source_id) {
                continue;
            }
            let now = db::now_utc_iso();
            edges_inserted += self.tx.execute(
                "INSERT OR IGNORE INTO memory_edges
                   (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
                 VALUES (?1,?2,'distilled_from',1.0,
                         json_object('source','r12_memory_hygiene'),?3,?3,NULL)",
                params![distill_id, source_id, now],
            )?;
        }

        let covered_pairs = {
            let mut statement = self.tx.prepare(
                "WITH coverage AS (
                   SELECT d.id AS distill_id, e.target_id AS source_id, d.timestamp AS distill_ts
                   FROM memory_edges e
                   JOIN memories d ON d.id=e.source_id
                   WHERE e.relation='distilled_from'
                     AND COALESCE(d.archived,0)=0
                     AND d.source='foundry_distill'
                   UNION
                   SELECT d.id AS distill_id, json_each.value AS source_id, d.timestamp AS distill_ts
                   FROM memories d,
                        json_each(
                          CASE
                            WHEN json_valid(d.metadata)
                             AND json_type(d.metadata, '$.source_memory_ids')='array'
                            THEN json_extract(d.metadata, '$.source_memory_ids')
                            ELSE '[]'
                          END
                        )
                   WHERE COALESCE(d.archived,0)=0
                     AND d.source='foundry_distill'
                 ),
                 ranked AS (
                   SELECT s.id AS source_id,
                          c.distill_id,
                          ROW_NUMBER() OVER (
                            PARTITION BY s.id
                            ORDER BY c.distill_ts DESC, c.distill_id DESC
                          ) AS rn
                   FROM coverage c
                   JOIN memories s ON s.id=c.source_id
                   WHERE COALESCE(s.archived,0)=0
                     AND s.source!='foundry_distill'
                     AND s.tier='raw'
                     AND COALESCE(s.access_count,0)=0
                     AND COALESCE(s.recall_count,0)=0
                     AND COALESCE(s.retention_policy,'') NOT IN ('pinned','permanent')
                     AND COALESCE(s.importance,0)<0.85
                 )
                 SELECT source_id, distill_id FROM ranked
                 WHERE rn=1
                 ORDER BY source_id",
            )?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<(String, String)>, _>>()?;
            rows
        }
        .into_iter()
        .filter(|(source_id, distill_id)| {
            !retired_sticky.contains(source_id) && !retired_sticky.contains(distill_id)
        })
        .collect::<Vec<_>>();
        let covered_ids = covered_pairs
            .iter()
            .map(|(source_id, _)| source_id.as_str())
            .collect::<HashSet<_>>();

        let duplicate_pairs = {
            let mut statement = self.tx.prepare(
                "WITH normalized AS (
                   SELECT id, path, summary, source, category, retention_policy, access_count,
                          recall_count, importance, timestamp, tier,
                          CASE
                            WHEN path LIKE '/trading/equity/daily_review/%'
                              AND instr(text,'{') > 0
                              AND json_valid(substr(text, instr(text,'{')))
                            THEN json_remove(
                              substr(text, instr(text,'{')),
                              '$.summary.created_at',
                              '$.created_at',
                              '$.timestamp',
                              '$.updated_at'
                            )
                            ELSE text
                          END AS normalized_text
                   FROM memories
                   WHERE COALESCE(archived,0)=0
                 ),
                 ranked AS (
                   SELECT *,
                          COUNT(*) OVER (
                            PARTITION BY path, summary, normalized_text, source, category
                          ) AS group_count,
                          FIRST_VALUE(id) OVER (
                            PARTITION BY path, summary, normalized_text, source, category
                            ORDER BY
                              CASE WHEN retention_policy IN ('pinned','permanent') THEN 1 ELSE 0 END DESC,
                              COALESCE(access_count,0) DESC,
                              COALESCE(recall_count,0) DESC,
                              COALESCE(importance,0) DESC,
                              timestamp DESC,
                              id DESC
                          ) AS keep_id,
                          ROW_NUMBER() OVER (
                            PARTITION BY path, summary, normalized_text, source, category
                            ORDER BY
                              CASE WHEN retention_policy IN ('pinned','permanent') THEN 1 ELSE 0 END DESC,
                              COALESCE(access_count,0) DESC,
                              COALESCE(recall_count,0) DESC,
                              COALESCE(importance,0) DESC,
                              timestamp DESC,
                              id DESC
                          ) AS rn
                   FROM normalized
                 )
                 SELECT id, keep_id
                 FROM ranked
                 WHERE group_count > 1
                   AND rn > 1
                   AND id != keep_id
                   AND source!='foundry_distill'
                   AND tier='raw'
                   AND COALESCE(access_count,0)=0
                   AND COALESCE(recall_count,0)=0
                   AND COALESCE(retention_policy,'') NOT IN ('pinned','permanent')
                 ORDER BY id",
            )?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<(String, String)>, _>>()?;
            rows
        }
        .into_iter()
        .filter(|(source_id, keep_id)| {
            !retired_sticky.contains(source_id)
                && !retired_sticky.contains(keep_id)
                && !covered_ids.contains(source_id.as_str())
        })
        .collect::<Vec<_>>();

        Ok(R12MemoryHygienePreparation {
            promoted,
            derived_inserted,
            edges_inserted,
            covered_pairs,
            duplicate_pairs,
        })
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

    /// Merge fixed dependent metadata into a source already claimed by this
    /// transaction. Lifecycle columns remain exclusively owned by the claim;
    /// this method only records the caller's bounded repair annotation.
    pub fn update_claimed_source_metadata(
        &mut self,
        source_id: &str,
        metadata_patch: &Value,
    ) -> Result<(), MemoryError> {
        let Some(receipt) = self
            .pending_receipts
            .iter()
            .find(|receipt| receipt.source_id == source_id)
        else {
            return Err(MemoryError::InvalidArg(format!(
                "claimed source metadata update refused: no pending claim for {source_id}"
            )));
        };
        let Some(patch) = metadata_patch.as_object() else {
            return Err(MemoryError::InvalidArg(
                "claimed source metadata patch must be a JSON object".to_string(),
            ));
        };
        let current: String = self.tx.query_row(
            "SELECT metadata FROM memories WHERE id=?1 AND archived=1 AND superseded_by=?2",
            params![source_id, receipt.target_id],
            |row| row.get(0),
        )?;
        let mut merged = serde_json::from_str::<Value>(&current)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Map::new()));
        let object = merged.as_object_mut().ok_or_else(|| {
            MemoryError::Internal("claimed source metadata lost its object shape".to_string())
        })?;
        for (key, value) in patch {
            object.insert(key.clone(), value.clone());
        }
        let encoded = serde_json::to_string(&merged).map_err(|error| {
            MemoryError::Internal(format!("serialize claimed metadata: {error}"))
        })?;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let changed = self.tx.execute(
            "UPDATE memories SET metadata=?1 WHERE id=?2 AND archived=1 AND superseded_by=?3",
            params![encoded, source_id, receipt.target_id],
        )?;
        if changed != 1 {
            return Err(MemoryError::InvalidArg(format!(
                "claimed source metadata update CAS failed: {source_id}"
            )));
        }
        Ok(())
    }

    /// Write a provenance edge inside the replacement transaction.
    pub fn add_edge(&mut self, edge: &MemoryEdge) -> Result<(), MemoryError> {
        if edge.relation == "supersedes" {
            return Err(MemoryError::InvalidArg(
                "supersedes edges must be created by the canonical immutable-supersession claim"
                    .to_string(),
            ));
        }
        db::add_edge(&self.tx, edge)
    }

    /// [`Self::add_edge`] plus an explicit authority classification
    /// (tachi#1646) for the appended `edge_observations` row.
    pub fn add_edge_with_provenance(
        &mut self,
        edge: &MemoryEdge,
        provenance: &db::EdgeProvenance,
    ) -> Result<(), MemoryError> {
        if edge.relation == "supersedes" {
            return Err(MemoryError::InvalidArg(
                "supersedes edges must be created by the canonical immutable-supersession claim"
                    .to_string(),
            ));
        }
        db::add_edge_with_provenance(&self.tx, edge, provenance)
    }

    /// Persist the lifecycle edge that belongs to an applied canonical claim.
    ///
    /// This is deliberately separate from the generic graph writer: the edge
    /// must be the reverse projection of a pending source -> target claim in
    /// this same immutable transaction. The pending receipt is the authority
    /// check, so a caller cannot mint a `supersedes` edge for an unrelated pair
    /// or after finalization.
    pub fn add_canonical_supersession_edge(
        &mut self,
        edge: &MemoryEdge,
        provenance: &db::EdgeProvenance,
    ) -> Result<(), MemoryError> {
        if edge.relation != "supersedes" {
            return Err(MemoryError::InvalidArg(
                "canonical supersession edge must use the supersedes relation".to_string(),
            ));
        }
        let Some(_receipt) = self.pending_receipts.iter().find(|receipt| {
            receipt.source_id == edge.target_id && receipt.target_id == edge.source_id
        }) else {
            return Err(MemoryError::InvalidArg(format!(
                "canonical supersession edge has no matching pending claim: {} -> {}",
                edge.source_id, edge.target_id
            )));
        };
        if edge.source_id == edge.target_id {
            return Err(MemoryError::InvalidArg(
                "canonical supersession edge endpoints must be distinct".to_string(),
            ));
        }
        let _authorization =
            db::authorize_canonical_supersession_edge_write(&self.reserved_reference_write)?;
        db::add_edge_with_provenance(&self.tx, edge, provenance)
    }

    pub(crate) fn transaction(&self) -> &Transaction<'tx> {
        &self.tx
    }

    /// Record exact-dedupe lineage only after the canonical loser claim has
    /// succeeded in this same transaction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_exact_dedupe_lineage(
        &mut self,
        loser_id: &str,
        apply_id: &str,
        plan_digest: &str,
        winner_id: &str,
        before_revision: i64,
        archived_revision: i64,
        loser_valid_until_before: Option<&str>,
        applied_at: &str,
    ) -> Result<(), MemoryError> {
        self.tx.execute(
            "INSERT INTO exact_dedupe_apply_lineage (
                 loser_id,apply_id,plan_digest,winner_id,
                 before_revision,archived_revision,
                 loser_valid_until_before,applied_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                loser_id,
                apply_id,
                plan_digest,
                winner_id,
                before_revision,
                archived_revision,
                loser_valid_until_before,
                applied_at,
            ],
        )?;
        Ok(())
    }

    /// Transfer non-lifecycle graph projections after the canonical loser
    /// claim. The reserved `supersedes` edge itself is created only by the
    /// claim primitive, never by this dependent-write helper.
    pub(crate) fn transfer_exact_dedupe_edges(
        &mut self,
        loser: &str,
        winner: &str,
    ) -> Result<(), MemoryError> {
        self.tx.execute(
            "INSERT OR IGNORE INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
             SELECT ?1, target_id, relation, weight, metadata, created_at, valid_from, valid_to
             FROM memory_edges WHERE source_id = ?2 AND target_id != ?1
               AND relation != 'supersedes'",
            params![winner, loser],
        )?;
        self.tx.execute(
            "DELETE FROM memory_edges WHERE source_id = ?1 AND relation != 'supersedes'",
            params![loser],
        )?;
        self.tx.execute(
            "INSERT OR IGNORE INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
             SELECT source_id, ?1, relation, weight, metadata, created_at, valid_from, valid_to
             FROM memory_edges WHERE target_id = ?2 AND source_id != ?1
               AND relation != 'supersedes'",
            params![winner, loser],
        )?;
        self.tx.execute(
            "DELETE FROM memory_edges WHERE target_id = ?1 AND relation != 'supersedes'",
            params![loser],
        )?;
        Ok(())
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
mod tests;
