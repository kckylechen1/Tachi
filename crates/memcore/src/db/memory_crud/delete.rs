//! Exact-ID deletion and durable continuity projection retirement (#2299).
//!
//! A retirement is local to this store and identity. The source event ledger
//! remains append-only; replay admission uses the marker, not event removal.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{db::StoreProfile, error::MemoryError, types::ProjectionKind};

use super::super::{common::now_utc_iso, state::CONTINUITY_PROJECTION_RETIREMENT_NAMESPACE};
use super::{
    delete_memories_symbolic_fts, refuse_reserved_rem_operation_mutation,
    refuse_retired_sticky_row_within_tx,
};

/// Presence is authoritative, even if a damaged marker payload cannot be read.
/// Generic state writers and TTL maintenance cannot create or clear this state.
pub fn is_projection_retired(conn: &Connection, id: &str) -> Result<bool, MemoryError> {
    if !id.starts_with("projection-") {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM hard_state WHERE namespace = ?1 AND key = ?2)",
        params![CONTINUITY_PROJECTION_RETIREMENT_NAMESPACE, id],
        |row| row.get(0),
    )?)
}

/// Call only inside the same writer transaction as the memory INSERT/UPSERT.
/// An earlier projector read cannot admit a write after a concurrent delete.
pub(crate) fn refuse_retired_projection_write(
    tx: &Connection,
    id: &str,
) -> Result<(), MemoryError> {
    if is_projection_retired(tx, id)? {
        return Err(MemoryError::ProjectionRetired {
            memory_id: id.into(),
        });
    }
    Ok(())
}

fn retire_projection_within_tx(tx: &Connection, id: &str) -> Result<(), MemoryError> {
    // Recognize the producer's source and identity, never a broad path prefix.
    // Metadata is not authority: deleting an ordinary /timeline row must not
    // permanently reserve its ID, and source events are never touched here.
    let recognized_id = ProjectionKind::ALL.iter().any(|kind| {
        let prefix = format!("projection-{}-", kind.as_str());
        id.strip_prefix(prefix.as_str())
            .is_some_and(|key| !key.is_empty())
    });
    if !recognized_id {
        return Ok(());
    }
    let source: Option<String> = tx
        .query_row("SELECT source FROM memories WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    if source.as_deref() != Some("external:tachi_event_projection") {
        return Ok(());
    }
    let now = now_utc_iso();
    let marker = serde_json::json!({
        "schema_version": 1,
        "memory_id": id,
        "retired_at": now,
    })
    .to_string();
    tx.execute(
        "INSERT INTO hard_state(namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(namespace, key) DO NOTHING",
        params![CONTINUITY_PROJECTION_RETIREMENT_NAMESPACE, id, marker, now],
    )?;
    Ok(())
}

// ─── DELETE ───────────────────────────────────────────────────────────────────

/// Delete a memory entry by ID from main table, FTS index, and vector table.
/// Returns true if an entry was found and deleted.
pub fn delete(
    conn: &mut Connection,
    id: &str,
    vec_available: bool,
    profile: StoreProfile,
) -> Result<bool, MemoryError> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(MemoryError::InvalidArg("empty ID".to_string()));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let deleted = delete_memory_within_tx(&tx, trimmed, vec_available, profile)?;
    tx.commit()?;
    Ok(deleted)
}

/// Canonical exact-id deletion inside a caller-owned writer transaction.
/// Operator maintenance uses this seam so prepared-receipt publication and
/// every associated-index cleanup share one commit boundary.
pub(crate) fn delete_memory_within_tx(
    tx: &Connection,
    id: &str,
    vec_available: bool,
    profile: StoreProfile,
) -> Result<bool, MemoryError> {
    if tx.is_autocommit() {
        return Err(MemoryError::InvalidArg(
            "canonical deletion requires a caller-owned transaction".into(),
        ));
    }
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(MemoryError::InvalidArg("empty ID".to_string()));
    }
    refuse_reserved_rem_operation_mutation(trimmed, "deleted")?;
    refuse_retired_sticky_row_within_tx(tx, trimmed, "deleted")?;
    retire_projection_within_tx(tx, trimmed)?;
    // Delete from main table and check if anything was actually removed
    tx.execute("DELETE FROM memories WHERE id = ?1", params![trimmed])?;
    let deleted = tx.changes() > 0;

    if deleted {
        // Clean up FTS index
        tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![trimmed])?;
        delete_memories_symbolic_fts(tx, trimmed)?;

        if vec_available {
            tx.execute("DELETE FROM memories_vec WHERE id = ?1", params![trimmed])?;
        }

        // Clean up graph edges
        tx.execute(
            "DELETE FROM memory_edges WHERE source_id = ?1 OR target_id = ?1",
            params![trimmed],
        )?;

        // Clean up access history (CASCADE)
        tx.execute(
            "DELETE FROM access_history WHERE memory_id = ?1",
            params![trimmed],
        )?;

        // Clean up agent known state (CASCADE). PRODUCT table (#1585 D4): a
        // PortableKernel store never created it, so there is nothing to
        // cascade to. Explicit profile check, not a `table_exists` sniff.
        if profile.includes_product() {
            tx.execute(
                "DELETE FROM agent_known_state WHERE memory_id = ?1",
                params![trimmed],
            )?;
        }
    }

    Ok(deleted)
}
