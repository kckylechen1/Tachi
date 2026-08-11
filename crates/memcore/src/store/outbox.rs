//! The single-transaction local commit boundary for the durable outbox
//! (tachi#1643, #1630 workstream A leaf A1).
//!
//! #1630's frozen contract: *local commit is authoritative for local
//! visibility*. The memory write and the outbox event that announces it become
//! durable together or not at all — there is no window in which a consumer
//! could read a row with no event (a mutation that silently never syncs) or an
//! event with no row (an announcement of something that does not exist). This
//! module owns the `BEGIN IMMEDIATE` boundary that makes that true, and
//! `db::outbox` owns the rows inside it.
//!
//! ## Why the receipt is read back from the destination
//!
//! [`OutboxCommitReceipt`] is built from what the database holds after both
//! writes, inside the same transaction — never from the caller's inputs. That
//! is the tachi#1607 idiom, and here it does concrete work rather than
//! ceremony: the digest is recomputed from the stored row and compared against
//! the stored event, and the object's revision is read from `memories` and
//! compared against the event's `source_revision`. So a write path that
//! quietly redirected the row (a near-duplicate merge folding it into a
//! different id), or a payload the store normalized after the digest was
//! taken, fails the commit instead of producing a green receipt over a
//! divergent pair.
//!
//! ## Class law (tachi#1585)
//!
//! No closure and no `Connection`/`Transaction` appears in any *public*
//! signature here, which is why this surface is ungated for the portable
//! build. The one transaction-taking function,
//! [`enqueue_outbox_event_within_tx`], is crate-internal and reaches callers
//! through narrow handles — today
//! [`crate::store::immutable_supersession::ImmutableSupersessionTransaction::enqueue_outbox_event`].

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    db::{self, OutboxEventRow, OutboxState},
    error::MemoryError,
    types::MemoryEntry,
    MemoryStore,
};

/// The caller-supplied half of an outbox event.
///
/// Everything a caller *cannot* supply is absent by construction:
/// `source_revision` and `payload_digest` are derived from the stored row
/// inside the commit transaction, and `state`/`created_at`/`state_changed_at`
/// are stamped by the kernel. What remains here is exactly the identity and
/// provenance #1630 says the host owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEventMeta {
    /// Caller-stable and immutable. This is the idempotency key for the whole
    /// pipeline: a replay of the same logical mutation must reuse it, and the
    /// commit refuses a second event under an id that already exists rather
    /// than rewriting the recorded outcome.
    pub event_id: String,
    /// What kind of object this event announces (e.g. a memory). A token, not
    /// free text — bounded by [`db::MAX_OUTBOX_CLASS_BYTES`].
    pub object_class: String,
    /// Under what authority the mutation was made. A token, same bound. The
    /// kernel records it and never interprets it: #1630 puts judgment
    /// semantics outside the outbox.
    pub authority_class: String,
    /// Which store the mutation happened in.
    pub source_store: String,
    /// Which partition of that store.
    pub source_partition: String,
}

/// Proof that both halves of one commit are durable and agree.
///
/// Read the two fields as coming from two different tables: `event` is the
/// stored `memory_outbox_events` row, `object_revision` is `memories.revision`
/// read back independently. The commit refuses unless they agree, so holding
/// this receipt is the checkable form of "the row and its event landed in one
/// transaction".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxCommitReceipt {
    /// The event row exactly as stored: `state` is
    /// [`OutboxState::Pending`], and `payload_digest` is the digest of the
    /// payload **as the store holds it**.
    ///
    /// A caller that wants to know whether the write path changed its payload
    /// compares `event.payload_digest` against
    /// [`outbox_payload_digest`] over the entry it submitted. A difference is
    /// normal, not corruption: the ordinary write path scrubs think-tags,
    /// clamps importance, normalizes timestamps, and strips reserved metadata
    /// keys. The digest describes what will be synced, which is the stored
    /// row.
    pub event: OutboxEventRow,
    /// `memories.revision` of the committed object, read from the destination
    /// row. Equal to `event.source_revision`; the commit fails rather than
    /// returning a receipt where the two disagree.
    pub object_revision: i64,
}

/// The exact payload projection [`outbox_payload_digest`] hashes.
///
/// Field order is alphabetical and load-bearing: this struct is serialized
/// directly (never through a `serde_json::Value` wrapper), so the emitted JSON
/// object key order is this declaration order. Same rule, and same reason, as
/// `db::SnapshotLifecycleRow` and `store::memory_lifecycle`'s
/// `LifecycleApplyPayload`.
#[derive(Debug, Serialize)]
struct OutboxPayloadV1<'entry> {
    archived: bool,
    category: &'entry str,
    domain: Option<&'entry str>,
    entities: &'entry [String],
    id: &'entry str,
    importance: f64,
    keywords: &'entry [String],
    metadata: &'entry Value,
    path: &'entry str,
    retention_policy: Option<&'entry str>,
    revision: i64,
    scope: &'entry str,
    source: &'entry str,
    summary: &'entry str,
    text: &'entry str,
    tier: &'entry str,
    timestamp: &'entry str,
    topic: &'entry str,
    valid_from: &'entry str,
    valid_until: Option<&'entry str>,
}

/// Lowercase hex SHA-256 over the canonical payload projection of `entry`.
///
/// SHA-256 via `sha2`, not BLAKE2: `sha2` is already a `memcore` dependency
/// and every existing digest in this crate is lowercase-hex SHA-256
/// (`store::snapshot_import`, `store::exact_dedupe`,
/// `store::lifecycle_consistency`, `store::memory_lifecycle`,
/// `recall_impressions`). `blake2` is resolved in this workspace, but only for
/// `tachi-llm`/`tachi-params`/`tachi-server`; adding it here would give the
/// portable kernel a second hash family for no gain.
///
/// # What is hashed, and what is not
///
/// Hashed: the twenty fields of [`OutboxPayloadV1`] — the identity, body,
/// classification, lifecycle interval and metadata a consumer needs to
/// reconstruct the memory.
///
/// Not hashed, each for a reason:
///
/// * `vector` — a derived projection recomputable at the destination. Four
///   kilobytes of float bytes per event would make every digest expensive
///   while adding no semantic information the body does not already carry.
/// * `access_count`, `scored_count`, `last_access`, `last_use_at`,
///   `recall_count`, `query_diversity` — local usage telemetry that changes
///   when nobody edited the memory. Including it would make an ordinary
///   *recall* dirty the payload digest and manufacture phantom divergence.
/// * `persons`, `location` — not stored columns. `db::MEMORY_SELECT_COLUMNS`
///   synthesizes them as constant `'[]'` and `''` (relics of dropped physical
///   columns), so hashing them would hash a constant.
///
/// # Canonicalization
///
/// Compact UTF-8 JSON (`serde_json` compact form) of one object whose keys are
/// the field names above in alphabetical order. `metadata` is embedded as its
/// stored JSON value; its own key order comes from `serde_json::Map`, which is
/// a `BTreeMap` (this workspace does not enable `serde_json/preserve_order`)
/// and therefore sorted. That is the same property `compute_lifecycle_identity`
/// and the `exact_dedupe` plan/receipt digests already rely on, so it is an
/// existing crate-wide invariant rather than a new one this seam introduces.
///
/// This function touches no database, which is what makes it useful as the
/// source-side half of a reconciliation: comparing it against a stored
/// `payload_digest` compares two independent paths, one through SQLite storage
/// and back and one not.
pub fn outbox_payload_digest(entry: &MemoryEntry) -> Result<String, MemoryError> {
    let payload = OutboxPayloadV1 {
        archived: entry.archived,
        category: &entry.category,
        domain: entry.domain.as_deref(),
        entities: &entry.entities,
        id: &entry.id,
        importance: entry.importance,
        keywords: &entry.keywords,
        metadata: &entry.metadata,
        path: &entry.path,
        retention_policy: entry.retention_policy.as_deref(),
        revision: entry.revision,
        scope: &entry.scope,
        source: &entry.source,
        summary: &entry.summary,
        text: &entry.text,
        tier: &entry.tier,
        timestamp: &entry.timestamp,
        topic: &entry.topic,
        valid_from: &entry.valid_from,
        valid_until: entry.valid_until.as_deref(),
    };
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&payload)?)
    ))
}

/// Read one object back from inside the transaction that wrote it.
fn read_object_within_tx(
    tx: &rusqlite::Transaction<'_>,
    object_id: &str,
) -> Result<MemoryEntry, MemoryError> {
    db::fetch_by_ids(tx, &[object_id.to_string()], true)?
        .remove(object_id)
        .ok_or_else(|| {
            MemoryError::NotFound(format!(
                "outbox event names object '{object_id}', which this transaction has not written; \
                 the object write and its event must share one transaction"
            ))
        })
}

/// Record one outbox event for an object the caller's transaction has already
/// written, and return the stored row.
///
/// This is the composition primitive: any transaction that mutates a memory
/// can call it and get the #1643 atomicity guarantee without going through
/// [`MemoryStore::commit_with_outbox_event`].
///
/// The digest is computed here, from the object **as the transaction now holds
/// it**, not from anything the caller passes. That ordering is the point: a
/// digest taken from the caller's struct would describe the payload the caller
/// meant to write, and the event would then announce a payload the store never
/// stored.
///
/// It deliberately does **not** take the reserved-reference write
/// authorization. That guard is a non-reentrant compare-and-swap covering
/// reserved-reference DML on `memories`; the enclosing operation has already
/// taken it, and taking it a second time here would fail every composed call.
/// The outbox table is not a reserved-reference surface, so there is nothing
/// for it to authorize.
pub(crate) fn enqueue_outbox_event_within_tx(
    tx: &rusqlite::Transaction<'_>,
    object_id: &str,
    event: &OutboxEventMeta,
) -> Result<OutboxEventRow, MemoryError> {
    let new_event = build_new_outbox_event(tx, object_id, event)?;
    db::insert_outbox_event_within_tx(tx, &new_event)
}

/// The [`OutboxConflictResolution::LocalWins`] counterpart to
/// [`enqueue_outbox_event_within_tx`] (tachi#1644 review fix).
///
/// Identical digest/revision derivation, but the insert goes through
/// [`db::insert_resolution_successor_event_within_tx`] instead of
/// [`db::insert_outbox_event_within_tx`]: `event.event_id` here is
/// `crate::store::outbox_protocol::outbox_local_wins_successor_id`'s output,
/// which legitimately carries the reserved `::local-wins` suffix that the
/// ordinary caller-facing entry refuses. Reached from exactly one call
/// site — `resolve_outbox_conflict`'s `LocalWins` arm.
///
/// [`OutboxConflictResolution::LocalWins`]: crate::store::outbox_protocol::OutboxConflictResolution::LocalWins
pub(crate) fn enqueue_outbox_resolution_successor_event_within_tx(
    tx: &rusqlite::Transaction<'_>,
    object_id: &str,
    event: &OutboxEventMeta,
) -> Result<OutboxEventRow, MemoryError> {
    let new_event = build_new_outbox_event(tx, object_id, event)?;
    db::insert_resolution_successor_event_within_tx(tx, &new_event)
}

/// Shared digest/revision derivation for both
/// [`enqueue_outbox_event_within_tx`] and
/// [`enqueue_outbox_resolution_successor_event_within_tx`]; the only
/// difference between the two callers is which `db` insert entry the result
/// goes to.
fn build_new_outbox_event(
    tx: &rusqlite::Transaction<'_>,
    object_id: &str,
    event: &OutboxEventMeta,
) -> Result<db::NewOutboxEvent, MemoryError> {
    let stored = read_object_within_tx(tx, object_id)?;
    let payload_digest = outbox_payload_digest(&stored)?;
    Ok(db::NewOutboxEvent {
        event_id: event.event_id.clone(),
        object_id: object_id.to_string(),
        object_class: event.object_class.clone(),
        authority_class: event.authority_class.clone(),
        source_store: event.source_store.clone(),
        source_partition: event.source_partition.clone(),
        payload_digest,
    })
}

/// Build the receipt from the destination's post-write state, still inside the
/// transaction, so any disagreement rolls the whole commit back.
fn build_commit_receipt(
    tx: &rusqlite::Transaction<'_>,
    object_id: &str,
    event_id: &str,
) -> Result<OutboxCommitReceipt, MemoryError> {
    let event = db::read_outbox_event(tx, event_id)?.ok_or_else(|| {
        MemoryError::Internal(format!(
            "outbox event '{event_id}' is absent from the transaction that just inserted it"
        ))
    })?;
    if event.state != OutboxState::Pending {
        return Err(MemoryError::Internal(format!(
            "outbox event '{event_id}' was born in state '{}' rather than 'pending'",
            event.state
        )));
    }
    if event.object_id != object_id {
        return Err(MemoryError::Internal(format!(
            "outbox event '{event_id}' announces object '{}' but this commit wrote '{object_id}'",
            event.object_id
        )));
    }

    // Re-read the object and recompute: this is what catches a write path that
    // redirected or renormalized the row after the digest was taken.
    let stored = read_object_within_tx(tx, object_id)?;
    let recomputed = outbox_payload_digest(&stored)?;
    if recomputed != event.payload_digest {
        return Err(MemoryError::Internal(format!(
            "outbox event '{event_id}' records digest {} but the stored object '{object_id}' \
             digests to {recomputed}",
            event.payload_digest
        )));
    }
    if stored.revision != event.source_revision {
        return Err(MemoryError::Internal(format!(
            "outbox event '{event_id}' records source_revision {} but the stored object \
             '{object_id}' is at revision {}",
            event.source_revision, stored.revision
        )));
    }

    Ok(OutboxCommitReceipt {
        event,
        object_revision: stored.revision,
    })
}

impl MemoryStore {
    /// Commit one memory write and its outbox event in a single
    /// `BEGIN IMMEDIATE` transaction, returning a receipt read back from the
    /// destination.
    ///
    /// # Atomicity
    ///
    /// One transaction covers the main row, its FTS/symbolic/vector
    /// projections, and the event. A failure at any point — an invalid entry,
    /// a duplicate `event_id`, a receipt disagreement — rolls back every part
    /// of it, so neither half is ever durable without the other. That is
    /// #1630's "local commit is authoritative for local visibility" stated as
    /// code.
    ///
    /// # Near-duplicate policy
    ///
    /// The write goes through [`db::upsert_within_tx`], which is
    /// [`db::NearDuplicatePolicy::NonSemantic`] — it writes exactly the row
    /// the caller named and never folds it into a similar one. That matters
    /// more here than on an ordinary upsert: a merge would write the payload
    /// into a *different* id, and the event would then announce an object the
    /// caller never committed. This is not left to the callee's policy alone.
    /// The receipt re-reads `entry.id` from the destination and refuses if it
    /// is absent or its digest disagrees, so a merge cannot produce a green
    /// receipt even if the policy ever changed underneath.
    ///
    /// # Refusals
    ///
    /// Everything ordinary `upsert` refuses (path validation, blank id,
    /// reserved `anchor:`/`wiki-rem:`/Wiki-log identities), plus: an
    /// `event_id` that already exists ([`MemoryError::Duplicate`]), a blank or
    /// oversized classification token, and any receipt disagreement.
    pub fn commit_with_outbox_event(
        &mut self,
        entry: &MemoryEntry,
        event: &OutboxEventMeta,
    ) -> Result<OutboxCommitReceipt, MemoryError> {
        // Path validation before any write, and outside the retry loop: an
        // invalid entry must never open a transaction at all (the ordering
        // `upsert_batch_in_tx` uses).
        self.validate_write_path(entry)?;
        let db_label = self.db_label.clone();
        let reserved_reference_write = self.reserved_reference_write.clone();
        db::retry_memory_locked("commit_with_outbox_event", &db_label, || {
            let _authorization = db::authorize_reserved_reference_write(&reserved_reference_write)?;
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            db::upsert_within_tx(&tx, entry, self.vec_available, None)?;
            enqueue_outbox_event_within_tx(&tx, &entry.id, event)?;
            let receipt = build_commit_receipt(&tx, &entry.id, &event.event_id)?;
            tx.commit()?;
            Ok(receipt)
        })
    }

    /// Read one outbox event by its caller-stable id.
    pub fn outbox_event(&self, event_id: &str) -> Result<Option<OutboxEventRow>, MemoryError> {
        db::read_outbox_event(&self.conn, event_id)
    }

    /// List events in one state, oldest first, bounded by `limit`.
    ///
    /// This is the queue read a sync loop drains from: `Pending` in
    /// enqueue order is exactly the work list. It is on the store (rather than
    /// only inside `db`) because A2's reconciliation has no other way to reach
    /// it — the `db` seams take a `Connection` and are crate-internal.
    pub fn list_outbox_events(
        &self,
        state: OutboxState,
        limit: usize,
    ) -> Result<Vec<OutboxEventRow>, MemoryError> {
        db::list_outbox_events_by_state(&self.conn, state, limit)
    }

    /// The consumer-neutral health snapshot from one consistent SQLite
    /// snapshot. This compatibility wrapper uses the named provisional
    /// [`db::outbox::DEFAULT_OUTBOX_HEALTH_STALE_AFTER`] bound; callers with an
    /// operational lease should use [`Self::outbox_health_with_stale_after`]
    /// so the threshold is explicit and deterministic.
    ///
    /// `local_store_status` and `pending_count`/`oldest_pending_at` are
    /// straightforward local facts. The other three need to be read with their
    /// derivation in mind, because **this leaf contains no remote**:
    ///
    /// * `remote_sync_status` is explicitly
    ///   [`db::RemoteSyncStatus::Unconfigured`]: this crate has no transport,
    ///   so local terminal rows cannot be presented as remote success.
    /// * `last_successful_sync` is preserved for the host-owned remote health
    ///   seam, but is always `None` here: this crate has no transport, and a
    ///   local `acknowledged` row is not evidence of remote synchronization.
    /// * `last_error_class` is the class of the most recently changed event
    ///   still in a failure state. Non-failure transitions clear the column,
    ///   so it never reports a class the outbox has moved past.
    ///
    /// Timestamps are compared as RFC3339 instants, so supported legacy offset
    /// and precision forms cannot invert the extrema.
    pub fn outbox_health(&self) -> Result<db::OutboxHealth, MemoryError> {
        self.outbox_health_with_stale_after(db::outbox::DEFAULT_OUTBOX_HEALTH_STALE_AFTER)
    }

    /// Read health using an explicit lease staleness bound.
    ///
    /// `stale_lease_count` is the number of currently `in_flight` rows whose
    /// lease stamp is at or before `now - stale_after`. `Duration::ZERO` is a
    /// valid immediate boundary: every currently held lease is eligible, so it
    /// is unsuitable for proving pre-expiry refusal or post-takeover
    /// one-shot exclusion. Use a small positive bound for that proof. The
    /// entire report is derived from one SQLite snapshot.
    pub fn outbox_health_with_stale_after(
        &self,
        stale_after: Duration,
    ) -> Result<db::OutboxHealth, MemoryError> {
        db::read_outbox_health(&self.conn, stale_after)
    }

    /// Move one event through the frozen state machine, returning the stored
    /// row.
    ///
    /// `error_class` is required by the failure states
    /// (`rejected`/`conflicted`/`quarantined`) and refused by the others. An
    /// illegal transition is [`MemoryError::OutboxIllegalTransition`] and
    /// writes nothing. See [`OutboxState::can_transition_to`] for the full
    /// matrix — in particular, there is no retry edge: a retry is a new event
    /// with a new id, not a rewrite of this one's history.
    ///
    /// The transition runs in its own `BEGIN IMMEDIATE` transaction. It is
    /// deliberately *not* fused to a memory write: an outcome reported by a
    /// consumer is information about the event, not a new mutation of the
    /// object.
    pub fn transition_outbox_event(
        &mut self,
        event_id: &str,
        next: OutboxState,
        error_class: Option<&str>,
    ) -> Result<OutboxEventRow, MemoryError> {
        let db_label = self.db_label.clone();
        db::retry_memory_locked("transition_outbox_event", &db_label, || {
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let row = db::transition_outbox_event_within_tx(&tx, event_id, next, error_class)?;
            tx.commit()?;
            Ok(row)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/scratch/outbox".to_string(),
            summary: String::new(),
            text: text.to_string(),
            importance: 0.6,
            timestamp: "2026-08-05T00:00:00.000Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "manual".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn meta(event_id: &str) -> OutboxEventMeta {
        OutboxEventMeta {
            event_id: event_id.to_string(),
            object_class: "memory".to_string(),
            authority_class: "host".to_string(),
            source_store: "global".to_string(),
            source_partition: "default".to_string(),
        }
    }

    fn assert_reserved_resolved_class_refusal(error: MemoryError) {
        match error {
            MemoryError::InvalidArg(message) => assert!(
                message.contains("reserved resolved-conflict class prefix"),
                "unexpected refusal text: {message}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    fn count_memories(store: &MemoryStore, id: &str) -> i64 {
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .expect("count memories")
    }

    #[test]
    fn commit_lands_the_row_and_a_pending_event_with_a_destination_receipt() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let row = entry("outbox-commit-1", "outbox commit body one");
        let receipt = store
            .commit_with_outbox_event(&row, &meta("evt-commit-1"))
            .expect("commit must succeed");

        assert_eq!(receipt.event.event_id, "evt-commit-1");
        assert_eq!(receipt.event.object_id, "outbox-commit-1");
        assert_eq!(receipt.event.state, OutboxState::Pending);
        assert_eq!(receipt.event.object_class, "memory");
        assert_eq!(receipt.event.authority_class, "host");
        assert_eq!(receipt.event.source_store, "global");
        assert_eq!(receipt.event.source_partition, "default");
        assert_eq!(receipt.event.last_error_class, None);
        assert_eq!(receipt.event.source_revision, receipt.object_revision);

        let stored = store.get("outbox-commit-1").expect("get").expect("present");
        assert_eq!(
            receipt.event.payload_digest,
            outbox_payload_digest(&stored).expect("digest"),
            "the event must record the digest of the payload as stored"
        );
        assert_eq!(
            store.outbox_event("evt-commit-1").expect("read").unwrap(),
            receipt.event,
            "the receipt must equal what a later reader sees"
        );
    }

    #[test]
    fn commit_with_outbox_event_refuses_reserved_resolved_class_metadata() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");

        let mut object_class_meta = meta("evt-reserved-object-class");
        object_class_meta.object_class = "conflict_resolved_memory".to_string();
        let object_class_error = store
            .commit_with_outbox_event(
                &entry("outbox-reserved-object-class", "reserved object class"),
                &object_class_meta,
            )
            .expect_err("a caller object_class in the reserved namespace must be refused");
        assert_reserved_resolved_class_refusal(object_class_error);
        assert_eq!(count_memories(&store, "outbox-reserved-object-class"), 0);
        assert!(store
            .outbox_event("evt-reserved-object-class")
            .expect("read")
            .is_none());

        let mut authority_class_meta = meta("evt-reserved-authority-class");
        authority_class_meta.authority_class = "conflict_resolved_host".to_string();
        let authority_class_error = store
            .commit_with_outbox_event(
                &entry(
                    "outbox-reserved-authority-class",
                    "reserved authority class",
                ),
                &authority_class_meta,
            )
            .expect_err("a caller authority_class in the reserved namespace must be refused");
        assert_reserved_resolved_class_refusal(authority_class_error);
        assert_eq!(count_memories(&store, "outbox-reserved-authority-class"), 0);
        assert!(store
            .outbox_event("evt-reserved-authority-class")
            .expect("read")
            .is_none());
    }

    /// The atomicity clause, exercised through the production seam: the second
    /// commit's upsert succeeds and its event insert then fails on the
    /// duplicate `event_id`, so the failure lands exactly between the two
    /// writes. Neither may survive.
    #[test]
    fn a_failure_between_the_upsert_and_the_event_leaves_neither_half() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        store
            .commit_with_outbox_event(&entry("outbox-atomic-1", "first body"), &meta("evt-shared"))
            .expect("first commit");

        let error = store
            .commit_with_outbox_event(
                &entry("outbox-atomic-2", "second body"),
                &meta("evt-shared"),
            )
            .expect_err("a duplicate event_id must fail the commit");
        assert!(
            matches!(error, MemoryError::Duplicate(_)),
            "unexpected error variant: {error:?}"
        );

        assert_eq!(
            count_memories(&store, "outbox-atomic-2"),
            0,
            "the memory write must roll back with its failed event"
        );
        assert!(
            store.get("outbox-atomic-2").expect("get").is_none(),
            "no visible row may survive a rolled-back commit"
        );
        let events: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM memory_outbox_events", [], |row| {
                row.get(0)
            })
            .expect("count events");
        assert_eq!(events, 1, "only the first commit's event may exist");
        // The first commit is untouched by the second's rollback.
        assert_eq!(count_memories(&store, "outbox-atomic-1"), 1);
    }

    /// The mirror direction: when the *memory* half is refused, no event is
    /// left behind either.
    #[test]
    fn a_refused_memory_write_leaves_no_event() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let reserved = entry("wiki-rem:outbox-reserved", "reserved identity body");
        let error = store
            .commit_with_outbox_event(&reserved, &meta("evt-reserved"))
            .expect_err("a reserved identity must be refused");
        assert!(
            matches!(error, MemoryError::InvalidArg(_)),
            "unexpected error variant: {error:?}"
        );
        assert!(store.outbox_event("evt-reserved").expect("read").is_none());
    }

    /// tachi#1634's discipline at this seam: two byte-identical bodies must
    /// stay two rows with two events. If write-time near-duplicate merging
    /// ever ran here, the second event would announce the *first* row's id and
    /// the caller's object would have silently vanished.
    #[test]
    fn byte_identical_bodies_commit_as_two_objects_with_two_events() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let shared = "identical body that write-time consolidation would merge";
        let first = store
            .commit_with_outbox_event(&entry("outbox-dup-1", shared), &meta("evt-dup-1"))
            .expect("first commit");
        let second = store
            .commit_with_outbox_event(&entry("outbox-dup-2", shared), &meta("evt-dup-2"))
            .expect("second commit");

        assert_eq!(first.event.object_id, "outbox-dup-1");
        assert_eq!(second.event.object_id, "outbox-dup-2");
        assert_eq!(count_memories(&store, "outbox-dup-1"), 1);
        assert_eq!(count_memories(&store, "outbox-dup-2"), 1);
        for id in ["outbox-dup-1", "outbox-dup-2"] {
            let superseded_by: Option<String> = store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .expect("row must exist");
            assert!(
                superseded_by.is_none(),
                "the commit seam invented a supersession edge for {id}"
            );
        }
    }

    #[test]
    fn a_second_commit_of_the_same_object_records_the_new_revision_and_digest() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let first = store
            .commit_with_outbox_event(&entry("outbox-rev", "first body"), &meta("evt-rev-1"))
            .expect("first commit");
        let second = store
            .commit_with_outbox_event(&entry("outbox-rev", "second body"), &meta("evt-rev-2"))
            .expect("second commit");

        assert!(
            second.object_revision > first.object_revision,
            "an update must bump the revision the event records: {} -> {}",
            first.object_revision,
            second.object_revision
        );
        assert_ne!(
            first.event.payload_digest, second.event.payload_digest,
            "a changed body must change the payload digest"
        );
        // Both events survive: the outbox is a log, not a latest-value cell.
        assert!(store.outbox_event("evt-rev-1").expect("read").is_some());
        assert!(store.outbox_event("evt-rev-2").expect("read").is_some());
    }

    /// The digest is over the payload as *stored*, so a caller-side digest of
    /// a payload the write path normalizes will differ — and that difference
    /// is the signal, not a bug.
    #[test]
    fn digest_describes_the_stored_payload_not_the_submitted_one() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let mut submitted = entry("outbox-digest", "body with a tail");
        submitted.metadata = serde_json::json!({ "evidence_refs_v1": [{ "ref": "#1" }] });
        let receipt = store
            .commit_with_outbox_event(&submitted, &meta("evt-digest"))
            .expect("commit");

        let stored = store.get("outbox-digest").expect("get").expect("present");
        assert_eq!(
            receipt.event.payload_digest,
            outbox_payload_digest(&stored).expect("digest of stored")
        );
        assert_ne!(
            receipt.event.payload_digest,
            outbox_payload_digest(&submitted).expect("digest of submitted"),
            "the ordinary write path strips reserved metadata, so the stored payload differs"
        );
    }

    #[test]
    fn digest_ignores_usage_telemetry_but_tracks_body_and_lifecycle() {
        let base = entry("outbox-fields", "digest field body");
        let baseline = outbox_payload_digest(&base).expect("digest");

        let mut recalled = base.clone();
        recalled.access_count = 41;
        recalled.recall_count = 7;
        recalled.last_access = Some("2026-08-05T01:02:03.000Z".to_string());
        recalled.query_diversity = 3;
        recalled.scored_count = 9;
        recalled.last_use_at = Some("2026-08-05T01:02:03.000Z".to_string());
        assert_eq!(
            baseline,
            outbox_payload_digest(&recalled).expect("digest"),
            "reading a memory must not dirty its payload digest"
        );

        let mut vectored = base.clone();
        vectored.vector = Some(vec![0.25_f32; 8]);
        assert_eq!(
            baseline,
            outbox_payload_digest(&vectored).expect("digest"),
            "the embedding is a derived projection, not payload"
        );

        let digest_after = |mutate: &dyn Fn(&mut MemoryEntry)| {
            let mut changed = base.clone();
            mutate(&mut changed);
            outbox_payload_digest(&changed).expect("digest")
        };
        for (field, changed) in [
            (
                "text",
                digest_after(&|e| e.text = "different body".to_string()),
            ),
            ("archived", digest_after(&|e| e.archived = true)),
            ("revision", digest_after(&|e| e.revision = 9)),
            (
                "valid_until",
                digest_after(&|e| e.valid_until = Some("2026-09-01T00:00:00.000Z".to_string())),
            ),
            (
                "metadata",
                digest_after(&|e| e.metadata = serde_json::json!({ "k": 1 })),
            ),
            (
                "path",
                digest_after(&|e| e.path = "/scratch/elsewhere".to_string()),
            ),
        ] {
            assert_ne!(
                baseline, changed,
                "changing {field} must change the payload digest"
            );
        }
    }

    /// The composition seam: a replacement transaction that supersedes a
    /// predecessor and announces the successor does all of it in one commit.
    #[test]
    fn enqueue_composes_into_an_existing_supersession_transaction() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        store
            .commit_with_outbox_event(&entry("outbox-pred", "predecessor body"), &meta("evt-pred"))
            .expect("predecessor commit");

        let event = store
            .with_immutable_supersession_transaction(|replacement| {
                replacement.upsert(&entry("outbox-succ", "successor body"))?;
                replacement.claim_immutable_supersession("outbox-pred", "outbox-succ")?;
                replacement.enqueue_outbox_event("outbox-succ", &meta("evt-succ"))
            })
            .expect("composed replacement must commit");

        assert_eq!(event.state, OutboxState::Pending);
        assert_eq!(event.object_id, "outbox-succ");
        let stored = store.get("outbox-succ").expect("get").expect("present");
        assert_eq!(
            event.payload_digest,
            outbox_payload_digest(&stored).expect("digest"),
            "the composed event must digest the successor as stored"
        );
        let superseded_by: Option<String> = store
            .connection()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = 'outbox-pred'",
                [],
                |row| row.get(0),
            )
            .expect("predecessor row");
        assert_eq!(superseded_by.as_deref(), Some("outbox-succ"));
    }

    /// An injected failure *after* both writes in a composed transaction: the
    /// row, its supersession edge, and the event all roll back together.
    #[test]
    fn a_failure_after_a_composed_enqueue_rolls_back_every_half() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let result: Result<(), MemoryError> =
            store.with_immutable_supersession_transaction(|replacement| {
                replacement.upsert(&entry("outbox-composed", "composed body"))?;
                replacement.enqueue_outbox_event("outbox-composed", &meta("evt-composed"))?;
                Err(MemoryError::Internal(
                    "injected failure after both writes".to_string(),
                ))
            });
        assert!(
            result.is_err(),
            "the injected failure must fail the operation"
        );

        assert!(
            store.get("outbox-composed").expect("get").is_none(),
            "the memory write must not survive"
        );
        assert!(
            store.outbox_event("evt-composed").expect("read").is_none(),
            "the event must not survive"
        );
    }

    #[test]
    fn enqueue_for_an_object_the_transaction_never_wrote_is_refused() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let result: Result<db::OutboxEventRow, MemoryError> = store
            .with_immutable_supersession_transaction(|replacement| {
                replacement.enqueue_outbox_event("outbox-absent", &meta("evt-absent"))
            });
        assert!(
            matches!(result, Err(MemoryError::NotFound(_))),
            "unexpected result: {result:?}"
        );
        assert!(store.outbox_event("evt-absent").expect("read").is_none());
    }

    #[test]
    fn composed_enqueue_refuses_reserved_resolved_class_metadata() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let mut event = meta("evt-composed-reserved-class");
        event.object_class = "conflict_resolved_memory".to_string();

        let result: Result<db::OutboxEventRow, MemoryError> = store
            .with_immutable_supersession_transaction(|replacement| {
                replacement.upsert(&entry("outbox-composed-reserved-class", "composed body"))?;
                replacement.enqueue_outbox_event("outbox-composed-reserved-class", &event)
            });
        let error =
            result.expect_err("a composed enqueue class in the reserved namespace must be refused");
        assert_reserved_resolved_class_refusal(error);
        assert!(
            store
                .get("outbox-composed-reserved-class")
                .expect("get")
                .is_none(),
            "the memory write must roll back with its failed event"
        );
        assert!(store
            .outbox_event("evt-composed-reserved-class")
            .expect("read")
            .is_none());
    }

    /// The health read model over the public surface, walked across a state
    /// mix. The derivation itself is pinned by `db::outbox`'s tests; this
    /// proves the store wires through to it and that the fields move as a
    /// commit progresses.
    #[test]
    fn health_tracks_a_commit_through_the_states_it_can_honestly_report() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");

        let empty = store.outbox_health().expect("health");
        assert_eq!(empty.local_store_status, db::LocalStoreStatus::Healthy);
        assert_eq!(empty.remote_sync_status, db::RemoteSyncStatus::Unconfigured);
        assert_eq!(empty.pending_count, 0);
        assert_eq!(empty.oldest_pending_at, None);
        assert_eq!(empty.last_successful_sync, None);
        assert_eq!(empty.last_error_class, None);

        let first = store
            .commit_with_outbox_event(&entry("outbox-health-1", "one"), &meta("evt-health-1"))
            .expect("commit one");
        store
            .commit_with_outbox_event(&entry("outbox-health-2", "two"), &meta("evt-health-2"))
            .expect("commit two");

        let queued = store.outbox_health().expect("health");
        assert_eq!(queued.pending_count, 2);
        assert_eq!(
            queued.remote_sync_status,
            db::RemoteSyncStatus::Unconfigured,
            "the kernel has no configured remote transport"
        );
        assert_eq!(
            queued.oldest_pending_at.as_deref(),
            Some(first.event.created_at.as_str())
        );

        store
            .transition_outbox_event("evt-health-1", OutboxState::InFlight, None)
            .expect("in_flight");
        assert_eq!(
            store.outbox_health().expect("health").remote_sync_status,
            db::RemoteSyncStatus::Unconfigured,
            "the kernel has no configured remote transport"
        );

        store
            .transition_outbox_event("evt-health-1", OutboxState::Acknowledged, None)
            .expect("acknowledged");
        let synced = store.outbox_health().expect("health");
        assert_eq!(
            synced.last_successful_sync, None,
            "a local acknowledgement cannot fabricate remote success"
        );
        assert_eq!(synced.pending_count, 1);

        store
            .transition_outbox_event(
                "evt-health-2",
                OutboxState::Quarantined,
                Some("operator_hold"),
            )
            .expect("quarantined");
        let held = store.outbox_health().expect("health");
        assert_eq!(
            held.local_store_status,
            db::LocalStoreStatus::Quarantined {
                quarantined_count: 1
            },
            "quarantine is a local-store condition, not a remote one"
        );
        assert_eq!(held.remote_sync_status, db::RemoteSyncStatus::Unconfigured);
        assert_eq!(held.pending_count, 0);
        assert_eq!(held.oldest_pending_at, None);
        assert_eq!(held.last_error_class.as_deref(), Some("operator_hold"));
    }

    #[test]
    fn transition_outbox_event_refuses_reserved_resolved_error_class() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let receipt = store
            .commit_with_outbox_event(
                &entry("outbox-reserved-error-class", "reserved transition class"),
                &meta("evt-reserved-error-class"),
            )
            .expect("commit");

        let error = store
            .transition_outbox_event(
                "evt-reserved-error-class",
                OutboxState::Quarantined,
                Some("conflict_resolved_operator_hold"),
            )
            .expect_err("a caller error_class in the reserved namespace must be refused");
        assert_reserved_resolved_class_refusal(error);
        assert_eq!(
            store
                .outbox_event("evt-reserved-error-class")
                .expect("read")
                .unwrap(),
            receipt.event,
            "the reserved-prefix refusal must leave the event untouched"
        );
    }

    #[test]
    fn transitions_and_listing_reach_the_state_machine_through_the_store() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        store
            .commit_with_outbox_event(&entry("outbox-fsm-1", "one"), &meta("evt-fsm-1"))
            .expect("commit one");
        store
            .commit_with_outbox_event(&entry("outbox-fsm-2", "two"), &meta("evt-fsm-2"))
            .expect("commit two");

        let pending = store
            .list_outbox_events(OutboxState::Pending, 10)
            .expect("list pending");
        assert_eq!(
            pending
                .iter()
                .map(|row| row.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["evt-fsm-1", "evt-fsm-2"]
        );

        let error = store
            .transition_outbox_event("evt-fsm-1", OutboxState::Acknowledged, None)
            .expect_err("pending -> acknowledged must be refused");
        assert!(
            matches!(error, MemoryError::OutboxIllegalTransition { .. }),
            "unexpected error variant: {error:?}"
        );

        store
            .transition_outbox_event("evt-fsm-1", OutboxState::InFlight, None)
            .expect("pending -> in_flight");
        let acknowledged = store
            .transition_outbox_event("evt-fsm-1", OutboxState::Acknowledged, None)
            .expect("in_flight -> acknowledged");
        assert_eq!(acknowledged.state, OutboxState::Acknowledged);
        assert_eq!(
            store
                .list_outbox_events(OutboxState::Pending, 10)
                .expect("list pending")
                .len(),
            1
        );
    }
}
