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
        if entry.id.starts_with("wiki-rem:") {
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
            false,
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
        allow_near_duplicate_merge: bool,
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
            allow_near_duplicate_merge,
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
