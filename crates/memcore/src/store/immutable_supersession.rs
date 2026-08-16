//! Atomic, Rust-only mutation boundary for replacements that claim a
//! supersession edge and then perform dependent writes.
//!
//! This is intentionally not a generic SQL or arbitrary transaction API.
//! Callers can only claim an unset supersession edge, upsert a memory, archive
//! a claimed source, add a graph edge, or save a derived item. A failed claim
//! or any later mutation error drops the `BEGIN IMMEDIATE` transaction and
//! rolls every earlier mutation back.

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{Map, Value};

use crate::{
    db,
    error::MemoryError,
    types::{MemoryEdge, MemoryEntry},
    MemoryStore,
};

/// tachi#1645 (#1635 finding 2): bound the `superseded_by` chain walk
/// `claim_immutable_supersession` performs before installing a new edge. A
/// legitimate lineage should never need anywhere close to this many hops;
/// hitting the cap is treated as a refusal (see
/// `refuse_supersession_cycle`), not an unbounded scan.
const MAX_SUPERSESSION_CHAIN_WALK: u32 = 32;

/// Narrow mutation handle passed only inside
/// [`MemoryStore::with_immutable_supersession_transaction`].
pub struct ImmutableSupersessionTransaction<'tx> {
    tx: Transaction<'tx>,
    vec_available: bool,
    reserved_reference_write: db::ReservedReferenceWriteFlag,
}

impl<'tx> ImmutableSupersessionTransaction<'tx> {
    fn validate_memory_write(entry: &MemoryEntry) -> Result<(), MemoryError> {
        crate::path_router::validate_retired_sticky_write(&entry.path, &entry.category)
            .map_err(|error| MemoryError::InvalidArg(error.to_string()))
    }

    /// Claim `source_id -> target_id` exactly once.
    ///
    /// A same-edge replay and a conflicting edge both fail loudly, so callers
    /// cannot accidentally run side effects as though their requested edge won.
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
    /// - `foundry_runtime_ops/daily_distill/persist.rs:174`
    ///   (`claim_distilled_sources`) — target is `distill_entry.id`, only
    ///   reached after `replacement.insert_if_absent(entry)` already
    ///   returned `InsertMemoryResult::default` (fresh row) earlier in the
    ///   same transaction; the `Existing` branch returns before ever
    ///   calling this method.
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
    ) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        // Cycle guard first: it gives the more specific diagnosis when a
        // target is already-superseded *and* the chain closes back onto
        // `source_id` (see `claim_immutable_supersession_permits_a_two_hop_cycle_finding`'s
        // successor test below). Eligibility second: it catches every other
        // way a target can be retired (archived, or superseded by something
        // that does NOT lead back to `source_id`).
        self.refuse_supersession_cycle(source_id, target_id)?;
        self.refuse_ineligible_supersession_target(target_id)?;
        let changed = db::supersede_memory_within_tx(&self.tx, source_id, target_id)?;
        if !changed {
            return Err(MemoryError::InvalidArg(format!(
                "immutable supersession CAS refused for {source_id} -> {target_id}"
            )));
        }
        Ok(())
    }

    /// tachi#1645 (#1635 finding 2): walk `target_id`'s `superseded_by`
    /// chain forward, bounded at [`MAX_SUPERSESSION_CHAIN_WALK`] hops. A row
    /// that does not exist yet — e.g. a target a caller is about to
    /// materialize later in this same `BEGIN IMMEDIATE` transaction, such as
    /// Wiki ingest's replacement row (`persist_wiki_ingest_entry` claims
    /// each predecessor onto the NEW entry's id before upserting it) — has
    /// no chain and passes trivially; the surrounding transaction still
    /// guarantees that id either gets created before commit or this claim
    /// rolls back with it.
    ///
    /// Returns a distinct, differently-worded error for "the chain closes
    /// back onto `source_id`" (a genuine cycle) vs "the chain did not
    /// terminate within the depth cap" (refuse rather than risk an
    /// undetected cycle past the cap) — both refuse the claim.
    fn refuse_supersession_cycle(
        &self,
        source_id: &str,
        target_id: &str,
    ) -> Result<(), MemoryError> {
        let mut current = target_id.to_string();
        for _ in 0..MAX_SUPERSESSION_CHAIN_WALK {
            let next: Option<String> = self
                .tx
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = ?1",
                    [current.as_str()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            match next {
                None => return Ok(()),
                Some(next_id) if next_id == source_id => {
                    return Err(MemoryError::InvalidArg(format!(
                        "immutable supersession refused: {source_id} -> {target_id} would \
                         close a superseded_by cycle back to {source_id}"
                    )));
                }
                Some(next_id) => current = next_id,
            }
        }
        Err(MemoryError::InvalidArg(format!(
            "immutable supersession refused: {source_id} -> {target_id}'s superseded_by chain \
             did not terminate within {MAX_SUPERSESSION_CHAIN_WALK} hops; refusing rather than \
             risk an undetected cycle past the depth cap"
        )))
    }

    /// tachi#1645 (#1635 finding 1): a target must be active + eligible
    /// (not archived, not itself already superseded) to receive a new
    /// predecessor — the mechanism must enforce this itself, not rely on
    /// caller self-defense (some callers already re-check with
    /// `memory_is_active_unsuperseded` before claiming; `apply_lifecycle_action`'s
    /// "supersede"/"merge_into" arms in
    /// `tachi-server/facade_memory_ops/consolidate_ops.rs` did not).
    ///
    /// A target with no row yet is not "retired" and is left eligible for
    /// the same claim-before-materialize reason documented on
    /// `refuse_supersession_cycle` above.
    fn refuse_ineligible_supersession_target(&self, target_id: &str) -> Result<(), MemoryError> {
        let retired = self.tx.query_row(
            "SELECT COUNT(*) FROM memories WHERE id = ?1 AND (archived = 1 OR superseded_by IS NOT NULL)",
            [target_id],
            |row| row.get::<_, i64>(0),
        )? > 0;
        if retired {
            return Err(MemoryError::InvalidArg(format!(
                "immutable supersession target ineligible: {target_id} is archived or already \
                 superseded"
            )));
        }
        Ok(())
    }

    /// Persist an entry inside the replacement transaction.
    pub fn upsert(&mut self, entry: &MemoryEntry) -> Result<(), MemoryError> {
        Self::validate_memory_write(entry)?;
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
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
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
            };
            let result = operation(&mut replacement)?;
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
    /// way the raw `db::supersede_memory` CAS does — but here a refused CAS
    /// becomes a loud `Err`, not a silent `Ok(false)`, per the doc comment on
    /// `claim_immutable_supersession` above ("A same-edge replay and a
    /// conflicting edge both fail loudly").
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
            error.to_string().contains("CAS refused"),
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

    /// tachi#1635 (#1632 conformance, item 7): applying the identical
    /// supersession a second time must not silently no-op through the
    /// transaction wrapper — `claim_immutable_supersession` turns the raw
    /// CAS's `Ok(false)` (see `supersede_memory_refuses_self_supersession`-
    /// style callers in `db::memory_crud`) into an explicit `Err`. This is
    /// the "or explicit refusal" half of item 7 for the same-edge-replay
    /// case; `merge_into_for_project_refuses_conflicting_immutable_supersession_without_side_effects`
    /// (tachi-server consolidate_lifecycle tests) already covers the
    /// conflicting-edge half at this layer.
    #[test]
    fn claim_immutable_supersession_same_edge_replay_is_refused_not_silently_reapplied() {
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

        let error = store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("replay-source", "replay-target")
            })
            .expect_err("identical replay must refuse loudly, not silently succeed again");
        assert!(
            error.to_string().contains("CAS refused"),
            "unexpected error: {error}"
        );
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
            error.to_string().contains("target ineligible"),
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

    /// tachi#1635 (#1632 conformance, item 10) — FINDING, marked `#[ignore]`
    /// because it pins the SPEC (a partition/authority boundary refusal) that
    /// does not exist in the mechanism today, not current behavior.
    /// `ImmutableSupersessionTransaction` carries no partition/project/
    /// authority field at all (see the struct definition above); `grep -rn
    /// '\bpartition\b' crates/memcore/src` only turns up SQL window-function
    /// `PARTITION BY` clauses in `db/schema/ddl.rs` and `db/stats_gc.rs`,
    /// never a memory-admission/authority concept. `MemoryEntry::scope`
    /// ("general"/"project"/...) is the closest candidate field and is never
    /// read by `claim_immutable_supersession` or `db::supersede_memory`. The
    /// only boundary that exists is structural: source/target ids are looked
    /// up on ONE SQLite connection (one project DB or the global DB), so a
    /// literal cross-database supersession can't be expressed through this
    /// API — but nothing stops two rows in the SAME connection with
    /// different `scope`/`domain`/project-tag values from superseding each
    /// other. Left `#[ignore]`d per #1635 task instructions ("write the test
    /// #[ignore]d with a comment naming the gap"); not fixed here.
    #[test]
    #[ignore = "tachi#1635 finding: no partition/authority boundary check exists on claim_immutable_supersession"]
    fn claim_immutable_supersession_refuses_cross_partition_supersession() {
        let mut store = MemoryStore::open_in_memory().expect("open memory store");
        let mut source = fixture_entry("partition-a-source");
        source.scope = "project:alpha".to_string();
        let mut target = fixture_entry("partition-b-target");
        target.scope = "project:beta".to_string();
        store
            .insert_if_absent(&source)
            .expect("seed alpha-partition source");
        store
            .insert_if_absent(&target)
            .expect("seed beta-partition target");

        let result = store.with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("partition-a-source", "partition-b-target")
        });
        assert!(
            result.is_err(),
            "a source and target in different admitted partitions must refuse, \
             not silently claim across the boundary: {result:?}"
        );
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
