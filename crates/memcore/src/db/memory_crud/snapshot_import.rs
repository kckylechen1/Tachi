//! Transaction-scoped row writer and readback for snapshot import
//! (tachi#1607).
//!
//! This is deliberately **not** the ordinary upsert body. `upsert_within_tx`
//! exists to apply a *write*: it stamps `created_at`/`updated_at` with wall
//! clock, bumps `revision` on conflict, defaults `valid_from`, normalizes
//! timestamps, and runs write-time near-duplicate consolidation which can
//! stamp `superseded_by` on a row the caller never asked to supersede. Every
//! one of those behaviors is correct for a write and wrong for a snapshot: a
//! migration that ran them would preserve row count and visible `MemoryEntry`
//! fields while changing which rows satisfy the canonical-active predicate
//! `archived = 0 AND superseded_by IS NULL`, i.e. it would change default
//! recall.
//!
//! What is shared with the ordinary path, on purpose, is everything that is
//! *not* lifecycle policy: the reserved-identity refusals
//! ([`super::refuse_reserved_write_identity`] — the same function
//! `upsert_prepared_within_tx` calls, so the blank-id / `anchor:` /
//! `wiki-rem:` / Wiki-operation-log refusals are byte-identical, not a copy),
//! the entity canonicalization ([`super::canonical_entities_json`]), the FTS
//! + symbolic-FTS projection ([`super::sync_memories_fts`]), and the vector
//! blob encoding ([`crate::db::sqlite_vec::serialize_f32`]).

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::error::MemoryError;
use crate::store::snapshot_import::PortableImportEntry;

use super::{canonical_entities_json, refuse_reserved_write_identity, sync_memories_fts};
use crate::db::sqlite_vec::serialize_f32;

/// One destination row's lifecycle projection, read back from `memories`
/// **after** the import wrote it.
///
/// Field order is alphabetical and load-bearing: this struct is serialized
/// directly (never through `serde_json::Value`) to build
/// `PortableImportReceipt::lifecycle_checksum`, so the emitted JSON object key
/// order is this declaration order regardless of any `serde_json` feature
/// selection downstream. Do not reorder without changing the documented
/// canonicalization on [`crate::PortableImportReceipt`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SnapshotLifecycleRow {
    pub archived: bool,
    pub created_at: String,
    pub id: String,
    pub revision: i64,
    pub superseded_by: Option<String>,
    pub updated_at: String,
    pub valid_until: Option<String>,
}

/// One destination row's vector projection identity, read back from
/// `memories_vec` after the import wrote it. Alphabetical field order, same
/// rule as [`SnapshotLifecycleRow`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SnapshotVectorRow {
    pub id: String,
    pub vector_sha256: String,
}

/// Insert one snapshot row plus its FTS/symbolic/vector projections inside a
/// caller-owned transaction, writing every column verbatim.
///
/// Refusals, in order: the shared reserved-identity guard, then an
/// already-present `id`. The id check is what makes this an *import* rather
/// than a raw-write escape — snapshot import targets a fresh destination, and
/// silently rewriting an existing row through `ON CONFLICT DO UPDATE` (what
/// ordinary upsert does) would be an unaudited overwrite of a row the caller
/// never named.
///
/// `idless_identity` is deliberately left NULL: `MemoryEntry` does not carry
/// it, so there is no supplied value to preserve, and inventing one would
/// claim an identity the source never granted.
pub(crate) fn import_snapshot_row_within_tx(
    tx: &rusqlite::Transaction<'_>,
    import: &PortableImportEntry,
    vec_available: bool,
) -> Result<(), MemoryError> {
    let entry = &import.entry;
    let path = crate::path_router::normalize_path(&entry.path);
    refuse_reserved_write_identity(&entry.id, &path, &entry.topic, false, false)?;

    if entry.vector.is_some() && !vec_available {
        return Err(MemoryError::InvalidArg(format!(
            "snapshot import of id '{}' carries a vector but this store has no vector \
             projection table; importing it would silently drop the embedding",
            entry.id
        )));
    }

    if memory_row_exists_within_tx(tx, &entry.id)? {
        return Err(MemoryError::Duplicate(format!(
            "snapshot import target already contains id '{}'; import requires a destination \
             without the imported ids",
            entry.id
        )));
    }

    let metadata_json = serde_json::to_string(&entry.metadata)?;
    let keywords_json = serde_json::to_string(&entry.keywords)?;
    let entities_json = canonical_entities_json(entry)?;

    let rows_written = tx.execute(
        r#"INSERT INTO memories
              (id, path, summary, text, importance,
               timestamp, valid_from, valid_until, category, topic, keywords, entities,
               source, scope, archived, created_at, updated_at,
               access_count, scored_count, last_access, last_use_at, revision, metadata,
               superseded_by, retention_policy, domain, recall_count, query_diversity, tier)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29)"#,
        params![
            entry.id,
            &path,
            &entry.summary,
            &entry.text,
            entry.importance,
            &entry.timestamp,
            &entry.valid_from,
            &entry.valid_until,
            &entry.category,
            &entry.topic,
            keywords_json,
            entities_json,
            &entry.source,
            &entry.scope,
            entry.archived,
            &import.created_at,
            &import.updated_at,
            entry.access_count,
            entry.scored_count,
            &entry.last_access,
            &entry.last_use_at,
            entry.revision,
            metadata_json,
            &import.superseded_by,
            &entry.retention_policy,
            &entry.domain,
            entry.recall_count,
            entry.query_diversity,
            &entry.tier,
        ],
    )?;
    if rows_written != 1 {
        return Err(MemoryError::Internal(format!(
            "snapshot import wrote {rows_written} rows for id '{}'",
            entry.id
        )));
    }

    let keywords_joined = entry.keywords.join(" ");
    let mut entities = entry.entities.clone();
    crate::types::fold_person_names_into_entities(&mut entities, entry.persons.clone());
    let entities_joined = entities.join(" ");
    sync_memories_fts(
        tx,
        &entry.id,
        &path,
        &entry.summary,
        &entry.text,
        &keywords_joined,
        &entities_joined,
    )?;

    if let Some(vector) = &entry.vector {
        let blob = serialize_f32(vector);
        // vec0 virtual tables do NOT support ON CONFLICT / UPSERT; the id is
        // known-absent above, so a bare INSERT is enough.
        tx.execute(
            "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
            params![entry.id, blob],
        )?;
    }

    Ok(())
}

/// True when `memories` already carries `id`.
pub(crate) fn memory_row_exists_within_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> Result<bool, MemoryError> {
    Ok(tx
        .query_row("SELECT 1 FROM memories WHERE id = ?1", params![id], |_| {
            Ok(())
        })
        .optional()?
        .is_some())
}

/// Read one row's lifecycle projection back out of `memories`.
///
/// The receipt hashes *this* — the destination's post-write state — never the
/// caller's input structs. A checksum computed from the inputs would be a
/// tautology that stays green even if the writer dropped `superseded_by`.
pub(crate) fn read_snapshot_lifecycle_row_within_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> Result<SnapshotLifecycleRow, MemoryError> {
    tx.query_row(
        "SELECT id, revision, archived, valid_until, created_at, updated_at, superseded_by
         FROM memories WHERE id = ?1",
        params![id],
        |row| {
            Ok(SnapshotLifecycleRow {
                archived: row.get::<_, i64>(2)? != 0,
                created_at: row.get(4)?,
                id: row.get(0)?,
                revision: row.get(1)?,
                superseded_by: row.get(6)?,
                updated_at: row.get(5)?,
                valid_until: row.get(3)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| {
        MemoryError::Internal(format!(
            "snapshot import cannot read back the row it just wrote for id '{id}'"
        ))
    })
}

/// Read one row's stored embedding blob back out of `memories_vec`.
pub(crate) fn read_snapshot_vector_blob_within_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    vec_available: bool,
) -> Result<Option<Vec<u8>>, MemoryError> {
    if !vec_available {
        return Ok(None);
    }
    Ok(tx
        .query_row(
            "SELECT embedding FROM memories_vec WHERE id = ?1",
            params![id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?)
}
