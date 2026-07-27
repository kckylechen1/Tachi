use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::error::MemoryError;
use crate::types::{AuthorityLevel, EffectScope, GcConfig, TachiEventRecord};

use super::common::now_utc_iso;
use super::event_ledger::insert_tachi_event;

// ─── GC (Garbage Collection) ──────────────────────────────────────────────────

/// Retention-based cleanup of growing tables.
/// Thresholds are driven by `GcConfig` (replaces previously hardcoded literals).
/// Returns a summary of how many rows were deleted from each table.
pub fn gc_tables(conn: &mut Connection, cfg: &GcConfig) -> Result<serde_json::Value, MemoryError> {
    let tx = conn.transaction()?;

    // 1. access_history: retain latest N entries per (memory_id, event_kind),
    //    delete rest.
    //
    //    tachi#1446: the partition includes `event_kind` deliberately. Display
    //    events outnumber use events by construction — one display row per
    //    returned row per search, versus one use row only when a caller-
    //    initiated save named a memory's id — so a quota partitioned by
    //    `memory_id` alone would spend the whole budget on display rows and
    //    delete the rare use rows first. That failure is silent: the counters
    //    keep reporting rows pruned, and the signal the ranking knob depends on
    //    erodes with no error anywhere. Per-kind quotas make the retained
    //    budget for each provenance independent of the other's volume, and
    //    match the per-kind read cap in `memory_crud::access`'s
    //    `ACCESS_TIMES_MAX_PER_MEMORY`.
    let ah_sql = format!(
        "DELETE FROM access_history
         WHERE rowid IN (
             SELECT rowid FROM (
                 SELECT rowid,
                        ROW_NUMBER() OVER (
                            PARTITION BY memory_id, event_kind
                            ORDER BY accessed_at DESC
                        ) AS rn
                 FROM access_history
             ) ranked
             WHERE rn > {}
         )",
        cfg.access_history_keep_per_memory
    );
    let ah_deleted: usize = tx.execute(&ah_sql, [])?;

    // 2. processed_events: delete older than N days
    let pe_sql = format!(
        "DELETE FROM processed_events
         WHERE created_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-{} days')",
        cfg.processed_events_max_days
    );
    let pe_deleted: usize = tx.execute(&pe_sql, [])?;

    // 3. audit_log: delete older than N days OR keep only latest M rows
    let al_sql = format!(
        "DELETE FROM audit_log
         WHERE created_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-{} days')",
        cfg.audit_log_max_days
    );
    let al_deleted: usize = tx.execute(&al_sql, [])?;
    // Also cap at max_rows total
    let al_cap_sql = format!(
        "DELETE FROM audit_log WHERE id NOT IN (
            SELECT id FROM audit_log ORDER BY id DESC LIMIT {}
        )",
        cfg.audit_log_max_rows
    );
    let al_cap_deleted: usize = tx.execute(&al_cap_sql, [])?;

    // 4. agent_known_state: delete older than N days
    let aks_sql = format!(
        "DELETE FROM agent_known_state
         WHERE synced_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-{} days')",
        cfg.agent_known_state_max_days
    );
    let aks_deleted: usize = tx.execute(&aks_sql, [])?;

    // 5. Orphaned access_history (memory was deleted but history remained)
    let orphan_deleted: usize = tx.execute(
        "DELETE FROM access_history WHERE memory_id NOT IN (SELECT id FROM memories)",
        [],
    )?;

    // Reconcile query_diversity after pruning access_history rows.
    let diversity_updated: usize = tx.execute(
        "UPDATE memories
         SET query_diversity = (
             SELECT COUNT(DISTINCT query_hash)
             FROM access_history
             WHERE memory_id = memories.id AND query_hash != ''
         )",
        [],
    )?;

    // 6. Orphaned agent_known_state (memory was deleted but known-state remained)
    let orphan_aks_deleted: usize = tx.execute(
        "DELETE FROM agent_known_state WHERE memory_id NOT IN (SELECT id FROM memories)",
        [],
    )?;

    tx.commit()?;

    Ok(serde_json::json!({
        "access_history_pruned": ah_deleted,
        "query_diversity_reconciled": diversity_updated,
        "processed_events_pruned": pe_deleted,
        "audit_log_pruned": al_deleted + al_cap_deleted,
        "agent_known_state_pruned": aks_deleted,
        "orphaned_access_history": orphan_deleted,
        "orphaned_agent_known_state": orphan_aks_deleted,
    }))
}

// ─── AUTO-ARCHIVE STALE MEMORIES ──────────────────────────────────────────────

/// `event_type` of the receipt [`archive_stale_memories`] writes to
/// `tachi_events`. Named rather than inlined because both the writer and every
/// reader that wants to ask "has GC ever archived anything, and what" must
/// agree on the exact string — tachi#1463.
pub const GC_MEMORY_ARCHIVED_EVENT_TYPE: &str = "memory.gc_archived";

/// One auto-archive predicate, paired with the receipt vocabulary describing it.
///
/// The descriptive fields sit in the same literal as the statement they
/// describe so a receipt cannot drift into claiming a threshold the SQL does
/// not apply; a separately maintained list of pass names would eventually
/// diverge with nothing to catch it.
struct ArchivalPass {
    /// Stable identifier for this predicate, recorded in the receipt.
    name: &'static str,
    /// Column supplying this predicate's staleness reference.
    recency_column: &'static str,
    /// Rows qualify strictly below this importance.
    importance_below: f64,
    /// Which `retention_policy` values this predicate claims.
    retention_scope: &'static str,
    /// `UPDATE … RETURNING id`; `?1` is `stale_days`, `?2` the archival time.
    sql: String,
}

/// Result of running one [`ArchivalPass`].
struct ArchivalOutcome {
    count: usize,
    ids: Vec<String>,
}

/// Run a single archival pass and capture the ids it actually archived.
///
/// The `query_map` iterator is drained to exhaustion rather than run through
/// `execute`: with a `RETURNING` clause the modified rows are the statement's
/// output, so stepping it to completion is what both applies the whole update
/// and yields every id. `execute` is the wrong verb for a returning statement.
///
fn run_archival_pass(
    conn: &Connection,
    pass: &ArchivalPass,
    stale_days: u32,
    now: &str,
) -> Result<ArchivalOutcome, MemoryError> {
    let mut stmt = conn.prepare(&pass.sql)?;
    let mut count = 0usize;
    let mut ids: Vec<String> = Vec::new();
    let rows = stmt.query_map(params![stale_days, now], |row| row.get::<_, String>(0))?;
    for row in rows {
        let id = row?;
        count += 1;
        ids.push(id);
    }
    Ok(ArchivalOutcome { count, ids })
}

pub(crate) fn write_gc_archived_receipt(
    conn: &Connection,
    archived_at: &str,
    stale_days: u32,
    passes: serde_json::Value,
    archived_total: usize,
    source: &str,
) -> Result<(), MemoryError> {
    let event = TachiEventRecord {
        id: format!("gc-archive-{}", uuid::Uuid::new_v4()),
        source_repo: "tachi".to_string(),
        adapter: "memcore_gc".to_string(),
        project: String::new(),
        domain: String::new(),
        session_id: String::new(),
        actor: "memcore_gc".to_string(),
        event_type: GC_MEMORY_ARCHIVED_EVENT_TYPE.to_string(),
        authority: AuthorityLevel::RawFact,
        effects: vec![EffectScope::MemoryWrite, EffectScope::Recall],
        projection_hints: Vec::new(),
        payload: serde_json::json!({
            "stale_days": stale_days,
            "archived_total": archived_total,
            "archived_at": archived_at,
            "passes": passes,
        }),
        provenance: serde_json::json!({
            "source": source,
            "note": "scheduled unattended archival; rows listed here were removed from default search and had their revision bumped",
        }),
        created_at: archived_at.to_string(),
    };
    insert_tachi_event(conn, &event)
}

/// Archive low-importance memories that haven't been accessed in `stale_days`.
/// Respects `retention_policy`:
///   - permanent / pinned → never auto-archived (GC-exempt)
///   - ephemeral → more aggressive threshold (importance < 0.7 / < 0.5)
///   - durable (or NULL) → standard thresholds (importance < 0.5 / < 0.3)
///
/// Returns the number of memories archived.
///
/// # Receipt (tachi#1463)
///
/// Archiving a row removes it from default search, so this is a scheduled,
/// unattended mutation of what recall can return. Every sweep that archives at
/// least one row therefore writes a `memory.gc_archived` event to
/// `tachi_events` naming the threshold, the predicate that fired, and the rows
/// it moved. Before this existed the function returned a bare count into an
/// `eprintln!`, and establishing that GC had never archived anything took
/// forensic signature analysis of the corpus instead of a lookup.
///
/// Three deliberate choices here, each of which could reasonably have gone the
/// other way:
///
/// - **`tachi_events`, not `audit_log`.** `audit_log` is shaped for MCP proxy
///   tool calls (`timestamp, server_id, tool_name, args_hash, success,
///   duration_ms, error_kind`) with no free-form column to hold "which rows,
///   which predicate, which threshold" — and [`gc_tables`] prunes it, so GC
///   would reap its own receipts.
/// - **memcore writes to its own ledger.** Every other `insert_tachi_event`
///   caller lives in `tachi-server`; memcore has owned the DDL and the insert
///   function without ever using them. This is its first self-write. The
///   alternative — returning the ids and emitting from the `tachi-server`
///   scheduler — leaves the receipt optional at the layer that can forget it,
///   which is precisely how the gap arose. The crate that owns both the
///   mutation and the ledger emits the record for its own mutation.
/// - **Emit only when rows moved.** Nothing prunes `tachi_events`; a no-op
///   marker every six hours would add rows forever to record non-events. "GC
///   ran at all" is a scheduler-liveness question, not an archival receipt.
///
/// The sweep is also now a single transaction. It has to be, for the receipt to
/// mean anything: four autocommit statements could half-apply and leave a
/// receipt describing an archival that partially rolled back.
pub fn archive_stale_memories(conn: &Connection, stale_days: u32) -> Result<u64, MemoryError> {
    // `unchecked_transaction` takes `&Connection`, which keeps this function's
    // signature — and `MemoryStore::archive_stale_memories`'s `&self` — intact.
    // It rolls back on drop, so any `?` below abandons the whole sweep.
    let tx = conn.unchecked_transaction()?;
    let now = now_utc_iso();

    // Skip permanent and pinned memories entirely
    let exempt_clause =
        "AND (retention_policy IS NULL OR retention_policy NOT IN ('permanent', 'pinned'))";

    // `revision = revision + 1` is not bookkeeping, it is the point.
    // `restore_archived_if_revision` (memory_crud.rs) un-archives under
    // `WHERE id = ?2 AND archived = 1 AND revision = ?3`. If GC archived
    // without moving `revision`, a caller holding the pre-GC revision would
    // succeed at un-archiving a row it never learned had been archived — the
    // CAS guard silently blind to the one mutation no caller can observe. A CAS
    // failure after a GC sweep is the guard working. `updated_at` moves with it
    // because every sibling archival path sets both (`archive_memory`,
    // `archive_memory_if_revision`, `restore_archived_if_revision`, and the
    // lifecycle-proposal path in `store/memory_lifecycle.rs`); a row mutated at
    // a time its `updated_at` does not mention is
    // a column that lies. Checked before doing this: `updated_at` is not a
    // ranking input — it is absent from `MEMORY_SELECT_COLUMNS`, absent from
    // `MemoryEntry`, and the scorer's recency reference is `last_use_at` /
    // `last_access` falling back to `timestamp` — so a bumped `updated_at`
    // cannot make a later-restored row look spuriously fresh in search.
    //
    // The `WHERE` clauses below are unchanged: this commit records what the
    // predicates did, it does not touch what they select (tachi#1458 owns
    // whether GC should reap by display or by use).
    let passes = [
        // Durable (NULL or 'durable'): standard thresholds
        ArchivalPass {
            name: "durable_stale_by_last_access",
            recency_column: "last_access",
            importance_below: 0.5,
            retention_scope: "durable_or_unset",
            sql: format!(
                "UPDATE memories SET archived = 1, updated_at = ?2, revision = revision + 1
                 WHERE archived = 0
                   AND last_access IS NOT NULL
                   AND unixepoch(last_access) < unixepoch('now', '-' || ?1 || ' days')
                   AND importance < 0.5
                   AND (retention_policy IS NULL OR retention_policy = 'durable')
                   {exempt_clause}
                 RETURNING id"
            ),
        },
        ArchivalPass {
            name: "durable_never_accessed_by_timestamp",
            recency_column: "timestamp",
            importance_below: 0.3,
            retention_scope: "durable_or_unset",
            sql: format!(
                "UPDATE memories SET archived = 1, updated_at = ?2, revision = revision + 1
                 WHERE archived = 0
                   AND last_access IS NULL
                   AND unixepoch(timestamp) < unixepoch('now', '-' || ?1 || ' days')
                   AND importance < 0.3
                   AND (retention_policy IS NULL OR retention_policy = 'durable')
                   {exempt_clause}
                 RETURNING id"
            ),
        },
        // Ephemeral: more aggressive thresholds (importance < 0.7 / < 0.5)
        ArchivalPass {
            name: "ephemeral_stale_by_last_access",
            recency_column: "last_access",
            importance_below: 0.7,
            retention_scope: "ephemeral",
            sql: "UPDATE memories SET archived = 1, updated_at = ?2, revision = revision + 1
                  WHERE archived = 0
                    AND last_access IS NOT NULL
                    AND unixepoch(last_access) < unixepoch('now', '-' || ?1 || ' days')
                    AND importance < 0.7
                    AND retention_policy = 'ephemeral'
                  RETURNING id"
                .to_string(),
        },
        ArchivalPass {
            name: "ephemeral_never_accessed_by_timestamp",
            recency_column: "timestamp",
            importance_below: 0.5,
            retention_scope: "ephemeral",
            sql: "UPDATE memories SET archived = 1, updated_at = ?2, revision = revision + 1
                  WHERE archived = 0
                    AND last_access IS NULL
                    AND unixepoch(timestamp) < unixepoch('now', '-' || ?1 || ' days')
                    AND importance < 0.5
                    AND retention_policy = 'ephemeral'
                  RETURNING id"
                .to_string(),
        },
    ];

    let mut total: usize = 0;
    let mut pass_receipts: Vec<serde_json::Value> = Vec::with_capacity(passes.len());
    for pass in &passes {
        let outcome = run_archival_pass(&tx, pass, stale_days, &now)?;
        total += outcome.count;
        pass_receipts.push(serde_json::json!({
            "predicate": pass.name,
            "recency_column": pass.recency_column,
            "importance_below": pass.importance_below,
            "retention_scope": pass.retention_scope,
            "archived_count": outcome.count,
            "memory_ids": outcome.ids,
        }));
    }

    if total > 0 {
        write_gc_archived_receipt(
            &tx,
            &now,
            stale_days,
            serde_json::Value::Array(pass_receipts),
            total,
            "memcore::db::stats_gc::archive_stale_memories",
        )?;
    }

    tx.commit()?;
    Ok(total as u64)
}

// ─── STATS ────────────────────────────────────────────────────────────────────

/// Get aggregate statistics about the memory store.
pub fn stats(
    conn: &Connection,
    include_archived: bool,
) -> Result<crate::types::StatsResult, MemoryError> {
    fn i64_to_u64(value: i64, label: &str) -> Result<u64, MemoryError> {
        u64::try_from(value)
            .map_err(|_| MemoryError::InvalidArg(format!("negative aggregate count for {label}")))
    }

    fn aggregate_counts(
        conn: &Connection,
        sql: &str,
        include_archived: bool,
        label: &str,
    ) -> Result<HashMap<String, u64>, MemoryError> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![include_archived as i64], |row| {
            let key: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((key, count))
        })?;

        let mut out = HashMap::new();
        for row in rows {
            let (key, count) = row?;
            out.insert(key, i64_to_u64(count, label)?);
        }
        Ok(out)
    }

    fn root_path(path: &str) -> String {
        let mut parts = path.split('/').filter(|part| !part.is_empty());
        match parts.next() {
            Some(root) => format!("/{root}"),
            None => "/".to_string(),
        }
    }

    let total_i64: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE (?1 = 1 OR archived = 0)",
        params![include_archived as i64],
        |row| row.get(0),
    )?;
    let total = i64_to_u64(total_i64, "total")?;

    let by_scope = aggregate_counts(
        conn,
        "SELECT scope, COUNT(*) FROM memories
         WHERE (?1 = 1 OR archived = 0)
         GROUP BY scope",
        include_archived,
        "scope",
    )?;
    let by_category = aggregate_counts(
        conn,
        "SELECT category, COUNT(*) FROM memories
         WHERE (?1 = 1 OR archived = 0)
         GROUP BY category",
        include_archived,
        "category",
    )?;

    let mut stmt = conn.prepare("SELECT path FROM memories WHERE (?1 = 1 OR archived = 0)")?;
    let rows = stmt.query_map(params![include_archived as i64], |row| {
        row.get::<_, String>(0)
    })?;
    let mut by_root_path: HashMap<String, u64> = HashMap::new();
    for row in rows {
        let path = row?;
        *by_root_path.entry(root_path(&path)).or_insert(0) += 1;
    }

    Ok(crate::types::StatsResult {
        total,
        by_scope,
        by_category,
        by_root_path,
    })
}
