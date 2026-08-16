//! Consumer-neutral destination-side apply/readback for the durable outbox
//! (#1718, #1630 A4).
//!
//! This is deliberately a destination API, not a transport.  A caller hands
//! the kernel one claimed event envelope; the kernel validates the source
//! binding, enforces the destination's stamped store/partition identity,
//! classifies an existing receipt or object, and commits the object plus the
//! immutable readback receipt in one `BEGIN IMMEDIATE` transaction.

use rusqlite::TransactionBehavior;

use crate::{
    db::{self, OutboxDestinationApplyReceiptRow},
    error::MemoryError,
    path_router::UNKNOWN_DB_LABEL,
    types::MemoryEntry,
    MemoryStore,
};

/// The destination store identity named by a claimed envelope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxDestinationIdentity {
    pub store: String,
    pub partition: String,
}

/// One typed source claim plus its payload and destination binding.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutboxDestinationApplyEnvelope {
    pub claimed: super::outbox_protocol::ClaimedOutboxEvent,
    pub payload: MemoryEntry,
    pub destination: OutboxDestinationIdentity,
}

/// Whether a destination apply created the durable receipt or read it back as
/// an idempotent replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxDestinationApplyApplication {
    Applied,
    Duplicate,
}

/// The only two destination conflict classes in the frozen #1718 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxDestinationConflictReason {
    EventReplayMismatch,
    ExistingObjectDiverges,
}

/// Durable readback proof for an applied or duplicate envelope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxDestinationApplyReceipt {
    pub event_id: String,
    pub object_id: String,
    pub source_store: String,
    pub source_partition: String,
    pub source_revision: i64,
    pub source_payload_digest: String,
    pub destination_store: String,
    pub destination_partition: String,
    pub destination_object_revision: i64,
    pub destination_payload_digest: String,
    pub application: OutboxDestinationApplyApplication,
}

/// Binding and destination readback evidence for a refused apply.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxDestinationConflictReceipt {
    pub event_id: String,
    pub object_id: String,
    pub source_store: String,
    pub source_partition: String,
    pub source_revision: i64,
    pub source_payload_digest: String,
    pub destination_store: String,
    pub destination_partition: String,
    pub destination_object_revision: i64,
    pub destination_payload_digest: String,
    pub reason: OutboxDestinationConflictReason,
}

/// Closed result set for the destination apply boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutboxDestinationApplyResult {
    Applied(OutboxDestinationApplyReceipt),
    Duplicate(OutboxDestinationApplyReceipt),
    Conflict(OutboxDestinationConflictReceipt),
}

fn validate_identity(field: &str, value: &str) -> Result<(), MemoryError> {
    if value.trim().is_empty() {
        return Err(MemoryError::InvalidArg(format!(
            "destination {field} must be non-empty"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(MemoryError::InvalidArg(format!(
            "destination {field} contains control characters"
        )));
    }
    Ok(())
}

fn validate_envelope(envelope: &OutboxDestinationApplyEnvelope) -> Result<(), MemoryError> {
    let event = &envelope.claimed.event;
    if event.state != db::OutboxState::InFlight {
        return Err(MemoryError::InvalidArg(format!(
            "destination apply requires claimed event '{}' to be in_flight, found '{}'",
            event.event_id, event.state
        )));
    }
    db::validate_destination_event_binding(event)?;
    validate_identity("store", &envelope.destination.store)?;
    validate_identity("partition", &envelope.destination.partition)?;
    if envelope.destination.store == UNKNOWN_DB_LABEL {
        return Err(MemoryError::InvalidArg(
            "destination store identity must not be 'unknown'".to_string(),
        ));
    }
    if envelope.payload.id.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "destination payload id must be non-empty".to_string(),
        ));
    }
    if envelope.payload.revision < 1 {
        return Err(MemoryError::InvalidArg(
            "destination payload revision must be positive".to_string(),
        ));
    }
    if envelope.payload.id != event.object_id {
        return Err(MemoryError::InvalidArg(format!(
            "destination payload id '{}' does not match event object_id '{}'",
            envelope.payload.id, event.object_id
        )));
    }
    if envelope.payload.revision != event.source_revision {
        return Err(MemoryError::InvalidArg(format!(
            "destination payload revision {} does not match event source_revision {}",
            envelope.payload.revision, event.source_revision
        )));
    }
    let payload_digest = super::outbox::outbox_payload_digest(&envelope.payload)?;
    if payload_digest != event.payload_digest {
        return Err(MemoryError::InvalidArg(format!(
            "destination payload digest does not match event '{}' payload_digest",
            event.event_id
        )));
    }
    crate::path_router::validate_retired_sticky_write(
        &envelope.payload.path,
        &envelope.payload.category,
    )
    .map_err(|error| MemoryError::InvalidArg(error.to_string()))?;
    Ok(())
}

fn read_destination_object(
    tx: &rusqlite::Transaction<'_>,
    object_id: &str,
) -> Result<Option<MemoryEntry>, MemoryError> {
    Ok(db::fetch_by_ids(tx, &[object_id.to_string()], true)?.remove(object_id))
}

fn readback_digest(entry: &MemoryEntry) -> Result<String, MemoryError> {
    super::outbox::outbox_payload_digest(entry)
}

fn require_object_readback(
    tx: &rusqlite::Transaction<'_>,
    object_id: &str,
    expected_revision: i64,
    expected_digest: &str,
    context: &str,
) -> Result<MemoryEntry, MemoryError> {
    let stored = read_destination_object(tx, object_id)?.ok_or_else(|| {
        MemoryError::Internal(format!(
            "destination apply {context} receipt names missing object '{object_id}'"
        ))
    })?;
    let digest = readback_digest(&stored)?;
    if stored.id != object_id || stored.revision != expected_revision || digest != expected_digest {
        return Err(MemoryError::Internal(format!(
            "destination apply {context} readback disagrees for object '{object_id}': expected revision {expected_revision}, digest {expected_digest}; found revision {}, digest {digest}",
            stored.revision
        )));
    }
    Ok(stored)
}

fn validate_stored_receipt(receipt: &OutboxDestinationApplyReceiptRow) -> Result<(), MemoryError> {
    if receipt.application != "applied" {
        return Err(MemoryError::InvalidArg(format!(
            "stored destination apply receipt '{}' has unknown application '{}', expected 'applied'",
            receipt.event_id, receipt.application
        )));
    }
    validate_identity("receipt source_store", &receipt.source_store)?;
    validate_identity("receipt source_partition", &receipt.source_partition)?;
    validate_identity("receipt destination_store", &receipt.destination_store)?;
    validate_identity(
        "receipt destination_partition",
        &receipt.destination_partition,
    )?;
    if receipt.source_revision < 1 || receipt.destination_object_revision < 1 {
        return Err(MemoryError::InvalidArg(format!(
            "stored destination apply receipt '{}' has non-positive revision",
            receipt.event_id
        )));
    }
    db::refuse_non_canonical_digest(&receipt.source_payload_digest)?;
    db::refuse_non_canonical_digest(&receipt.destination_payload_digest)?;
    Ok(())
}

fn receipt_matches_envelope(
    receipt: &OutboxDestinationApplyReceiptRow,
    envelope: &OutboxDestinationApplyEnvelope,
) -> bool {
    let event = &envelope.claimed.event;
    receipt.event_id == event.event_id
        && receipt.object_id == event.object_id
        && receipt.source_store == event.source_store
        && receipt.source_partition == event.source_partition
        && receipt.source_revision == event.source_revision
        && receipt.source_payload_digest == event.payload_digest
        && receipt.destination_store == envelope.destination.store
        && receipt.destination_partition == envelope.destination.partition
}

fn conflict_from_receipt(
    envelope: &OutboxDestinationApplyEnvelope,
    receipt: &OutboxDestinationApplyReceiptRow,
) -> OutboxDestinationConflictReceipt {
    let event = &envelope.claimed.event;
    OutboxDestinationConflictReceipt {
        event_id: event.event_id.clone(),
        object_id: event.object_id.clone(),
        source_store: event.source_store.clone(),
        source_partition: event.source_partition.clone(),
        source_revision: event.source_revision,
        source_payload_digest: event.payload_digest.clone(),
        destination_store: envelope.destination.store.clone(),
        destination_partition: envelope.destination.partition.clone(),
        destination_object_revision: receipt.destination_object_revision,
        destination_payload_digest: receipt.destination_payload_digest.clone(),
        reason: OutboxDestinationConflictReason::EventReplayMismatch,
    }
}

fn conflict_from_object(
    envelope: &OutboxDestinationApplyEnvelope,
    stored: &MemoryEntry,
) -> Result<OutboxDestinationConflictReceipt, MemoryError> {
    let event = &envelope.claimed.event;
    Ok(OutboxDestinationConflictReceipt {
        event_id: event.event_id.clone(),
        object_id: event.object_id.clone(),
        source_store: event.source_store.clone(),
        source_partition: event.source_partition.clone(),
        source_revision: event.source_revision,
        source_payload_digest: event.payload_digest.clone(),
        destination_store: envelope.destination.store.clone(),
        destination_partition: envelope.destination.partition.clone(),
        destination_object_revision: stored.revision,
        destination_payload_digest: readback_digest(stored)?,
        reason: OutboxDestinationConflictReason::ExistingObjectDiverges,
    })
}

fn receipt_from_row(
    row: OutboxDestinationApplyReceiptRow,
    application: OutboxDestinationApplyApplication,
) -> OutboxDestinationApplyReceipt {
    OutboxDestinationApplyReceipt {
        event_id: row.event_id,
        object_id: row.object_id,
        source_store: row.source_store,
        source_partition: row.source_partition,
        source_revision: row.source_revision,
        source_payload_digest: row.source_payload_digest,
        destination_store: row.destination_store,
        destination_partition: row.destination_partition,
        destination_object_revision: row.destination_object_revision,
        destination_payload_digest: row.destination_payload_digest,
        application,
    }
}

impl MemoryStore {
    /// Apply one claimed outbox envelope to this destination store.
    ///
    /// Validation happens before a transaction opens.  The destination role
    /// stamp, first partition stamp, object insert/readback and receipt row
    /// are then observed under one immediate transaction.  A conflict result
    /// intentionally drops that transaction so a rejected envelope cannot
    /// establish a partition stamp or leave any partial object/ledger state.
    pub fn apply_outbox_destination(
        &mut self,
        envelope: &OutboxDestinationApplyEnvelope,
    ) -> Result<OutboxDestinationApplyResult, MemoryError> {
        validate_envelope(envelope)?;
        let db_label = self.db_label.clone();
        let reserved_reference_write = self.reserved_reference_write.clone();
        let vec_available = self.vec_available;

        let (result, commit) = db::retry_memory_locked(
            "apply_outbox_destination",
            &db_label,
            || {
                let _authorization =
                    db::authorize_reserved_reference_write(&reserved_reference_write)?;
                let tx = self
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;

                let stamped_store =
                    db::store_identity::read_stamp(&tx, db::store_profile::STORE_ROLE_KEY)?
                        .ok_or_else(|| {
                            MemoryError::InvalidArg(
                                "destination apply requires a stamped destination store identity"
                                    .to_string(),
                            )
                        })?;
                if stamped_store == UNKNOWN_DB_LABEL || stamped_store != envelope.destination.store
                {
                    return Err(MemoryError::InvalidArg(format!(
                        "destination store identity mismatch: stamped '{}', envelope '{}'",
                        stamped_store, envelope.destination.store
                    )));
                }

                let stamped_partition = db::store_identity::read_stamp(
                    &tx,
                    db::store_identity::OUTBOX_DESTINATION_PARTITION_KEY,
                )?;
                match stamped_partition.as_deref() {
                    Some(stamped) if stamped != envelope.destination.partition => {
                        return Err(MemoryError::InvalidArg(format!(
                            "destination partition mismatch: stamped '{}', envelope '{}'",
                            stamped, envelope.destination.partition
                        )))
                    }
                    Some(_) => {}
                    None => {
                        db::store_identity::write_stamp_if_absent(
                            &tx,
                            db::store_identity::OUTBOX_DESTINATION_PARTITION_KEY,
                            &envelope.destination.partition,
                            "outbox_destination_apply",
                        )?;
                    }
                }

                let event = &envelope.claimed.event;
                let stored_receipt =
                    db::read_outbox_destination_apply_receipt(&tx, &event.event_id)?;
                let (result, commit) = if let Some(receipt) = stored_receipt {
                    validate_stored_receipt(&receipt).map_err(|error| {
                        MemoryError::Internal(format!(
                            "corrupt destination apply receipt '{}': {error}",
                            receipt.event_id
                        ))
                    })?;
                    // Verify the durable receipt's own object before classifying a
                    // replay.  A missing or divergent object is corruption, not
                    // a fresh success to synthesize.
                    require_object_readback(
                        &tx,
                        &receipt.object_id,
                        receipt.destination_object_revision,
                        &receipt.destination_payload_digest,
                        "stored",
                    )?;
                    if receipt_matches_envelope(&receipt, envelope) {
                        (
                            OutboxDestinationApplyResult::Duplicate(receipt_from_row(
                                receipt,
                                OutboxDestinationApplyApplication::Duplicate,
                            )),
                            true,
                        )
                    } else {
                        (
                            OutboxDestinationApplyResult::Conflict(conflict_from_receipt(
                                envelope, &receipt,
                            )),
                            false,
                        )
                    }
                } else {
                    let existing = read_destination_object(&tx, &event.object_id)?;
                    let stored = match existing {
                        None => {
                            match db::insert_if_absent_within_tx(
                                &tx,
                                &envelope.payload,
                                vec_available,
                            )? {
                                db::InsertMemoryResult::Inserted => require_object_readback(
                                    &tx,
                                    &event.object_id,
                                    event.source_revision,
                                    &event.payload_digest,
                                    "newly applied",
                                )?,
                                db::InsertMemoryResult::Existing => {
                                    read_destination_object(&tx, &event.object_id)?.ok_or_else(
                                        || {
                                            MemoryError::Internal(
                                                "destination insert reported Existing but object disappeared"
                                                    .to_string(),
                                            )
                                        },
                                    )?
                                }
                            }
                        }
                        Some(existing) => existing,
                    };
                    let stored_digest = readback_digest(&stored)?;
                    if stored.id != event.object_id
                        || stored.revision != event.source_revision
                        || stored_digest != event.payload_digest
                    {
                        (
                            OutboxDestinationApplyResult::Conflict(conflict_from_object(
                                envelope, &stored,
                            )?),
                            false,
                        )
                    } else {
                        let row = db::insert_outbox_destination_apply_receipt_within_tx(
                            &tx,
                            &OutboxDestinationApplyReceiptRow {
                                event_id: event.event_id.clone(),
                                object_id: event.object_id.clone(),
                                source_store: event.source_store.clone(),
                                source_partition: event.source_partition.clone(),
                                source_revision: event.source_revision,
                                source_payload_digest: event.payload_digest.clone(),
                                destination_store: envelope.destination.store.clone(),
                                destination_partition: envelope.destination.partition.clone(),
                                destination_object_revision: stored.revision,
                                destination_payload_digest: stored_digest,
                                application: "applied".to_string(),
                            },
                        )?;
                        (
                            OutboxDestinationApplyResult::Applied(receipt_from_row(
                                row,
                                OutboxDestinationApplyApplication::Applied,
                            )),
                            true,
                        )
                    }
                };
                if commit {
                    tx.commit()?;
                }
                Ok((result, commit))
            },
        )?;
        let _ = commit;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, types::MemoryEntry, OutboxClaimRequest, OutboxEventMeta};
    use rusqlite::{params, Connection};
    use std::{path::Path, time::Duration};
    use tempfile::tempdir;

    fn test_entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: "/scratch/memcore/destination-apply".into(),
            summary: format!("{id} summary"),
            text: text.into(),
            importance: 0.7,
            timestamp: "2026-08-10T00:00:00Z".into(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "destination-apply".into(),
            keywords: vec!["outbox".into()],
            persons: vec![],
            entities: vec!["memcore".into()],
            location: String::new(),
            source: "manual".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({"contract": "1718"}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".into(),
        }
    }

    fn source_and_envelope(
        source_path: &Path,
        object_id: &str,
    ) -> (MemoryStore, OutboxDestinationApplyEnvelope) {
        let source_path = source_path.to_string_lossy().into_owned();
        let mut source =
            MemoryStore::open_with_label(&source_path, "global").expect("open source store");
        let payload = test_entry(object_id, "destination apply payload");
        source
            .commit_with_outbox_event(
                &payload,
                &OutboxEventMeta {
                    event_id: format!("evt-{object_id}"),
                    object_class: "memory".into(),
                    authority_class: "host".into(),
                    source_store: "global".into(),
                    source_partition: "source-a".into(),
                },
            )
            .expect("source commit");
        let claimed = source
            .claim_outbox_events(&OutboxClaimRequest::first_claims_only(1))
            .expect("claim source event")
            .pop()
            .expect("one claimed source event");
        let stored_payload = source
            .get_with_options(object_id, true)
            .expect("source readback")
            .expect("source object");
        (
            source,
            OutboxDestinationApplyEnvelope {
                claimed,
                payload: stored_payload,
                destination: OutboxDestinationIdentity {
                    store: "global".into(),
                    partition: "destination-a".into(),
                },
            },
        )
    }

    fn envelope_with_retired_payload(
        source_path: &Path,
        object_id: &str,
        path: &str,
        category: &str,
    ) -> OutboxDestinationApplyEnvelope {
        let (_source, mut envelope) = source_and_envelope(source_path, object_id);
        envelope.payload.path = path.into();
        envelope.payload.category = category.into();
        envelope.claimed.event.payload_digest =
            crate::outbox_payload_digest(&envelope.payload).expect("retired payload digest");
        envelope
    }

    fn open_destination(path: &Path) -> MemoryStore {
        let path = path.to_string_lossy().into_owned();
        MemoryStore::open_with_label(&path, "global").expect("open destination store")
    }

    fn receipt_count(store: &MemoryStore) -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_outbox_destination_apply_receipts",
                [],
                |row| row.get(0),
            )
            .expect("count destination receipts")
    }

    fn partition_stamp(store: &MemoryStore) -> Option<String> {
        db::store_identity::read_stamp(
            &store.conn,
            db::store_identity::OUTBOX_DESTINATION_PARTITION_KEY,
        )
        .expect("read destination partition stamp")
    }

    #[derive(Debug, PartialEq, Eq)]
    struct DestinationMutationState {
        memory: Option<Vec<u8>>,
        memory_count: i64,
        receipt_count: i64,
        event_count: i64,
        outbox_health: db::OutboxHealth,
        identity_rows: Vec<(String, String, String, i64, String, String)>,
        partition_stamp: Option<String>,
    }

    fn destination_mutation_state(
        store: &MemoryStore,
        object_id: &str,
    ) -> DestinationMutationState {
        let memory = store
            .get_with_options(object_id, true)
            .expect("read destination memory")
            .map(|entry| serde_json::to_vec(&entry).expect("serialize destination memory"));
        let memory_count = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE id = ?1",
                params![object_id],
                |row| row.get(0),
            )
            .expect("count destination memory");
        let receipt_count = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_outbox_destination_apply_receipts",
                [],
                |row| row.get(0),
            )
            .expect("count destination receipts");
        let event_count = store
            .conn
            .query_row("SELECT COUNT(*) FROM memory_outbox_events", [], |row| {
                row.get(0)
            })
            .expect("count destination outbox events");
        let mut identity_rows = store
            .conn
            .prepare(
                "SELECT namespace, key, value_json, version, created_at, updated_at
                 FROM hard_state ORDER BY namespace, key",
            )
            .expect("prepare identity snapshot")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .expect("read identity snapshot")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect identity snapshot");
        identity_rows.shrink_to_fit();

        DestinationMutationState {
            memory,
            memory_count,
            receipt_count,
            event_count,
            outbox_health: store
                .outbox_health()
                .expect("read destination outbox health"),
            identity_rows,
            partition_stamp: partition_stamp(store),
        }
    }

    fn flip_digest(digest: &str) -> String {
        let mut changed = digest.to_string();
        let replacement = if changed.starts_with('0') { '1' } else { '0' };
        changed.replace_range(0..1, &replacement.to_string());
        changed
    }

    #[test]
    fn destination_apply_rejects_retired_sticky_payload_before_any_mutation() {
        for (case, path, category) in [
            ("path", "/sticky/legacy-bucket", "fact"),
            ("category", "/scratch/memcore/destination-apply", "Sticky"),
        ] {
            let source_dir = tempdir().expect("source tempdir");
            let destination_dir = tempdir().expect("destination tempdir");
            let object_id = format!("unit-destination-apply-retired-{case}");
            let envelope = envelope_with_retired_payload(
                &source_dir.path().join("memory.db"),
                &object_id,
                path,
                category,
            );
            let mut destination = open_destination(&destination_dir.path().join("memory.db"));
            let before = destination_mutation_state(&destination, &object_id);

            let error = destination
                .apply_outbox_destination(&envelope)
                .expect_err("retired sticky payload must be refused");
            assert!(
                matches!(error, MemoryError::InvalidArg(_))
                    && error.to_string().contains("retired"),
                "retired {case} refusal must be typed and explicit: {error}"
            );

            assert_eq!(
                destination_mutation_state(&destination, &object_id),
                before,
                "retired {case} refusal must leave destination state byte-equivalent"
            );
            assert!(destination
                .get_with_options(&object_id, true)
                .expect("read refused destination memory")
                .is_none());
            assert_eq!(receipt_count(&destination), 0);
            assert_eq!(
                destination
                    .outbox_health()
                    .expect("read refused destination outbox health"),
                db::OutboxHealth {
                    local_store_status: db::LocalStoreStatus::Healthy,
                    remote_sync_status: db::RemoteSyncStatus::Unconfigured,
                    pending_count: 0,
                    in_flight_count: 0,
                    acknowledged_count: 0,
                    rejected_count: 0,
                    conflicted_count: 0,
                    quarantined_count: 0,
                    oldest_pending_at: None,
                    oldest_in_flight_at: None,
                    last_successful_sync: None,
                    last_error_class: None,
                    resolved_count: 0,
                    stale_lease_count: 0,
                }
            );
            assert!(partition_stamp(&destination).is_none());
        }
    }

    #[test]
    fn destination_duplicate_reads_immutable_receipt_and_conserves_one_ledger_row() {
        let source_dir = tempdir().expect("source tempdir");
        let destination_dir = tempdir().expect("destination tempdir");
        let object_id = "unit-destination-apply-duplicate";
        let (_source, envelope) =
            source_and_envelope(&source_dir.path().join("memory.db"), object_id);
        let destination_path = destination_dir.path().join("memory.db");
        let mut destination = open_destination(&destination_path);
        let applied = match destination
            .apply_outbox_destination(&envelope)
            .expect("first apply")
        {
            OutboxDestinationApplyResult::Applied(receipt) => receipt,
            other => panic!("expected Applied, got {other:?}"),
        };
        let applied_stamp = partition_stamp(&destination);
        assert_eq!(receipt_count(&destination), 1);
        let applied_object = destination
            .get_with_options(object_id, true)
            .expect("read applied object")
            .expect("applied object exists");
        assert_eq!(
            applied.destination_payload_digest,
            crate::outbox_payload_digest(&applied_object).expect("applied object digest")
        );
        drop(destination);

        let mut restarted = open_destination(&destination_path);
        let duplicate = match restarted
            .apply_outbox_destination(&envelope)
            .expect("replay after restart")
        {
            OutboxDestinationApplyResult::Duplicate(receipt) => receipt,
            other => panic!("expected Duplicate, got {other:?}"),
        };
        assert_eq!(duplicate.event_id, applied.event_id);
        assert_eq!(duplicate.object_id, applied.object_id);
        assert_eq!(duplicate.source_store, applied.source_store);
        assert_eq!(duplicate.source_partition, applied.source_partition);
        assert_eq!(duplicate.source_revision, applied.source_revision);
        assert_eq!(
            duplicate.source_payload_digest,
            applied.source_payload_digest
        );
        assert_eq!(duplicate.destination_store, applied.destination_store);
        assert_eq!(
            duplicate.destination_partition,
            applied.destination_partition
        );
        assert_eq!(
            duplicate.destination_object_revision,
            applied.destination_object_revision
        );
        assert_eq!(
            duplicate.destination_payload_digest,
            applied.destination_payload_digest
        );
        assert_eq!(
            duplicate.application,
            OutboxDestinationApplyApplication::Duplicate
        );
        assert_eq!(receipt_count(&restarted), 1);
        assert_eq!(partition_stamp(&restarted), applied_stamp);
    }

    #[test]
    fn destination_receipt_object_disagreement_is_loud_and_unrepaired() {
        let source_dir = tempdir().expect("source tempdir");
        let destination_dir = tempdir().expect("destination tempdir");
        let object_id = "unit-destination-apply-corruption";
        let (_source, envelope) =
            source_and_envelope(&source_dir.path().join("memory.db"), object_id);
        let mut destination = open_destination(&destination_dir.path().join("memory.db"));
        let applied = match destination
            .apply_outbox_destination(&envelope)
            .expect("first apply")
        {
            OutboxDestinationApplyResult::Applied(receipt) => receipt,
            other => panic!("expected Applied, got {other:?}"),
        };
        let corrupt_digest = flip_digest(&applied.destination_payload_digest);
        destination
            .conn
            .execute(
                "UPDATE memory_outbox_destination_apply_receipts
                 SET destination_payload_digest = ?1 WHERE event_id = ?2",
                params![corrupt_digest, &applied.event_id],
            )
            .expect("inject receipt corruption");

        let error = destination
            .apply_outbox_destination(&envelope)
            .expect_err("receipt/object disagreement must fail closed");
        assert!(
            matches!(error, MemoryError::Internal(_))
                && error.to_string().contains("readback disagrees"),
            "expected loud readback invariant error, got {error}"
        );
        let stored_corrupt_digest: String = destination
            .conn
            .query_row(
                "SELECT destination_payload_digest
                 FROM memory_outbox_destination_apply_receipts WHERE event_id = ?1",
                params![&applied.event_id],
                |row| row.get(0),
            )
            .expect("read corrupted receipt");
        assert_eq!(stored_corrupt_digest, corrupt_digest);
        assert_eq!(receipt_count(&destination), 1);
        assert!(partition_stamp(&destination).is_some());
        let object = destination
            .get_with_options(object_id, true)
            .expect("read object after corruption refusal")
            .expect("object remains after corruption refusal");
        assert_eq!(
            crate::outbox_payload_digest(&object).expect("object digest after refusal"),
            applied.destination_payload_digest
        );
    }

    #[test]
    fn destination_busy_retry_exhaustion_leaves_no_object_receipt_or_partition_stamp() {
        let source_dir = tempdir().expect("source tempdir");
        let destination_dir = tempdir().expect("destination tempdir");
        let object_id = "unit-destination-apply-busy";
        let (_source, envelope) =
            source_and_envelope(&source_dir.path().join("memory.db"), object_id);
        let destination_path = destination_dir.path().join("memory.db");
        let mut destination = open_destination(&destination_path);
        destination
            .conn
            .busy_timeout(Duration::ZERO)
            .expect("disable destination busy wait");
        destination
            .conn
            .pragma_update(None, "journal_mode", "DELETE")
            .expect("use rollback journal for lock discrimination");

        let locker = Connection::open(&destination_path).expect("open lock owner");
        locker
            .busy_timeout(Duration::ZERO)
            .expect("disable lock owner busy wait");
        locker
            .execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
            .expect("hold exclusive destination lock");

        let error = destination
            .apply_outbox_destination(&envelope)
            .expect_err("persistent lock must exhaust retries");
        assert!(
            matches!(error, MemoryError::Sqlite(ref sqlite) if db::sqlite_error_is_locked(sqlite)),
            "retry exhaustion must preserve the typed SQLite lock error: {error}"
        );
        drop(locker);
        drop(destination);

        let reopened = open_destination(&destination_path);
        assert!(reopened
            .get_with_options(object_id, true)
            .expect("read object after busy exhaustion")
            .is_none());
        assert_eq!(receipt_count(&reopened), 0);
        assert!(partition_stamp(&reopened).is_none());
    }
}
