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
//!   blob encoding ([`crate::db::sqlite_vec::serialize_f32`]).

use rusqlite::{params, OptionalExtension, Statement};
use serde::Serialize;

use crate::error::MemoryError;
use crate::store::snapshot_import::PortableImportEntry;

use super::{canonical_entities_json, refuse_reserved_write_identity, sync_memories_fts};
use crate::db::sqlite_vec::serialize_f32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotFtsMode {
    Synchronize,
    InsertOnly,
}

/// Choose once after BEGIN IMMEDIATE. Schema/trigger drift or existing rows
/// retain the old synchronization path; actual inspection errors propagate.
pub(crate) fn snapshot_import_fts_mode(
    tx: &rusqlite::Transaction<'_>,
) -> Result<SnapshotFtsMode, MemoryError> {
    if !crate::db::schema::current_store_admission::has_canonical_snapshot_fts(tx)? {
        return Ok(SnapshotFtsMode::Synchronize);
    }
    let occupied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.memories)
             OR EXISTS(SELECT 1 FROM main.memories_fts)
             OR EXISTS(SELECT 1 FROM main.memories_symbolic_fts)",
        [],
        |row| row.get(0),
    )?;
    if occupied {
        return Ok(SnapshotFtsMode::Synchronize);
    }
    match crate::db::open::validate_persistent_trigger_inventory(tx, true) {
        Ok(()) => Ok(SnapshotFtsMode::InsertOnly),
        Err(MemoryError::InvalidArg(_)) => Ok(SnapshotFtsMode::Synchronize),
        Err(error) => Err(error),
    }
}

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

/// Insert snapshot rows plus their FTS/symbolic/vector projections inside a
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
pub(crate) fn import_snapshot_rows_within_tx(
    tx: &rusqlite::Transaction<'_>,
    entries: &[PortableImportEntry],
    vec_available: bool,
    fts_mode: SnapshotFtsMode,
) -> Result<(), MemoryError> {
    // Keep the SQL literal in this writer for the dual-projection census.
    const MEMORY_INSERT_SQL: &str = r#"INSERT INTO memories
              (id, path, summary, text, importance,
               timestamp, valid_from, valid_until, category, topic, keywords, entities,
               source, scope, archived, created_at, updated_at,
               access_count, scored_count, last_access, last_use_at, revision, metadata,
               superseded_by, retention_policy, domain, recall_count, query_diversity, tier)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29)"#;
    // Ordinary statements finalize here; cached DML could outlive write authority.
    let mut memory_insert: Option<Statement<'_>> = None;
    for import in entries {
        let entry = &import.entry;
        let path = crate::path_router::normalize_path(&entry.path);
        refuse_reserved_write_identity(&entry.id, &path, &entry.topic, false, false)?;
        super::refuse_retired_projection_write(tx, &entry.id)?;

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

        let insert = match &mut memory_insert {
            Some(statement) => statement,
            slot @ None => slot.insert(tx.prepare(MEMORY_INSERT_SQL)?),
        };
        let rows_written = insert.execute(params![
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
        ])?;
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
        #[cfg(test)]
        let fts_timing = crate::db::snapshot_import_measurement::span(
            crate::db::snapshot_import_measurement::Phase::Fts,
        );
        let project = match fts_mode {
            SnapshotFtsMode::Synchronize => sync_memories_fts,
            SnapshotFtsMode::InsertOnly => crate::db::fts_projection::insert_snapshot_fts,
        };
        project(
            tx,
            &entry.id,
            &path,
            &entry.summary,
            &entry.text,
            &keywords_joined,
            &entities_joined,
        )?;
        #[cfg(test)]
        drop(fts_timing);

        if let Some(vector) = &entry.vector {
            let blob = serialize_f32(vector);
            // vec0 virtual tables do NOT support ON CONFLICT / UPSERT; the id is
            // known-absent above, so a bare INSERT is enough.
            tx.execute(
                "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
                params![entry.id, blob],
            )?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbOpenContext, MemoryStore, StoreProfile};
    use std::ffi::CStr;

    const STAMP: &str = "2026-01-02T03:04:05.000Z";

    fn entry(id: &str) -> PortableImportEntry {
        PortableImportEntry {
            entry: serde_json::from_value(serde_json::json!({
                "id": id, "path": "/snapshot/statement", "summary": "",
                "text": "snapshot statement scope", "importance": 0.5,
                "timestamp": STAMP, "valid_from": STAMP, "category": "fact",
                "topic": "snapshot", "keywords": [], "entities": [],
                "source": "migration", "scope": "project", "archived": false,
                "revision": 1, "metadata": {}, "tier": "raw"
            }))
            .unwrap(),
            created_at: STAMP.to_string(),
            updated_at: STAMP.to_string(),
            superseded_by: None,
        }
    }

    fn fixture() -> (tempfile::TempDir, MemoryStore) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.db");
        let context = DbOpenContext::create_fresh().with_profile(StoreProfile::PortableKernel);
        let store = MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap();
        (directory, store)
    }

    fn row_count(store: &MemoryStore) -> i64 {
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .unwrap()
    }

    fn retained_memory_inserts(conn: &rusqlite::Connection) -> Vec<String> {
        let mut inserts = Vec::new();
        // SAFETY: this owned connection is borrowed for read-only enumeration.
        // SQLite owns each VM and SQL pointer. Copy SQL before any cache access;
        // never step or finalize the raw statements while enumerating them.
        unsafe {
            let handle = conn.handle();
            let mut statement = rusqlite::ffi::sqlite3_next_stmt(handle, std::ptr::null_mut());
            while !statement.is_null() {
                let pointer = rusqlite::ffi::sqlite3_sql(statement);
                if !pointer.is_null() {
                    let sql = CStr::from_ptr(pointer).to_str().unwrap();
                    let normalized = sql.trim_start().to_ascii_lowercase();
                    if normalized
                        .strip_prefix("insert into memories")
                        .and_then(|suffix| suffix.chars().next())
                        .is_some_and(|next| next.is_ascii_whitespace() || next == '(')
                    {
                        inserts.push(sql.to_string());
                    }
                }
                statement = rusqlite::ffi::sqlite3_next_stmt(handle, statement);
            }
        }
        inserts
    }

    fn execute_probe(statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            "outside-import",
            "/snapshot/statement",
            "",
            "ordinary unprivileged probe",
            0.5,
            STAMP,
            STAMP,
            None::<&str>,
            "fact",
            "snapshot",
            "[]",
            "[]",
            "migration",
            "project",
            false,
            STAMP,
            STAMP,
            0_i64,
            0_i64,
            None::<&str>,
            None::<&str>,
            1_i64,
            "{}",
            None::<&str>,
            None::<&str>,
            None::<&str>,
            0_i64,
            0_i64,
            "raw",
        ])
    }

    fn assert_authorization_denied(error: rusqlite::Error) {
        assert!(
            matches!(&error, rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ErrorCode::AuthorizationForStatementDenied),
            "expected the SQLite authorizer to refuse the statement: {error:?}"
        );
    }

    fn assert_insert_authority_is_released(store: &MemoryStore) {
        let conn = store.connection();
        // Capture the actual unexpanded SQL before any other cache access.
        let retained = retained_memory_inserts(conn);
        let count = row_count(store);
        let generation = store.search_generation().unwrap();

        for sql in &retained {
            let result = conn
                .prepare_cached(sql)
                .and_then(|mut statement| execute_probe(&mut statement));
            assert_authorization_denied(
                result.expect_err(
                    "retained snapshot INSERT executed outside its write authorization",
                ),
            );
        }
        assert!(
            retained.is_empty(),
            "snapshot INSERT must finalize at batch exit: {retained:?}"
        );

        let error = conn
            .prepare("INSERT INTO memories(id, text) VALUES (?1, ?2)")
            .expect_err("ordinary raw INSERT must still require write authorization");
        assert_authorization_denied(error);
        assert_eq!(row_count(store), count);
        assert_eq!(store.search_generation().unwrap(), generation);
        assert!(conn.is_autocommit());
    }

    #[test]
    fn snapshot_insert_authority_does_not_escape_successful_batch() {
        let (_directory, mut store) = fixture();
        let receipt = store.import_snapshot_batch(&[entry("imported")]).unwrap();
        assert_eq!(receipt.rows_imported, 1);
        assert_insert_authority_is_released(&store);
        assert_eq!(row_count(&store), 1);
    }

    #[test]
    fn snapshot_insert_authority_does_not_escape_failed_batch() {
        let (_directory, mut store) = fixture();
        let generation = store.search_generation().unwrap();
        let error = store
            .import_snapshot_batch(&[entry("written-first"), entry("anchor:blocked")])
            .expect_err("the later reserved identity must roll back the batch");
        assert!(matches!(error, MemoryError::InvalidArg(_)));
        assert_insert_authority_is_released(&store);
        assert_eq!(row_count(&store), 0);
        assert_eq!(store.search_generation().unwrap(), generation);
    }

    #[test]
    fn first_row_refusals_precede_insert_preparation() {
        let (directory, mut store) = fixture();
        store.import_snapshot_batch(&[entry("existing")]).unwrap();
        let generation = store.search_generation().unwrap();
        let raw = rusqlite::Connection::open(directory.path().join("snapshot.db")).unwrap();
        raw.execute_batch("ALTER TABLE main.memories DROP COLUMN scored_count")
            .unwrap();
        drop(raw);
        crate::db::open::validate_persistent_trigger_inventory(store.connection(), true).unwrap();

        for id in ["", "anchor:blocked"] {
            let error = store
                .import_snapshot_batch(&[entry(id)])
                .expect_err("identity refusal must precede INSERT preparation");
            assert!(
                matches!(error, MemoryError::InvalidArg(_)),
                "unexpected first-row refusal for {id:?}: {error:?}"
            );
            assert!(store.connection().is_autocommit());
        }
        let duplicate = store
            .import_snapshot_batch(&[entry("existing")])
            .expect_err("duplicate refusal must precede INSERT preparation");
        assert!(matches!(duplicate, MemoryError::Duplicate(_)));
        assert!(store.connection().is_autocommit());

        let error = store
            .import_snapshot_batch(&[entry("new-valid-id")])
            .expect_err("a valid new row must reach the damaged INSERT schema");
        assert!(
            matches!(&error, MemoryError::Sqlite(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("no column named scored_count")),
            "fixture must fail at INSERT preparation: {error:?}"
        );
        assert!(store.connection().is_autocommit());
        assert_eq!(row_count(&store), 1);
        assert_eq!(store.search_generation().unwrap(), generation);
        let text: String = store
            .connection()
            .query_row(
                "SELECT text FROM memories WHERE id = 'existing'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(text, entry("existing").entry.text);
        for table in ["memories_fts", "memories_symbolic_fts"] {
            let count: i64 = store
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1, "failed imports must preserve {table}");
        }
    }
}
