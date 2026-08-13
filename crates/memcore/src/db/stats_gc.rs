use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use std::collections::HashMap;

use crate::db::StoreProfile;
use crate::error::MemoryError;
use crate::types::{AuthorityLevel, EffectScope, GcConfig, TachiEventRecord};

use super::common::now_utc_iso;
use super::event_ledger::insert_tachi_event;

// ─── GC (Garbage Collection) ──────────────────────────────────────────────────

/// Retention-based cleanup of growing tables.
/// Thresholds are driven by `GcConfig` (replaces previously hardcoded literals).
/// Returns a summary of how many rows were deleted from each table.
/// `profile` is the store's effective [`StoreProfile`] (#1585 D4). `audit_log`
/// and `agent_known_state` are PRODUCT tables: on a `PortableKernel` store they
/// do not exist, and their prunes would fail the whole GC transaction with
/// `no such table`. The guard is an explicit profile check rather than a
/// `table_exists` sniff — a missing table on a store that is supposed to have
/// one is a broken database, and GC quietly skipping it is how that stays
/// invisible.
pub fn gc_tables(
    conn: &mut Connection,
    cfg: &GcConfig,
    profile: StoreProfile,
) -> Result<serde_json::Value, MemoryError> {
    let as_of = now_utc_iso();
    gc_tables_at(conn, cfg, profile, &as_of)
}

fn gc_tables_at(
    conn: &mut Connection,
    cfg: &GcConfig,
    profile: StoreProfile,
    as_of: &str,
) -> Result<serde_json::Value, MemoryError> {
    let product = profile.includes_product();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

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

    // 3. audit_log (PRODUCT): delete older than N days OR keep only latest M rows
    let (al_deleted, al_cap_deleted) = if product {
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
        (al_deleted, al_cap_deleted)
    } else {
        (0, 0)
    };

    // 4. agent_known_state (PRODUCT): delete older than N days
    let aks_deleted: usize = if product {
        let aks_sql = format!(
            "DELETE FROM agent_known_state
         WHERE synced_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-{} days')",
            cfg.agent_known_state_max_days
        );
        tx.execute(&aks_sql, [])?
    } else {
        0
    };

    // 5. Orphaned access_history (memory was deleted but history remained)
    let orphan_deleted: usize = tx.execute(
        "DELETE FROM access_history WHERE memory_id NOT IN (SELECT id FROM memories)",
        [],
    )?;

    // Reconcile query_diversity after pruning access_history rows. Candidate
    // discovery deliberately does not approximate the retired-sticky
    // classifier in SQL: normalize/classify every id against the canonical
    // ordinary-writer guard in this same writer transaction, then mutate only
    // the ids it admitted.
    let diversity_candidate_ids = {
        let mut stmt = tx.prepare("SELECT id FROM memories")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids
    };
    let mut diversity_ordinary_ids = Vec::with_capacity(diversity_candidate_ids.len());
    for id in diversity_candidate_ids {
        match super::refuse_retired_sticky_row_within_tx(&tx, &id, "query-diversity reconciled") {
            Ok(()) => diversity_ordinary_ids.push(id),
            Err(MemoryError::InvalidArg(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let mut diversity_updated = 0usize;
    for id in diversity_ordinary_ids {
        diversity_updated += tx.execute(
            "UPDATE memories
             SET query_diversity = (
                 SELECT COUNT(DISTINCT query_hash)
                 FROM access_history
                 WHERE memory_id = memories.id AND query_hash != ''
             )
             WHERE id = ?1",
            [&id],
        )?;
    }

    // 6. Orphaned agent_known_state (PRODUCT; memory was deleted but
    //    known-state remained)
    let orphan_aks_deleted: usize = if product {
        tx.execute(
            "DELETE FROM agent_known_state WHERE memory_id NOT IN (SELECT id FROM memories)",
            [],
        )?
    } else {
        0
    };

    // Recall impressions own their retention. Deleting groups cascades rows;
    // access_history and query_diversity are intentionally untouched.
    let impression_age_sql = format!(
        "DELETE FROM recall_impression_groups
         WHERE created_at < STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now', '-{} days')",
        cfg.recall_impression_max_days
    );
    let impression_age_deleted = tx.execute(&impression_age_sql, [])?;
    let impression_quota_sql = format!(
        "DELETE FROM recall_impression_groups
         WHERE group_id NOT IN (
             SELECT group_id FROM recall_impression_groups
             ORDER BY created_at DESC, group_id DESC LIMIT {}
         )",
        cfg.recall_impression_max_groups
    );
    let impression_quota_deleted = tx.execute(&impression_quota_sql, [])?;

    // #1751 v32: body content is retained until the authoritative terminal
    // receipt is at least 90 days old. The digest and every other envelope /
    // receipt field remain immutable. A bounded subquery prevents one GC call
    // from monopolizing the Product writer lock; later calls resume naturally.
    const A2A_BODY_SCRUB_BATCH: i64 = 100;
    let a2a_bodies_scrubbed = if product {
        tx.execute(
            "UPDATE a2a_envelopes SET body=NULL
             WHERE envelope_id IN (
                 SELECT e.envelope_id
                   FROM a2a_envelopes e
                   JOIN a2a_delivery_receipts r
                     ON r.envelope_id=e.envelope_id
                    AND r.state=e.current_state
                    AND r.envelope_version=e.state_version
                  WHERE e.body IS NOT NULL
                    AND e.current_state IN ('consumed','expired')
                    AND unixepoch(r.occurred_at, '+90 days') <= unixepoch(?1)
                  ORDER BY r.occurred_at,e.envelope_id
                  LIMIT ?2
             )",
            params![as_of, A2A_BODY_SCRUB_BATCH],
        )?
    } else {
        0
    };
    #[cfg(test)]
    test_hooks::fail_after_a2a_body_scrub()?;

    tx.commit()?;

    Ok(serde_json::json!({
        "access_history_pruned": ah_deleted,
        "query_diversity_reconciled": diversity_updated,
        "processed_events_pruned": pe_deleted,
        "audit_log_pruned": al_deleted + al_cap_deleted,
        "agent_known_state_pruned": aks_deleted,
        "orphaned_access_history": orphan_deleted,
        "orphaned_agent_known_state": orphan_aks_deleted,
        "recall_impression_groups_pruned": impression_age_deleted + impression_quota_deleted,
        "a2a_bodies_scrubbed": a2a_bodies_scrubbed,
    }))
}

#[cfg(test)]
mod test_hooks {
    use std::cell::Cell;

    use crate::error::MemoryError;

    thread_local! {
        static FAIL_AFTER_A2A_BODY_SCRUB: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn arm_fail_after_a2a_body_scrub() {
        FAIL_AFTER_A2A_BODY_SCRUB.with(|flag| flag.set(true));
    }

    pub(super) fn fail_after_a2a_body_scrub() -> Result<(), MemoryError> {
        if FAIL_AFTER_A2A_BODY_SCRUB.with(|flag| flag.replace(false)) {
            return Err(MemoryError::Internal(
                "test_hooks: injected failure after A2A body scrub".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod a2a_body_retention_tests {
    use super::*;

    fn product_conn() -> Connection {
        let _ = libsimple::enable_auto_extension();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().expect("open product fixture");
        crate::db::schema::init_schema(&conn).expect("initialize current Product schema");
        conn.execute_batch(
            "INSERT INTO agent_identities(agent_identity_id,created_at)
                 VALUES ('retention-agent','2026-01-01T00:00:00.000Z');
             INSERT INTO identity_admissions
                 (admission_id,agent_identity_id,connection_id,state,created_at)
                 VALUES ('retention-admission','retention-agent','retention-connection',
                         'self_asserted','2026-01-01T00:00:00.000Z');",
        )
        .expect("seed retention identity");
        conn
    }

    fn seed_envelope(conn: &Connection, id: &str, state: &str, occurred_at: Option<&str>) {
        let version = if state == "received" { 1 } else { 3 };
        conn.execute(
            "INSERT INTO a2a_envelopes
             (envelope_id,kind,issuer_agent_identity_id,issuer_admission_id,
              recipient_agent_identity_id,recipient_admission_id,subject_ref,body,
              body_digest,issuer_identity_assurance,recipient_identity_assurance,
              issuer_trust_domain,recipient_trust_domain,issuer_trust_basis,
              recipient_trust_basis,idempotency_key,created_at,expires_at,current_state,state_version)
             VALUES (?1,'turn_response/v1','retention-agent','retention-admission',
                     'retention-agent','retention-admission','peer_publication:retention',?2,
                     'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'self_asserted','self_asserted','same_host','same_host',
                     'current_local_connection','historical_local_admission',?1,
                     '2026-01-01T00:00:00.000Z','2027-01-01T00:00:00.000Z',?3,?4)",
            params![id, format!("body-{id}"), state, version],
        )
        .expect("seed envelope");
        if let Some(occurred_at) = occurred_at {
            conn.execute(
                "INSERT INTO a2a_delivery_receipts
                 (receipt_id,envelope_id,envelope_version,state,actor_agent_identity_id,
                  actor_admission_id,identity_assurance,trust_domain,trust_basis,occurred_at)
                 VALUES (?1 || ':receipt',?1,?2,?3,'retention-agent','retention-admission',
                         'self_asserted','same_host','current_local_connection',?4)",
                params![id, version, state, occurred_at],
            )
            .expect("seed matching receipt");
        }
    }

    fn body(conn: &Connection, id: &str) -> Option<String> {
        conn.query_row(
            "SELECT body FROM a2a_envelopes WHERE envelope_id=?1",
            [id],
            |row| row.get(0),
        )
        .expect("read envelope body")
    }

    #[test]
    fn gc_scrubs_only_terminal_body_at_matching_receipt_plus_ninety_days() {
        let mut conn = product_conn();
        let as_of = chrono::DateTime::parse_from_rfc3339("2026-08-13T12:00:00Z").unwrap();
        let at_90 = (as_of - chrono::Duration::days(90)).to_rfc3339();
        let at_89 = (as_of - chrono::Duration::days(89)).to_rfc3339();
        seed_envelope(&conn, "consumed-90", "consumed", Some(&at_90));
        seed_envelope(&conn, "expired-90", "expired", Some(&at_90));
        seed_envelope(&conn, "consumed-89", "consumed", Some(&at_89));
        seed_envelope(&conn, "received-old", "received", Some(&at_90));
        seed_envelope(&conn, "missing-receipt", "consumed", None);

        let first = gc_tables_at(
            &mut conn,
            &GcConfig::default(),
            StoreProfile::TachiFull,
            &as_of.to_rfc3339(),
        )
        .expect("retention GC");
        assert_eq!(first["a2a_bodies_scrubbed"], 2);
        assert_eq!(body(&conn, "consumed-90"), None);
        assert_eq!(body(&conn, "expired-90"), None);
        for id in ["consumed-89", "received-old", "missing-receipt"] {
            assert_eq!(body(&conn, id), Some(format!("body-{id}")), "{id}");
        }

        let repeat = gc_tables_at(
            &mut conn,
            &GcConfig::default(),
            StoreProfile::TachiFull,
            &as_of.to_rfc3339(),
        )
        .expect("idempotent repeat");
        assert_eq!(repeat["a2a_bodies_scrubbed"], 0);
    }

    #[test]
    fn gc_body_scrub_rolls_back_with_the_outer_table_sweep() {
        let mut conn = product_conn();
        seed_envelope(
            &conn,
            "rollback-terminal",
            "consumed",
            Some("2026-01-01T00:00:00Z"),
        );
        test_hooks::arm_fail_after_a2a_body_scrub();
        let error = gc_tables_at(
            &mut conn,
            &GcConfig::default(),
            StoreProfile::TachiFull,
            "2026-08-13T12:00:00Z",
        )
        .expect_err("injected post-scrub failure must roll back");
        assert!(error.to_string().contains("injected failure"), "{error}");
        assert_eq!(
            body(&conn, "rollback-terminal"),
            Some("body-rollback-terminal".to_string())
        );
    }

    #[test]
    fn gc_body_scrub_is_bounded_to_one_hundred_rows_per_call() {
        let mut conn = product_conn();
        for index in 0..101 {
            seed_envelope(
                &conn,
                &format!("bounded-{index:03}"),
                "expired",
                Some("2026-01-01T00:00:00Z"),
            );
        }
        let first = gc_tables_at(
            &mut conn,
            &GcConfig::default(),
            StoreProfile::TachiFull,
            "2026-08-13T12:00:00Z",
        )
        .unwrap();
        assert_eq!(first["a2a_bodies_scrubbed"], 100);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM a2a_envelopes WHERE body IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
    }

    fn seed_memory(conn: &Connection, id: &str, path: &str, category: &str, query_diversity: i64) {
        conn.execute(
            "INSERT INTO memories (
                 id, path, summary, text, importance, timestamp, category, source, scope,
                 created_at, updated_at, last_access, revision, retention_policy, query_diversity
             ) VALUES (?1, ?2, '', ?1, 0.4, '2020-01-01T00:00:00Z', ?3, 'manual', 'general',
                       '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z',
                       '2020-01-01T00:00:00Z', 1, 'durable', ?4)",
            params![id, path, category, query_diversity],
        )
        .expect("seed memory");
    }

    fn seed_display_access(conn: &Connection, id: &str, query_hash: &str) {
        conn.execute(
            "INSERT INTO access_history(memory_id, accessed_at, query_hash, event_kind)
             VALUES (?1, '2026-08-13T00:00:00Z', ?2, 'display')",
            params![id, query_hash],
        )
        .expect("seed display access");
    }

    #[test]
    fn gc_tables_preserves_malformed_retired_sticky_diversity_while_ordinary_reconciles() {
        let mut conn = product_conn();
        seed_memory(&conn, "ordinary-diversity", "/notes/ordinary", "fact", 7);
        seed_memory(
            &conn,
            "sticky-malformed-diversity",
            "//STICKY///legacy",
            "fact",
            7,
        );
        seed_display_access(&conn, "ordinary-diversity", "ordinary-query");
        seed_display_access(&conn, "sticky-malformed-diversity", "sticky-query");

        gc_tables_at(
            &mut conn,
            &GcConfig::default(),
            StoreProfile::TachiFull,
            "2026-08-13T12:00:00Z",
        )
        .expect("stats GC");

        let diversity: Vec<(String, i64)> = ["ordinary-diversity", "sticky-malformed-diversity"]
            .into_iter()
            .map(|id| {
                conn.query_row(
                    "SELECT path, query_diversity FROM memories WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(
            diversity,
            vec![
                ("/notes/ordinary".to_string(), 1),
                ("//STICKY///legacy".to_string(), 7),
            ]
        );
    }

    #[test]
    fn gc_archive_preserves_malformed_retired_sticky_while_ordinary_progresses() {
        let conn = product_conn();
        seed_memory(&conn, "ordinary-archive", "/notes/ordinary", "fact", 0);
        seed_memory(
            &conn,
            "sticky-malformed-archive",
            "//STICKY///legacy",
            "fact",
            0,
        );
        conn.execute(
            "UPDATE memories SET importance=0.2 WHERE id IN ('ordinary-archive', 'sticky-malformed-archive')",
            [],
        )
        .expect("lower archive fixture importance");

        let archived =
            archive_stale_memories_with_config(&conn, 60, &crate::RecallConfig::default())
                .expect("archive stale memories");
        assert_eq!(archived, 1);
        let states: Vec<(String, i64, i64)> = ["ordinary-archive", "sticky-malformed-archive"]
            .into_iter()
            .map(|id| {
                conn.query_row(
                    "SELECT path, archived, revision FROM memories WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(
            states,
            vec![
                ("/notes/ordinary".to_string(), 1, 2),
                ("//STICKY///legacy".to_string(), 0, 1),
            ]
        );
    }
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
    name: String,
    /// Column supplying this predicate's staleness reference.
    recency_column: &'static str,
    /// Rows qualify strictly below this importance.
    importance_below: f64,
    /// Which `retention_policy` values this predicate claims.
    retention_scope: &'static str,
    /// Candidate predicate; `?1` is `stale_days`.
    predicate_sql: String,
}

/// Result of running one [`ArchivalPass`].
struct ArchivalOutcome {
    count: usize,
    ids: Vec<String>,
}

/// Run a single archival pass and capture the ids it actually archived.
/// Candidate discovery, canonical retired-sticky classification, and mutation
/// all share the caller's immediate writer transaction.
fn run_archival_pass(
    tx: &Transaction<'_>,
    pass: &ArchivalPass,
    stale_days: u32,
    now: &str,
) -> Result<ArchivalOutcome, MemoryError> {
    let candidate_ids = {
        let sql = format!("SELECT id FROM memories WHERE {}", pass.predicate_sql);
        let mut stmt = tx.prepare(&sql)?;
        let ids = stmt
            .query_map([stale_days], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids
    };
    let mut ordinary_ids = Vec::with_capacity(candidate_ids.len());
    for id in candidate_ids {
        match super::refuse_retired_sticky_row_within_tx(tx, &id, "GC-archived") {
            Ok(()) => ordinary_ids.push(id),
            Err(MemoryError::InvalidArg(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let mut ids = Vec::with_capacity(ordinary_ids.len());
    for id in ordinary_ids {
        if tx.execute(
            "UPDATE memories
             SET archived = 1, updated_at = ?1, revision = revision + 1
             WHERE id = ?2",
            params![now, id],
        )? == 1
        {
            ids.push(id);
        }
    }
    ids.sort();
    Ok(ArchivalOutcome {
        count: ids.len(),
        ids,
    })
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
/// tachi#1585 D5: defaults to the pure `RecallConfig::default()`, not the
/// process-wide `RecallConfig::get()`. This free function takes a bare
/// `Connection` (no `MemoryStore` to draw a host-injected policy from); the
/// real production entry point is `MemoryStore::archive_stale_memories`
/// (`store/derived.rs`), which calls `archive_stale_memories_with_config`
/// directly with the store's `KernelPolicy::recall` and never reaches this
/// default.
pub fn archive_stale_memories(conn: &Connection, stale_days: u32) -> Result<u64, MemoryError> {
    archive_stale_memories_with_config(conn, stale_days, &crate::RecallConfig::default())
}

/// Configurable twin used by discrimination tests and explicit rollback.
/// Use provenance is the normal arm; `false` preserves the historical
/// display-derived GC clock byte-for-byte.
pub fn archive_stale_memories_with_config(
    conn: &Connection,
    stale_days: u32,
    recall_config: &crate::RecallConfig,
) -> Result<u64, MemoryError> {
    // `unchecked_transaction` takes `&Connection`, which keeps this function's
    // signature — and `MemoryStore::archive_stale_memories`'s `&self` — intact.
    // It rolls back on drop, so any `?` below abandons the whole sweep.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
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
    let (recency_column, stale_suffix, never_suffix) = if recall_config.use_provenance_recency {
        ("last_use_at", "last_use", "never_used")
    } else {
        ("last_access", "last_access", "never_accessed")
    };

    let passes = [
        // Durable (NULL or 'durable'): standard thresholds
        ArchivalPass {
            name: format!("durable_stale_by_{stale_suffix}"),
            recency_column,
            importance_below: 0.5,
            retention_scope: "durable_or_unset",
            predicate_sql: format!(
                "archived = 0
                   AND {recency_column} IS NOT NULL
                   AND unixepoch({recency_column}) < unixepoch('now', '-' || ?1 || ' days')
                   AND importance < 0.5
                   AND (retention_policy IS NULL OR retention_policy = 'durable')
                   {exempt_clause}"
            ),
        },
        ArchivalPass {
            name: format!("durable_{never_suffix}_by_timestamp"),
            recency_column: "timestamp",
            importance_below: 0.3,
            retention_scope: "durable_or_unset",
            predicate_sql: format!(
                "archived = 0
                   AND {recency_column} IS NULL
                   AND unixepoch(timestamp) < unixepoch('now', '-' || ?1 || ' days')
                   AND importance < 0.3
                   AND (retention_policy IS NULL OR retention_policy = 'durable')
                   {exempt_clause}"
            ),
        },
        // Ephemeral: more aggressive thresholds (importance < 0.7 / < 0.5)
        ArchivalPass {
            name: format!("ephemeral_stale_by_{stale_suffix}"),
            recency_column,
            importance_below: 0.7,
            retention_scope: "ephemeral",
            predicate_sql: format!(
                "archived = 0
                    AND {recency_column} IS NOT NULL
                    AND unixepoch({recency_column}) < unixepoch('now', '-' || ?1 || ' days')
                    AND importance < 0.7
                    AND retention_policy = 'ephemeral'"
            ),
        },
        ArchivalPass {
            name: format!("ephemeral_{never_suffix}_by_timestamp"),
            recency_column: "timestamp",
            importance_below: 0.5,
            retention_scope: "ephemeral",
            predicate_sql: format!(
                "archived = 0
                    AND {recency_column} IS NULL
                    AND unixepoch(timestamp) < unixepoch('now', '-' || ?1 || ' days')
                    AND importance < 0.5
                    AND retention_policy = 'ephemeral'"
            ),
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
