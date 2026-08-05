//! Atomic, Rust-only mutation boundary for replacements that claim a
//! supersession edge and then perform dependent writes.
//!
//! This is intentionally not a generic SQL or arbitrary transaction API.
//! Callers can only claim an unset supersession edge, upsert a memory, archive
//! a claimed source, add a graph edge, or save a derived item. A failed claim
//! or any later mutation error drops the `BEGIN IMMEDIATE` transaction and
//! rolls every earlier mutation back.

use rusqlite::{Transaction, TransactionBehavior};
use serde_json::{Map, Value};

use crate::{
    db,
    error::MemoryError,
    types::{MemoryEdge, MemoryEntry},
    MemoryStore,
};

/// Narrow mutation handle passed only inside
/// [`MemoryStore::with_immutable_supersession_transaction`].
pub struct ImmutableSupersessionTransaction<'tx> {
    tx: Transaction<'tx>,
    vec_available: bool,
    reserved_reference_write: db::ReservedReferenceWriteFlag,
}

impl<'tx> ImmutableSupersessionTransaction<'tx> {
    /// Claim `source_id -> target_id` exactly once.
    ///
    /// A same-edge replay and a conflicting edge both fail loudly, so callers
    /// cannot accidentally run side effects as though their requested edge won.
    pub fn claim_immutable_supersession(
        &mut self,
        source_id: &str,
        target_id: &str,
    ) -> Result<(), MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let changed = db::supersede_memory(&self.tx, source_id, target_id)?;
        if !changed {
            return Err(MemoryError::InvalidArg(format!(
                "immutable supersession CAS refused for {source_id} -> {target_id}"
            )));
        }
        Ok(())
    }

    /// Persist an entry inside the replacement transaction.
    pub fn upsert(&mut self, entry: &MemoryEntry) -> Result<(), MemoryError> {
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
        if !db::archive_memory(&self.tx, source_id)? {
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

    /// tachi#1635 (#1632 conformance, item 2) — FINDING, not a passing
    /// conformance pin: this test characterizes CURRENT behavior, which
    /// contradicts the acceptance item. `claim_immutable_supersession` ->
    /// `db::supersede_memory` (`crates/memcore/src/db/memory_crud.rs:3869-
    /// 3876`) only guards `WHERE id = ?3 AND superseded_by IS NULL` on the
    /// SOURCE row; it never reads the TARGET's `archived`/`superseded_by`
    /// state. Some callers self-defend (e.g.
    /// `crates/tachi-server/src/wiki_ops/ingest.rs:757` and
    /// `crates/tachi-server/src/foundry_runtime_ops/wiki_evolver.rs:779,815`
    /// call `memory_is_active_unsuperseded` on their target before claiming),
    /// but the mechanism itself does not enforce it — a caller that omits
    /// that check (e.g. `apply_lifecycle_action`'s "supersede"/"merge_into"
    /// arms in `crates/tachi-server/src/facade_memory_ops/consolidate_ops.rs`,
    /// which only call `refuse_if_protected` on the SOURCE) can point a fresh
    /// source at an already-archived, already-superseded target and the claim
    /// succeeds. Reported per #1635 task instructions ("target must be
    /// active+eligible... if not, FINDING"); not fixed here (edit-only leaf).
    #[test]
    fn claim_immutable_supersession_does_not_gate_target_eligibility_finding() {
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
        // claim under test — it is neither active nor eligible by the
        // acceptance item's own definition.
        store
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession(
                    "ineligible-target",
                    "ineligible-target-canonical",
                )?;
                operation.archive_claimed_source("ineligible-target")
            })
            .expect("pre-condition: target becomes archived+superseded");

        let result = store.with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("source-onto-dead-target", "ineligible-target")
        });
        assert!(
            result.is_ok(),
            "FINDING (tachi#1635 item 2): expected the mechanism to refuse \
             superseding onto an archived/already-superseded target, but it \
             succeeded: {result:?}"
        );
    }

    /// tachi#1635 (#1632 conformance, item 3) — FINDING, not a passing
    /// conformance pin: the mutation primitive itself performs no chain walk.
    /// `crates/memcore/src/recall_coverage_tests.rs::stored_supersession_cycle_still_fails_content_free_end_to_end`
    /// already proves the SAME thing through the public `MemoryStore::supersede_memory`
    /// wrapper and shows a downstream reader (`run_recall_coverage_probe`)
    /// detects and fails closed on the resulting cycle. This test pins the
    /// same absence of a guard directly at the `ImmutableSupersessionTransaction`
    /// mechanism the task names as canonical: A -> B then B -> A both succeed
    /// with no chain-walk refusal anywhere in `claim_immutable_supersession` /
    /// `db::supersede_memory`. Reported per #1635 task instructions ("no
    /// cycle creatable... if no chain check exists in code, that's a
    /// finding"); not fixed here (edit-only leaf).
    #[test]
    fn claim_immutable_supersession_permits_a_two_hop_cycle_finding() {
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

        let b_to_a = store.with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("cycle-b", "cycle-a")
        });
        assert!(
            b_to_a.is_ok(),
            "FINDING (tachi#1635 item 3): expected B -> A to refuse after \
             A -> B (cycle), but it succeeded: {b_to_a:?}"
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
