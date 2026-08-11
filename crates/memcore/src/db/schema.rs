use rusqlite::{params, Connection, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::MemoryError;

use super::common::normalize_utc_iso;

mod ddl;

/// Initialize a private fresh schema and run the same sentinel migrations used
/// by file-backed provisioning. The sole production caller is
/// [`crate::MemoryStore::open_in_memory`]; tests also use this as the complete
/// current-schema constructor.
pub fn init_schema(conn: &Connection) -> Result<(), MemoryError> {
    super::ensure_reserved_reference_write_guard(conn)?;
    crate::db::migrations::check_schema_version_gate(conn)?;
    crate::db::migrations::validate_current_schema_integrity(conn)?;
    apply_connection_pragmas(conn)?;
    let tx = conn.unchecked_transaction()?;
    // In-memory stores are ephemeral and carry no manifest identity, so they
    // are built at the default (full) profile and stamped with nothing: there
    // is no file for an identity to travel with (#1585 D3).
    let profile = crate::db::StoreProfile::default();
    init_schema_inner(&tx, profile)?;
    crate::db::migrations::run_data_migrations_in_tx(
        &tx,
        "global",
        Path::new(":memory:"),
        profile,
    )?;
    crate::db::migrations::write_schema_version_stamp(&tx)?;
    super::validate_persistent_trigger_inventory(&tx, true)?;
    validate_recall_impression_ledger_schema(&tx)?;
    validate_typo_fallback_attribution_schema(&tx)?;
    validate_wiki_recovery_ledgers_schema(&tx)?;
    validate_memory_outbox_schema(&tx)?;
    validate_memory_outbox_destination_apply_schema(&tx)?;
    validate_harness_session_attachments_schema(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Build the pre-sentinel fixture used only by migration unit tests. Production
/// fresh initialization must use [`init_schema`] so versioned schema is never
/// installed outside the migration runner.
#[cfg(test)]
pub(crate) fn init_unversioned_schema_for_migration_tests(
    conn: &Connection,
) -> Result<(), MemoryError> {
    super::ensure_reserved_reference_write_guard(conn)?;
    apply_connection_pragmas(conn)?;
    init_schema_inner(conn, crate::db::StoreProfile::default())?;
    install_reserved_reference_guard(conn)?;
    super::validate_persistent_trigger_inventory(conn, true)
}

/// Initialize schema and run data migrations with a known DB label and path.
/// Backs up the DB file before migrating when the schema fingerprint has
/// changed since the last successful init (see `maybe_backup_before_migration`).
///
/// Hard-fails before touching the DB (#984) if it is stamped with a schema
/// version newer than this kernel's `EXPECTED_SCHEMA_VERSION` supports — see
/// `crate::db::migrations::check_schema_version_gate`.
///
/// Also enforces the #1119 typed migration gate
/// (`crate::db::migrations::check_db_open_context_gate`) using the caller's
/// [`crate::db::DbOpenContext`]: an `OpenExisting` open of a *stamped older*
/// DB (`1 ≤ stored < EXPECTED`) without `MigrationAuthority::Allow` refuses
/// with a typed `SchemaMigrationOptInRequired` instead of silently migrating
/// in place. Fresh (`user_version == 0`) files build with no authority.
///
/// ## Compatibility transaction boundary (#984 F1 round 3)
///
/// Everything that mutates schema or data in a way another kernel's
/// [`crate::db::migrations::check_schema_version_gate`] would care about —
/// `init_schema_inner`'s DDL/legacy-column work (including the v6/v8/v9
/// standalone migrations previously committed outside any outer transaction)
/// AND `run_data_migrations`'s sentinel-gated migrations AND the final
/// `user_version` stamp — now run inside ONE `BEGIN IMMEDIATE` opened here
/// and committed only after all of it succeeds. Only work that literally
/// cannot run inside a transaction (the `journal_mode`/`foreign_keys`/
/// `busy_timeout`/`cache_size` connection PRAGMAs) and the pre-migration file
/// backup (a live-connection `rusqlite::backup::Backup`, not a DB write)
/// happen outside the transaction, before it opens.
pub fn init_schema_with_label_mut(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
    ctx: &crate::db::DbOpenContext,
) -> Result<SchemaInitOutcome, MemoryError> {
    super::ensure_reserved_reference_write_guard(conn)?;
    crate::db::migrations::check_schema_version_gate(conn)?;
    // #1119: typed migration gate. Runs BEFORE any backup/DDL/migration/stamp
    // mutates the DB — an unauthorized `OpenExisting + Deny` open of a
    // stamped older DB must refuse before `init_schema_inner`'s idempotent
    // DDL or the final `write_schema_version_stamp` touches the file.
    crate::db::migrations::check_db_open_context_gate(conn, current_db_path, ctx)?;
    crate::db::migrations::validate_current_schema_integrity(conn)?;
    // The same discriminator `check_db_open_context_gate` uses: an unstamped
    // file is fresh, whatever its content (#1119 owner ruling A). Sampled
    // BEFORE the transaction writes the new stamp.
    let fresh = crate::db::migrations::read_schema_version(conn)? == 0;
    maybe_backup_before_migration(conn, current_db_path)?;
    apply_connection_pragmas(conn)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // #1585/#1579: resolve identity BEFORE any DDL runs, so a refused open
    // (role conflict, profile mismatch, unstamped-under-portable) leaves the
    // database byte-identical. Everything below is inside the same
    // BEGIN IMMEDIATE, so even a later failure rolls the stamps back with it.
    let identity = resolve_store_identity_in_tx(&tx, db_label, current_db_path, ctx, fresh)?;
    init_schema_inner(&tx, identity.profile)?;
    stamp_store_identity_in_tx(&tx, &identity, ctx)?;
    #[cfg(test)]
    test_hooks::fail_after_legacy_work_before_stamp()?;
    // The RESOLVED label, not the caller's claim: from here down, one authority.
    let report = crate::db::migrations::run_data_migrations_in_tx(
        &tx,
        &identity.db_label,
        current_db_path,
        identity.profile,
    )?;
    crate::db::migrations::write_schema_version_stamp(&tx)?;
    super::validate_persistent_trigger_inventory(&tx, true)?;
    validate_recall_impression_ledger_schema(&tx)?;
    validate_typo_fallback_attribution_schema(&tx)?;
    validate_wiki_recovery_ledgers_schema(&tx)?;
    validate_memory_outbox_schema(&tx)?;
    validate_memory_outbox_destination_apply_schema(&tx)?;
    validate_harness_session_attachments_schema(&tx)?;
    tx.commit()?;

    remember_migration_fingerprint(conn, current_db_path)?;
    Ok(SchemaInitOutcome { report, identity })
}

/// What `init_schema_with_label_mut` hands back: the migration report it always
/// returned, plus the store identity it resolved (#1579/#1585).
///
/// The identity is a return value rather than something the caller re-reads,
/// because the caller must use *exactly* what the schema transaction committed
/// — re-reading opens a window where another process's stamp is observed
/// instead.
#[derive(Debug)]
pub struct SchemaInitOutcome {
    pub report: crate::db::migrations::MigrationReport,
    pub identity: crate::db::store_identity::StoreIdentity,
}

/// Read both identity stamps and apply the #1579 role table and the #1585 D2
/// admission table. Errors here abort the open with the transaction untouched.
fn resolve_store_identity_in_tx(
    tx: &Connection,
    claimed_label: &str,
    current_db_path: &Path,
    ctx: &crate::db::DbOpenContext,
    fresh: bool,
) -> Result<crate::db::store_identity::StoreIdentity, MemoryError> {
    use crate::db::store_identity;

    let (stored_role, stored_profile) = store_identity::read_identity(tx, current_db_path)?;
    let profile = store_identity::resolve_profile(
        stored_profile,
        fresh,
        ctx.required_profile,
        current_db_path,
    )?;
    let db_label =
        store_identity::resolve_role(stored_role.as_deref(), claimed_label, current_db_path)?;
    Ok(store_identity::StoreIdentity { db_label, profile })
}

/// Write the write-once identity rows. Runs AFTER `init_schema_inner` because
/// `hard_state` may not have existed yet, and inside the same transaction so a
/// later failure cannot leave a stamp behind on a database that never finished
/// initializing.
///
/// A role is stamped only when the caller actually declared one: an unlabelled
/// open (`MemoryStore::open`, CLI diagnostics, fixtures) must never confer
/// identity, which is precisely the order-dependence #1579 removes.
fn stamp_store_identity_in_tx(
    tx: &Connection,
    identity: &crate::db::store_identity::StoreIdentity,
    ctx: &crate::db::DbOpenContext,
) -> Result<(), MemoryError> {
    use crate::db::store_identity;
    use crate::db::store_profile::{STORE_PROFILE_KEY, STORE_ROLE_KEY};

    let conferred_by = match ctx.intent {
        crate::db::OpenIntent::CreateFresh => "open:create-fresh",
        crate::db::OpenIntent::OpenExisting => "open:existing",
    };
    store_identity::write_stamp_if_absent(
        tx,
        STORE_PROFILE_KEY,
        identity.profile.as_str(),
        conferred_by,
    )?;
    if identity.db_label != crate::path_router::UNKNOWN_DB_LABEL {
        store_identity::write_stamp_if_absent(
            tx,
            STORE_ROLE_KEY,
            &identity.db_label,
            conferred_by,
        )?;
    }
    Ok(())
}

/// Test-only fault-injection hook for proving the compatibility transaction
/// (#984 F1 round 3) actually rolls `init_schema_inner`'s legacy work back on
/// failure, not just the sentinel migrations. Real callers never touch this;
/// production builds don't compile it.
#[cfg(test)]
pub(crate) mod test_hooks {
    use crate::error::MemoryError;
    use std::cell::Cell;

    thread_local! {
        static FAIL_AFTER_LEGACY_WORK: Cell<bool> = const { Cell::new(false) };
    }

    /// Arm the injection: the NEXT call to `init_schema_with_label_mut` on
    /// this thread will fail right after `init_schema_inner` (DDL + legacy
    /// v6/v8/v9 work) completes but before `run_data_migrations_in_tx` and
    /// the version stamp run. Auto-disarms after firing once.
    pub(crate) fn arm_fail_after_legacy_work() {
        FAIL_AFTER_LEGACY_WORK.with(|flag| flag.set(true));
    }

    pub(super) fn fail_after_legacy_work_before_stamp() -> Result<(), MemoryError> {
        let armed = FAIL_AFTER_LEGACY_WORK.with(|flag| flag.replace(false));
        if armed {
            return Err(MemoryError::InvalidArg(
                "test_hooks: injected failure after init_schema_inner, before stamp".to_string(),
            ));
        }
        Ok(())
    }
}

/// Connection-level PRAGMAs (`journal_mode`, `foreign_keys`, `busy_timeout`,
/// `cache_size`). These MUST run outside any transaction — `journal_mode` in
/// particular is a no-op/error mid-transaction — so callers apply this before
/// opening the compatibility transaction, not inside `init_schema_inner`.
fn apply_connection_pragmas(conn: &Connection) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::CONNECTION_PRAGMA_SQL)?;
    // `MemoryStore::open_with_context_and_busy_timeout` installs a scoped
    // local deadline before schema work begins. Do not overwrite that writer's
    // busy budget here. Raw/direct schema callers without such a deadline keep
    // the historical five-second connection policy.
    if super::sqlite_busy_deadline_remaining().is_none() {
        super::configure_connection(conn)?;
    }
    Ok(())
}

/// Run one scoped DDL chunk list, skipping product chunks when the effective
/// profile does not include the product surface (#1585 D3).
///
/// `profile` is the store's **effective** profile — the stored one for an
/// existing database, the requested one only when building a fresh file. It is
/// never `DbOpenContext::required_profile` on an existing store; see
/// [`crate::db::store_profile`].
fn execute_schema_chunks(
    conn: &Connection,
    chunks: &[(ddl::SchemaScope, &str)],
    profile: crate::db::StoreProfile,
) -> Result<(), MemoryError> {
    for (scope, sql) in chunks {
        if matches!(scope, ddl::SchemaScope::Product) && !profile.includes_product() {
            continue;
        }
        execute_batch_retry(conn, sql)?;
    }
    Ok(())
}

fn init_schema_inner(
    conn: &Connection,
    profile: crate::db::StoreProfile,
) -> Result<(), MemoryError> {
    execute_schema_chunks(conn, ddl::BASE_SCHEMA_CHUNKS, profile)?;

    // Legacy recall-cache rows predate database-authoritative generation
    // snapshots. The empty default is intentionally non-matching, so the first
    // read after migration recomputes instead of presenting an old row as clean.
    ensure_column(
        conn,
        "recall_cache",
        "generation_fingerprint",
        "TEXT NOT NULL DEFAULT ''",
    )?;

    // Forward-compatible migrations for existing DB files created before
    // archived/created_at/updated_at columns existed.
    ensure_column(conn, "memories", "archived", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "memories", "created_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "memories", "updated_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_memories_scored_count(conn)?;
    ensure_column(conn, "memories", "revision", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(conn, "memories", "valid_from", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "memories", "valid_until", "TEXT")?;

    // Retention policy and domain columns for Issue #38 and #32
    ensure_column(conn, "memories", "retention_policy", "TEXT")?;
    ensure_column(conn, "memories", "domain", "TEXT")?;
    ensure_column(conn, "memories", "superseded_by", "TEXT")?;
    ensure_column(conn, "memories", "idless_identity", "TEXT")?;
    // #1289 note: unlike the non-memories evolutionary indexes centralized in
    // ddl.rs's MIGRATED_INDEXES_SQL, this one is built inline right after its
    // own `ensure_column` (its own ensure+build unit) — deliberately NOT in the
    // central list, because the `memories` table is fully rebuilt by
    // `rebuild_memories_with_check_constraints`, which recreates this same index
    // by name, so all of `memories`' indexes travel with that rebuild. It still
    // satisfies ruling A (present on the migration-free init_schema path) and is
    // covered by the convergence oracle in migration_tests.rs.
    conn.execute(
        r#"CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_idless_identity_active
           ON memories(idless_identity)
           WHERE idless_identity IS NOT NULL AND archived = 0 AND superseded_by IS NULL"#,
        [],
    )?;

    // Memory lifecycle columns for tier-based decay and historical training flags.
    //
    // tachi#1459: both of these observe the search path only; reads through
    // path-listing routes do not increment them. `record_access_with_updates`
    // (from `hybrid_search`) is the only production writer of `recall_count`,
    // and `query_diversity` is written there and reconciled by `gc_tables` from
    // `access_history`: only search writes non-empty query hashes, so only
    // those rows supply query-diversity evidence. Use rows have empty query
    // hashes and do not inflate diversity. They gate tier promotion, so that
    // gate sees a search-only view of a memory's query diversity.
    ensure_column(
        conn,
        "memories",
        "recall_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "memories",
        "query_diversity",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "memories", "tier", "TEXT NOT NULL DEFAULT 'raw'")?;
    // tachi#1446: exposure-free recency reference, nullable and never written
    // by this commit. Added here (not only in `ddl.rs`) because
    // `init_paths_converge_to_the_same_schema` (schema/migration_tests.rs)
    // requires the `init_schema`-only path to reach the same column set as the
    // full path — a sentinel migration alone would leave path (b) short.
    ensure_column(conn, "memories", "last_use_at", "TEXT")?;
    ensure_column(conn, "access_history", "query_hash", "TEXT")?;
    // tachi#1446 lever 5. Provenance discriminator on the access ledger.
    // `'display'` is the correct default for every pre-existing row: before
    // this column, `record_access_with_updates` (reached only from
    // `search.rs`'s `hybrid_search`) was the single production writer of this
    // table, so every legacy row is by construction a record of the pipeline
    // showing a result. NOT NULL + DEFAULT keeps the column total, so
    // `get_use_access_times`' `event_kind = 'use'` predicate can never be
    // confused by a NULL.
    ensure_column(
        conn,
        "access_history",
        "event_kind",
        "TEXT NOT NULL DEFAULT 'display'",
    )?;

    // Temporal edge columns for memory_edges
    ensure_column(
        conn,
        "memory_edges",
        "valid_from",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(conn, "memory_edges", "valid_to", "TEXT")?;

    // derived_items columns that may be missing on legacy databases
    ensure_column(conn, "derived_items", "summary", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(
        conn,
        "derived_items",
        "importance",
        "REAL NOT NULL DEFAULT 0.5",
    )?;
    ensure_column(
        conn,
        "derived_items",
        "scope",
        "TEXT NOT NULL DEFAULT 'general'",
    )?;
    ensure_column(
        conn,
        "derived_items",
        "created_at",
        "TEXT NOT NULL DEFAULT ''",
    )?;

    // ── Product-scoped legacy-column work (#1585 D3) ──────────────────────
    //
    // Everything above this line ensures columns on PORTABLE tables and is
    // unconditional. Everything inside this block touches tables a
    // `PortableKernel` store never creates (`hub_capabilities`, `exec_envs`,
    // `session_claims`, `vault_entries`), so an `ALTER TABLE` here would fail
    // with `no such table` rather than being a harmless no-op. The guard is an
    // explicit profile check, not a `table_exists` sniff: sniffing would
    // quietly "adapt" to a half-built full store instead of refusing it.
    if profile.includes_product() {
        init_product_schema_columns(conn)?;
    }

    // Indexes on migrated columns — MUST come after ensure_column so the
    // columns exist on legacy databases that were created without them.
    execute_schema_chunks(conn, ddl::MIGRATED_INDEXES_CHUNKS, profile)?;

    // Backfill empty values for legacy rows.
    conn.execute(
        "UPDATE memories SET created_at = timestamp WHERE created_at IS NULL OR created_at = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET updated_at = created_at WHERE updated_at IS NULL OR updated_at = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET revision = 1 WHERE revision IS NULL OR revision <= 0",
        [],
    )?;
    normalize_memory_validity_columns(conn)?;

    bridge_hypertachi_memory_columns(conn)?;
    crate::db::migrations::fold_and_drop_legacy_persons_column(conn)?;
    // Relocate legacy location before enum rebuild copies rows without that column.
    let _ = crate::db::migrations::migrate_v9_relocate_and_drop_location(conn)?;

    // Install the authority before any projection-only drift repair. File-backed
    // opens run this whole block inside BEGIN IMMEDIATE, so repaired FTS rows and
    // the bump commit together. A later memories-table rebuild may drop these
    // triggers; the second ensure below reinstalls and validates them.
    crate::db::search_generation::ensure_search_generation_schema(conn)?;
    ensure_fts_backfilled(conn)?;

    migrate_enum_constraints(conn)?;

    // Must run after any legacy `memories` rebuild because SQLite drops table
    // triggers during that migration. The trigger is the cross-process cache
    // authority; drift is an open failure, never a silently stale cache hit.
    crate::db::search_generation::ensure_search_generation_schema(conn)?;
    ensure_optimization_indexes(conn);

    // NOTE: sqlite-vec virtual table (memories_vec) is created separately after
    // the extension is loaded by the caller via register_sqlite_vec().
    Ok(())
}

/// Evolutionary columns on PRODUCT tables, plus the one pre-index repair that
/// touches a product table. Reached only when the effective profile
/// [`crate::db::StoreProfile::includes_product`] (#1585 D3) — the tables these
/// statements alter simply do not exist on a `PortableKernel` store.
///
/// Split out of `init_schema_inner` rather than sprinkled with `if` so the
/// portable/product line is a single visible seam: anything added here is
/// product by construction.
fn init_product_schema_columns(conn: &Connection) -> Result<(), MemoryError> {
    // Hub governance columns for review + health + routing metadata
    ensure_column(
        conn,
        "hub_capabilities",
        "review_status",
        "TEXT NOT NULL DEFAULT 'approved'",
    )?;
    ensure_column(
        conn,
        "hub_capabilities",
        "health_status",
        "TEXT NOT NULL DEFAULT 'healthy'",
    )?;
    ensure_column(conn, "hub_capabilities", "last_error", "TEXT")?;
    ensure_column(conn, "hub_capabilities", "last_success_at", "TEXT")?;
    ensure_column(conn, "hub_capabilities", "last_failure_at", "TEXT")?;
    ensure_column(
        conn,
        "hub_capabilities",
        "fail_streak",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "hub_capabilities", "active_version", "TEXT")?;
    ensure_column(
        conn,
        "hub_capabilities",
        "exposure_mode",
        "TEXT NOT NULL DEFAULT 'direct'",
    )?;

    // Vault entry ACL column. This used to live inside `ensure_fts_backfilled`,
    // which has nothing to do with the vault; it is a product-table
    // `ensure_column` and belongs with the others (#1585 D3). Ordering is
    // unchanged in effect: no index or migration between the two positions
    // reads `vault_entries.allowed_agents`.
    ensure_column(conn, "vault_entries", "allowed_agents", "TEXT")?;

    // v21 identity/WorkClaim spine columns referenced by the product chunks of
    // MIGRATED_INDEXES_CHUNKS below. The v21 sentinel migration
    // (identity_workclaim_spine.rs) also adds these, but that migration runs
    // AFTER init_schema_inner — so on a legacy pre-v21 DB the index build below
    // would `no such column`-crash unless the columns are ensured here first
    // (#1289). Idempotent: no-op on a fresh DB whose CREATE TABLE already
    // carries them, and the later v21 ALTER is then skipped by its own
    // `column_exists` guard.
    ensure_column(conn, "exec_envs", "agent_identity_id", "TEXT")?;
    ensure_column(conn, "exec_envs", "claim_id", "TEXT")?;
    ensure_column(conn, "session_claims", "mode", "TEXT")?;

    // #1289: collapse any pre-existing duplicate *modeless* active claims for
    // the same identity triple BEFORE MIGRATED_INDEXES_CHUNKS builds the partial
    // UNIQUE index `idx_session_claims_identity_active` (WHERE state = 'active'
    // AND mode IS NULL). A legacy DB written by the pre-#1001-round-2 kernel can
    // carry such duplicates; without this the CREATE UNIQUE INDEX below crashes
    // init on that DB (a crash previously masked by the mode-column ordering
    // bug fixed above). Reuses the v12 migration's dedup logic (single source),
    // scoped to `mode IS NULL` to match the index predicate exactly — v21
    // WorkClaims carrying a non-null mode legitimately share an identity and are
    // never deduped. No-op on a fresh/empty table and idempotent on every
    // subsequent startup (the index then prevents any new duplicate).
    crate::db::migrations::dedupe_session_claims_identity_conflicts(conn)?;
    Ok(())
}

/// Keep the scorer-only diagnostic column total for legacy databases.
pub(crate) fn ensure_memories_scored_count(conn: &Connection) -> Result<(), MemoryError> {
    ensure_column(
        conn,
        "memories",
        "scored_count",
        "INTEGER NOT NULL DEFAULT 0",
    )
}

pub(crate) fn install_recall_impression_ledger_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::RECALL_IMPRESSION_LEDGER_V26_SQL)
}

/// Install the historical v25 schema only as a step in the in-order migration
/// runner. New provisioning reaches v26 in the same transaction immediately
/// afterward; no current database is left at this shape by this binary.
pub(crate) fn install_v25_recall_impression_ledger_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::RECALL_IMPRESSION_LEDGER_V25_SQL)
}

/// Upgrade the v25 group table without inventing query fingerprints or replay
/// policy for rows whose source query and historical algorithm are unavailable.
pub(crate) fn migrate_recall_impression_ledger_to_v26(
    conn: &Connection,
) -> Result<(), MemoryError> {
    if !has_column(conn, "recall_impression_groups", "query_hash")? {
        return Err(MemoryError::InvalidArg(
            "incomplete v25 recall impression ledger: query_hash is missing".to_string(),
        ));
    }

    // Rebuild both tables rather than merely adding nullable columns. That
    // preserves the v26 all-or-none policy CHECK for future writes while
    // retaining each historical v25 group as an explicitly unversioned row.
    // The caller's outer migration transaction makes this table swap atomic.
    execute_batch_retry(
        conn,
        "ALTER TABLE recall_impressions RENAME TO recall_impressions_v25;
         ALTER TABLE recall_impression_groups RENAME TO recall_impression_groups_v25;
         DROP INDEX idx_recall_impression_groups_created;
         DROP INDEX idx_recall_impression_groups_query_hash;
         DROP INDEX idx_recall_impressions_memory;
         DROP INDEX idx_recall_impressions_group_final_rank;",
    )?;
    install_recall_impression_ledger_schema(conn)?;
    // NULL is the only honest migration value: v25 did not persist query text
    // or replay-policy provenance, so neither can be reconstructed now.
    conn.execute(
        "INSERT INTO recall_impression_groups (group_id, created_at, legacy_query_bucket, query_fingerprint, fusion_policy_version, pre_boost_adjustment_version, tie_break_policy_version, candidate_policy_version, schema_identity, weights_profile, semantic_weight, fts_weight, symbolic_weight, decay_weight, use_rrf, rrf_k, top_k, candidate_count, displayed_count, scored_returned_count, replay_count)
         SELECT group_id, created_at, query_hash, NULL, NULL, NULL, NULL, NULL, NULL, weights_profile, semantic_weight, fts_weight, symbolic_weight, decay_weight, use_rrf, rrf_k, top_k, candidate_count, displayed_count, scored_returned_count, replay_count
         FROM recall_impression_groups_v25",
        [],
    )?;
    conn.execute(
        "INSERT INTO recall_impressions (group_id, memory_id, vector_score, fts_score, symbolic_score, decay_score, vec_rank, fts_rank, sym_rank, merge_adjustment, pre_boost_score, pre_boost_rank, tie_break_epoch_millis, final_score, final_rank, scored, scored_returned, access_count_at_recall)
         SELECT group_id, memory_id, vector_score, fts_score, symbolic_score, decay_score, vec_rank, fts_rank, sym_rank, merge_adjustment, pre_boost_score, pre_boost_rank, tie_break_epoch_millis, final_score, final_rank, scored, scored_returned, access_count_at_recall
         FROM recall_impressions_v25",
        [],
    )?;
    execute_batch_retry(
        conn,
        "DROP TABLE recall_impressions_v25;
         DROP TABLE recall_impression_groups_v25;",
    )?;
    validate_recall_impression_ledger_schema(conn)
}

/// Canonical v27 installer. Production callers reach this only through the
/// sentinel-gated migration runner, after the typed migration-authority gate.
pub(crate) fn install_typo_fallback_attribution_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    for (table, column, definition) in [
        (
            "recall_impression_groups",
            "typo_fallback_activated",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "recall_impression_groups",
            "typo_fallback_prefilter_count",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "recall_impression_groups",
            "typo_fallback_compared_count",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "recall_impression_groups",
            "typo_fallback_token_comparison_count",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "recall_impression_groups",
            "typo_fallback_edit_cell_count",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "recall_impression_groups",
            "typo_fallback_candidate_count",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "recall_impressions",
            "typo_fallback_candidate",
            "INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        ensure_column(conn, table, column, definition)?;
    }
    Ok(())
}

pub(crate) fn validate_typo_fallback_attribution_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    const REQUIRED_COLUMNS: &[(&str, &str)] = &[
        ("recall_impression_groups", "typo_fallback_activated"),
        ("recall_impression_groups", "typo_fallback_prefilter_count"),
        ("recall_impression_groups", "typo_fallback_compared_count"),
        (
            "recall_impression_groups",
            "typo_fallback_token_comparison_count",
        ),
        ("recall_impression_groups", "typo_fallback_edit_cell_count"),
        ("recall_impression_groups", "typo_fallback_candidate_count"),
        ("recall_impressions", "typo_fallback_candidate"),
    ];
    for (table, column) in REQUIRED_COLUMNS {
        let sql = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1");
        let present = match conn.query_row(&sql, [column], |_| Ok(())) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(error) => return Err(error.into()),
        };
        if !present {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v27 typo fallback attribution: required column '{table}.{column}' is missing"
            )));
        }
    }
    Ok(())
}

/// Canonical v29 installer for the durable outbox (tachi#1643). Production
/// callers reach this only through the sentinel-gated migration runner after
/// migration authority has been checked.
pub(crate) fn install_memory_outbox_schema(conn: &Connection) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::MEMORY_OUTBOX_V29_SQL)
}

/// Refuse a `memory_outbox_events` shape this kernel's typed writers cannot
/// trust: a missing table/index, a drifted column list, or — the one
/// `pragma_table_info` cannot see — a missing or rewritten state CHECK
/// constraint. A table without that CHECK would accept a state token
/// [`crate::db::outbox::OutboxState`] cannot parse, turning every later read
/// into a typed refusal against data the schema itself allowed in.
pub(crate) fn validate_memory_outbox_schema(conn: &Connection) -> Result<(), MemoryError> {
    const REQUIRED_OBJECTS: &[(&str, &str)] = &[
        ("table", "memory_outbox_events"),
        ("index", "idx_memory_outbox_events_state_created"),
        ("index", "idx_memory_outbox_events_state_changed"),
        ("index", "idx_memory_outbox_events_object"),
    ];
    for (object_type, name) in REQUIRED_OBJECTS {
        let present = match conn.query_row(
            "SELECT 1 FROM main.sqlite_schema
             WHERE type = ?1 AND name = ?2 AND tbl_name = 'memory_outbox_events'",
            params![object_type, name],
            |_| Ok(()),
        ) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(error) => return Err(error.into()),
        };
        if !present {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v29 memory outbox: required {object_type} '{name}' on \
                 'memory_outbox_events' is missing"
            )));
        }
    }

    const OUTBOX_COLUMNS: &[(&str, &str, bool, i64)] = &[
        ("event_id", "TEXT", true, 1),
        ("object_id", "TEXT", true, 0),
        ("object_class", "TEXT", true, 0),
        ("authority_class", "TEXT", true, 0),
        ("source_store", "TEXT", true, 0),
        ("source_partition", "TEXT", true, 0),
        ("source_revision", "INTEGER", true, 0),
        ("payload_digest", "TEXT", true, 0),
        ("state", "TEXT", true, 0),
        ("last_error_class", "TEXT", false, 0),
        ("created_at", "TEXT", true, 0),
        ("state_changed_at", "TEXT", true, 0),
    ];
    let mut stmt = conn.prepare(
        "SELECT name, upper(type), [notnull] != 0, pk
         FROM pragma_table_info('memory_outbox_events') ORDER BY cid",
    )?;
    let actual = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected = OUTBOX_COLUMNS
        .iter()
        .map(|(name, ty, not_null, pk)| ((*name).to_string(), (*ty).to_string(), *not_null, *pk))
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(MemoryError::InvalidArg(
            "incomplete v29 memory outbox: table 'memory_outbox_events' has non-canonical column \
             shape"
                .to_string(),
        ));
    }

    let table_sql: String = conn.query_row(
        "SELECT COALESCE(sql, '') FROM main.sqlite_schema
         WHERE type = 'table' AND name = 'memory_outbox_events'",
        [],
        |row| row.get(0),
    )?;
    if !normalize_schema_sql(&table_sql)
        .contains(&normalize_schema_sql(ddl::MEMORY_OUTBOX_STATE_CHECK_CLAUSE))
    {
        return Err(MemoryError::InvalidArg(
            "incomplete v29 memory outbox: table 'memory_outbox_events' is missing the canonical \
             state CHECK constraint"
                .to_string(),
        ));
    }
    Ok(())
}

/// Canonical v30 installer for durable destination-side outbox apply
/// receipts (#1718).  It is portable surface and therefore intentionally
/// takes no `StoreProfile` argument.
pub(crate) fn install_memory_outbox_destination_apply_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::MEMORY_OUTBOX_DESTINATION_APPLY_V30_SQL)
}

/// Refuse a destination receipt ledger whose shape or closed application
/// vocabulary has drifted.  The Rust apply seam treats this table as an
/// immutable invariant ledger; accepting a widened or truncated shape would
/// make duplicate classification reconstruct facts that were never durable.
pub(crate) fn validate_memory_outbox_destination_apply_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    const REQUIRED_OBJECTS: &[(&str, &str)] = &[
        ("table", "memory_outbox_destination_apply_receipts"),
        ("index", "idx_memory_outbox_destination_apply_object"),
    ];
    for (object_type, name) in REQUIRED_OBJECTS {
        let present = match conn.query_row(
            "SELECT 1 FROM main.sqlite_schema
             WHERE type = ?1 AND name = ?2
               AND (type = 'table' OR tbl_name = 'memory_outbox_destination_apply_receipts')",
            params![object_type, name],
            |_| Ok(()),
        ) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(error) => return Err(error.into()),
        };
        if !present {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v30 destination apply ledger: required {object_type} '{name}' is missing"
            )));
        }
    }

    const REQUIRED_COLUMNS: &[(&str, &str, bool, i64)] = &[
        ("event_id", "TEXT", true, 1),
        ("object_id", "TEXT", true, 0),
        ("source_store", "TEXT", true, 0),
        ("source_partition", "TEXT", true, 0),
        ("source_revision", "INTEGER", true, 0),
        ("source_payload_digest", "TEXT", true, 0),
        ("destination_store", "TEXT", true, 0),
        ("destination_partition", "TEXT", true, 0),
        ("destination_object_revision", "INTEGER", true, 0),
        ("destination_payload_digest", "TEXT", true, 0),
        ("application", "TEXT", true, 0),
    ];
    let mut stmt = conn.prepare(
        "SELECT name, upper(type), [notnull] != 0, pk
         FROM pragma_table_info('memory_outbox_destination_apply_receipts')
         ORDER BY cid",
    )?;
    let actual = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected = REQUIRED_COLUMNS
        .iter()
        .map(|(name, ty, not_null, pk)| ((*name).to_string(), (*ty).to_string(), *not_null, *pk))
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(MemoryError::InvalidArg(
            "incomplete v30 destination apply ledger: table has non-canonical column shape"
                .to_string(),
        ));
    }

    let table_sql: String = conn.query_row(
        "SELECT COALESCE(sql, '') FROM main.sqlite_schema
         WHERE type = 'table' AND name = 'memory_outbox_destination_apply_receipts'",
        [],
        |row| row.get(0),
    )?;
    if !normalize_schema_sql(&table_sql).contains(&normalize_schema_sql(
        ddl::MEMORY_OUTBOX_DESTINATION_APPLY_APPLICATION_CHECK_CLAUSE,
    )) {
        return Err(MemoryError::InvalidArg(
            "incomplete v30 destination apply ledger: table is missing the canonical application CHECK constraint"
                .to_string(),
        ));
    }
    Ok(())
}

/// Canonical v31 installer for host-owned ACP attachment receipts (#1733).
/// The descriptor itself is projected at request time; this table stores only
/// the identity/revision/policy receipt needed to reproduce that projection.
pub(crate) fn install_harness_session_attachments_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    execute_batch_retry(
        conn,
        "CREATE TABLE IF NOT EXISTS harness_session_attachments (
            attachment_id TEXT PRIMARY KEY NOT NULL,
            host_identity TEXT NOT NULL CHECK (length(trim(host_identity)) > 0),
            protocol_version TEXT NOT NULL CHECK (length(trim(protocol_version)) > 0),
            adapter_connection_identity TEXT NOT NULL CHECK (length(trim(adapter_connection_identity)) > 0),
            remote_session_id TEXT NOT NULL CHECK (length(trim(remote_session_id)) > 0),
            work_claim_id TEXT NOT NULL CHECK (length(trim(work_claim_id)) > 0),
            expected_transition_version INTEGER NOT NULL CHECK (expected_transition_version >= 0),
            agent_identity_id TEXT NOT NULL CHECK (length(trim(agent_identity_id)) > 0),
            contract_digest TEXT NOT NULL CHECK (length(trim(contract_digest)) > 0),
            capabilities_json TEXT NOT NULL CHECK (json_valid(capabilities_json)),
            tool_profile TEXT NOT NULL CHECK (length(trim(tool_profile)) > 0),
            capability_class TEXT NOT NULL CHECK (length(trim(capability_class)) > 0),
            policy_digest TEXT NOT NULL CHECK (length(trim(policy_digest)) > 0),
            descriptor_digest TEXT NOT NULL CHECK (length(trim(descriptor_digest)) > 0),
            idempotency_key TEXT NOT NULL CHECK (length(trim(idempotency_key)) > 0),
            admission_receipt_ref TEXT NOT NULL CHECK (length(trim(admission_receipt_ref)) > 0),
            state TEXT NOT NULL CHECK (state IN ('attached', 'reconnect_failed', 'unknown')),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE (idempotency_key),
            UNIQUE (host_identity, protocol_version, adapter_connection_identity, remote_session_id)
        );
        CREATE INDEX IF NOT EXISTS idx_harness_session_attachments_claim
            ON harness_session_attachments(work_claim_id, expected_transition_version);
        CREATE INDEX IF NOT EXISTS idx_harness_session_attachments_receipt
            ON harness_session_attachments(admission_receipt_ref, created_at);",
    )
}

/// Refuse a drifted v31 attachment receipt shape before a current-schema open
/// can hand it to a typed writer.  The state CHECK is checked separately from
/// `pragma_table_info`, because SQLite does not expose CHECK constraints in
/// that pragma.
pub(crate) fn validate_harness_session_attachments_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    const REQUIRED_OBJECTS: &[(&str, &str)] = &[
        ("table", "harness_session_attachments"),
        ("index", "idx_harness_session_attachments_claim"),
        ("index", "idx_harness_session_attachments_receipt"),
    ];
    for (object_type, name) in REQUIRED_OBJECTS {
        let present = match conn.query_row(
            "SELECT 1 FROM main.sqlite_schema
             WHERE type = ?1 AND name = ?2
               AND (type = 'table' OR tbl_name = 'harness_session_attachments')",
            params![object_type, name],
            |_| Ok(()),
        ) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(error) => return Err(error.into()),
        };
        if !present {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v31 ACP attachment ledger: required {object_type} '{name}' is missing"
            )));
        }
    }

    const REQUIRED_COLUMNS: &[(&str, &str, bool, i64)] = &[
        ("attachment_id", "TEXT", true, 1),
        ("host_identity", "TEXT", true, 0),
        ("protocol_version", "TEXT", true, 0),
        ("adapter_connection_identity", "TEXT", true, 0),
        ("remote_session_id", "TEXT", true, 0),
        ("work_claim_id", "TEXT", true, 0),
        ("expected_transition_version", "INTEGER", true, 0),
        ("agent_identity_id", "TEXT", true, 0),
        ("contract_digest", "TEXT", true, 0),
        ("capabilities_json", "TEXT", true, 0),
        ("tool_profile", "TEXT", true, 0),
        ("capability_class", "TEXT", true, 0),
        ("policy_digest", "TEXT", true, 0),
        ("descriptor_digest", "TEXT", true, 0),
        ("idempotency_key", "TEXT", true, 0),
        ("admission_receipt_ref", "TEXT", true, 0),
        ("state", "TEXT", true, 0),
        ("created_at", "TEXT", true, 0),
        ("updated_at", "TEXT", true, 0),
    ];
    let mut stmt = conn.prepare(
        "SELECT name, upper(type), [notnull] != 0, pk
         FROM pragma_table_info('harness_session_attachments') ORDER BY cid",
    )?;
    let actual = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected = REQUIRED_COLUMNS
        .iter()
        .map(|(name, ty, not_null, pk)| ((*name).to_string(), (*ty).to_string(), *not_null, *pk))
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(MemoryError::InvalidArg(
            "incomplete v31 ACP attachment ledger: table has non-canonical column shape"
                .to_string(),
        ));
    }

    let table_sql: String = conn.query_row(
        "SELECT COALESCE(sql, '') FROM main.sqlite_schema
         WHERE type = 'table' AND name = 'harness_session_attachments'",
        [],
        |row| row.get(0),
    )?;
    let normalized = normalize_schema_sql(&table_sql);
    for clause in [
        "CHECK (state IN ('attached', 'reconnect_failed', 'unknown'))",
        "UNIQUE (idempotency_key)",
        "UNIQUE (host_identity, protocol_version, adapter_connection_identity, remote_session_id)",
    ] {
        if !normalized.contains(&normalize_schema_sql(clause)) {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v31 ACP attachment ledger: table is missing canonical clause {clause:?}"
            )));
        }
    }
    Ok(())
}

/// Collapse every run of ASCII whitespace to one space so a stored
/// `sqlite_schema.sql` can be compared against a canonical clause without the
/// comparison depending on the formatting SQLite echoed back.
fn normalize_schema_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Canonical v28 installer. Production callers reach this only through the
/// sentinel-gated migration runner after migration authority has been checked.
pub(crate) fn install_wiki_recovery_ledgers_schema(conn: &Connection) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::WIKI_RECOVERY_LEDGERS_V28_SQL)
}

pub(crate) fn validate_wiki_recovery_ledgers_schema(conn: &Connection) -> Result<(), MemoryError> {
    const REQUIRED_OBJECTS: &[(&str, &str, &str)] = &[
        ("table", "rem_source_claims", "rem_source_claims"),
        ("index", "idx_rem_source_claims_draft", "rem_source_claims"),
        (
            "table",
            "exact_dedupe_apply_lineage",
            "exact_dedupe_apply_lineage",
        ),
    ];
    for (object_type, name, table) in REQUIRED_OBJECTS {
        let present = match conn.query_row(
            "SELECT 1 FROM main.sqlite_schema
             WHERE type = ?1 AND name = ?2 AND tbl_name = ?3",
            params![object_type, name, table],
            |_| Ok(()),
        ) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(error) => return Err(error.into()),
        };
        if !present {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v28 Wiki recovery ledgers: required {object_type} '{name}' on '{table}' is missing"
            )));
        }
    }

    const REM_SOURCE_CLAIMS_COLUMNS: &[(&str, &str, bool, i64)] = &[
        ("source_key", "TEXT", true, 1),
        ("source_identity", "TEXT", true, 0),
        ("draft_id", "TEXT", true, 0),
        ("claimed_at", "TEXT", true, 0),
    ];
    const EXACT_DEDUPE_LINEAGE_COLUMNS: &[(&str, &str, bool, i64)] = &[
        ("loser_id", "TEXT", true, 1),
        ("apply_id", "TEXT", true, 0),
        ("plan_digest", "TEXT", true, 0),
        ("winner_id", "TEXT", true, 0),
        ("before_revision", "INTEGER", true, 0),
        ("archived_revision", "INTEGER", true, 0),
        ("loser_valid_until_before", "TEXT", false, 0),
        ("applied_at", "TEXT", true, 0),
    ];
    let validate_columns = |table: &str,
                            expected: &[(&str, &str, bool, i64)]|
     -> Result<(), MemoryError> {
        let mut stmt = conn.prepare(
            "SELECT name, upper(type), [notnull] != 0, pk
                 FROM pragma_table_info(?1) ORDER BY cid",
        )?;
        let actual = stmt
            .query_map([table], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let expected = expected
            .iter()
            .map(|(name, ty, not_null, pk)| {
                ((*name).to_string(), (*ty).to_string(), *not_null, *pk)
            })
            .collect::<Vec<_>>();
        if actual != expected {
            return Err(MemoryError::InvalidArg(format!(
                    "incomplete v28 Wiki recovery ledgers: table '{table}' has non-canonical column shape"
                )));
        }
        Ok(())
    };
    validate_columns("rem_source_claims", REM_SOURCE_CLAIMS_COLUMNS)?;
    validate_columns("exact_dedupe_apply_lineage", EXACT_DEDUPE_LINEAGE_COLUMNS)?;

    let (unique, partial): (bool, bool) = conn.query_row(
        "SELECT [unique] != 0, partial != 0
         FROM pragma_index_list('rem_source_claims')
         WHERE name = 'idx_rem_source_claims_draft'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let index_columns = conn
        .prepare(
            "SELECT name FROM pragma_index_info('idx_rem_source_claims_draft') ORDER BY seqno",
        )?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if unique || partial || index_columns != ["draft_id"] {
        return Err(MemoryError::InvalidArg(
            "incomplete v28 Wiki recovery ledgers: index 'idx_rem_source_claims_draft' has non-canonical shape"
                .to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_recall_impression_ledger_schema(
    conn: &Connection,
) -> Result<(), MemoryError> {
    const REQUIRED_OBJECTS: &[(&str, &str, &str)] = &[
        (
            "table",
            "recall_impression_groups",
            "recall_impression_groups",
        ),
        ("table", "recall_impressions", "recall_impressions"),
        (
            "index",
            "idx_recall_impression_groups_created",
            "recall_impression_groups",
        ),
        (
            "index",
            "idx_recall_impression_groups_fingerprint",
            "recall_impression_groups",
        ),
        (
            "index",
            "idx_recall_impressions_memory",
            "recall_impressions",
        ),
        (
            "index",
            "idx_recall_impressions_group_final_rank",
            "recall_impressions",
        ),
    ];
    const REQUIRED_GROUP_COLUMNS: &[&str] = &[
        "legacy_query_bucket",
        "query_fingerprint",
        "fusion_policy_version",
        "pre_boost_adjustment_version",
        "tie_break_policy_version",
        "candidate_policy_version",
        "schema_identity",
    ];

    for (object_type, name, table) in REQUIRED_OBJECTS {
        let present = match conn.query_row(
            "SELECT 1 FROM main.sqlite_schema
                 WHERE type = ?1 AND name = ?2 AND tbl_name = ?3",
            params![object_type, name, table],
            |_| Ok(()),
        ) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(error) => return Err(error.into()),
        };
        if !present {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v26 recall impression ledger: required {object_type} '{name}' on '{table}' is missing"
            )));
        }
    }
    for column in REQUIRED_GROUP_COLUMNS {
        if !has_column(conn, "recall_impression_groups", column)? {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete v26 recall impression ledger: required column '{column}' is missing"
            )));
        }
    }
    let malformed_identity_exists: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM recall_impression_groups
             WHERE CASE
                 WHEN query_fingerprint IS NULL
                      AND fusion_policy_version IS NULL
                      AND pre_boost_adjustment_version IS NULL
                      AND tie_break_policy_version IS NULL
                      AND candidate_policy_version IS NULL
                      AND schema_identity IS NULL
                 THEN 0
                 WHEN query_fingerprint IS NOT NULL
                      AND length(query_fingerprint) = 64
                      AND query_fingerprint NOT GLOB '*[^0-9a-f]*'
                      AND fusion_policy_version IS NOT NULL
                      AND pre_boost_adjustment_version IS NOT NULL
                      AND tie_break_policy_version IS NOT NULL
                      AND candidate_policy_version IS NOT NULL
                      AND schema_identity IS NOT NULL
                 THEN 0
                 ELSE 1
             END = 1
             LIMIT 1
         )",
        [],
        |row| row.get(0),
    )?;
    if malformed_identity_exists {
        return Err(MemoryError::InvalidArg(
            "incomplete v26 recall impression ledger: malformed replay identity row".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn install_reserved_reference_guard(conn: &Connection) -> Result<(), MemoryError> {
    execute_batch_retry(conn, ddl::RESERVED_REFERENCE_GUARD_SQL)
}

pub(crate) fn expected_reserved_reference_trigger(
    name: &str,
) -> Option<(&'static str, &'static str)> {
    if name.eq_ignore_ascii_case(ddl::RESERVED_REFERENCE_INSERT_TRIGGER_NAME) {
        Some((
            ddl::RESERVED_REFERENCE_INSERT_TRIGGER_NAME,
            ddl::RESERVED_REFERENCE_INSERT_TRIGGER_SQL,
        ))
    } else if name.eq_ignore_ascii_case(ddl::RESERVED_REFERENCE_UPDATE_TRIGGER_NAME) {
        Some((
            ddl::RESERVED_REFERENCE_UPDATE_TRIGGER_NAME,
            ddl::RESERVED_REFERENCE_UPDATE_TRIGGER_SQL,
        ))
    } else {
        None
    }
}

fn ensure_optimization_indexes(conn: &Connection) {
    if has_column(conn, "memories", "superseded_by").unwrap_or(false) {
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_path_active_ts \
             ON memories(path, timestamp DESC) \
             WHERE archived = 0 AND superseded_by IS NULL",
            [],
        );
    }
}

fn execute_batch_retry(conn: &Connection, sql: &str) -> Result<(), MemoryError> {
    retry_locked(|| conn.execute_batch(sql).map(|_| ()))
}

fn execute_retry(conn: &Connection, sql: &str) -> Result<usize, MemoryError> {
    retry_locked(|| conn.execute(sql, []))
}

fn retry_locked<T>(
    mut operation: impl FnMut() -> Result<T, rusqlite::Error>,
) -> Result<T, MemoryError> {
    let mut backoff = Duration::from_millis(10);
    let max_backoff = Duration::from_millis(250);
    let attempts = 24;

    for attempt in 1..=attempts {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error)
                if is_locked_error(&error)
                    && attempt < attempts
                    && super::sqlite_busy_deadline_remaining()
                        .is_none_or(|remaining| !remaining.is_zero() && backoff < remaining) =>
            {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(error) => return Err(error.into()),
        }
    }

    unreachable!("retry loop returns on every final attempt")
}

fn is_locked_error(error: &rusqlite::Error) -> bool {
    super::sqlite_error_is_locked(error)
}

fn is_duplicate_column_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.contains("duplicate column name")
    )
}

/// Align HyperTachi-shaped legacy DBs (`indexed_tags`, `domain_key`) with Sigil's
/// canonical columns before enum/CHECK migrations run.
fn bridge_hypertachi_memory_columns(conn: &Connection) -> Result<(), MemoryError> {
    ensure_column(conn, "memories", "domain", "TEXT")?;

    if has_column(conn, "memories", "indexed_tags")? {
        conn.execute(
            "UPDATE memories
             SET keywords = indexed_tags
             WHERE (keywords IS NULL OR trim(keywords) IN ('', '[]'))
               AND indexed_tags IS NOT NULL
               AND trim(indexed_tags) NOT IN ('', '[]')",
            [],
        )?;
    }

    if has_column(conn, "memories", "domain_key")? {
        conn.execute(
            "UPDATE memories
             SET domain = domain_key
             WHERE (domain IS NULL OR trim(COALESCE(domain, '')) = '')
               AND domain_key IS NOT NULL
               AND trim(domain_key) <> ''",
            [],
        )?;
    }

    // Undo mistaken v1 bridge that copied domain_key into location (pre-v9 DBs only).
    if has_column(conn, "memories", "location")? {
        conn.execute(
            "UPDATE memories
         SET domain = location, location = ''
         WHERE (domain IS NULL OR trim(COALESCE(domain, '')) = '')
           AND trim(location) <> ''
           AND location NOT GLOB '/*'
           AND location NOT LIKE '%/%'
           AND location NOT LIKE '% %'",
            [],
        )?;
    }

    Ok(())
}

/// Called from inside `init_schema_inner`, which itself runs either
/// standalone (via [`init_schema`], no enclosing transaction) or nested
/// inside `init_schema_with_label_mut`'s outer `BEGIN IMMEDIATE` (#984 F1
/// round 3) — so this uses a `SAVEPOINT` rather than a raw `BEGIN`, which
/// nests cleanly in either context (SQLite forbids nested top-level `BEGIN`
/// but savepoints nest, and a savepoint with no enclosing transaction
/// behaves like one).
fn normalize_memory_validity_columns(conn: &Connection) -> Result<(), MemoryError> {
    let rows = {
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, valid_from, valid_until FROM memories \
             WHERE valid_from = '' OR valid_from IS NULL",
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row?);
        }
        rows
    };

    conn.execute_batch("SAVEPOINT normalize_memory_validity_columns")?;
    let result = (|| -> Result<(), MemoryError> {
        for (id, timestamp, valid_from, valid_until) in rows {
            let valid_from_raw = valid_from
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(timestamp.trim());
            let normalized_from =
                normalize_utc_iso(valid_from_raw).unwrap_or_else(|_| valid_from_raw.to_string());
            let normalized_until = valid_until
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| normalize_utc_iso(value).unwrap_or_else(|_| value.to_string()));
            if valid_from.as_deref() == Some(normalized_from.as_str())
                && valid_until == normalized_until
            {
                continue;
            }
            conn.execute(
                "UPDATE memories SET valid_from = ?2, valid_until = ?3 WHERE id = ?1",
                params![id, normalized_from, normalized_until],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE normalize_memory_validity_columns")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO normalize_memory_validity_columns");
            let _ = conn.execute_batch("RELEASE normalize_memory_validity_columns");
            Err(e)
        }
    }
}

/// Idempotent migration that:
///   1. Detects whether CHECK constraints are already in place (no-op if so).
///   2. Normalizes legacy `source` / `category` / `scope` / `retention_policy`
///      values to the canonical vocabulary defined in `types.rs`.
///   3. Backfills `retention_policy` defaults for /handoff, /kanban, /wiki, and
///      foundry_distill rows.
///   4. Rebuilds the `memories` table with CHECK constraints (SQLite cannot
///      ALTER TABLE ADD CHECK).
///
/// The standalone `memories_fts` virtual table is independent of the rebuild
/// and is preserved across the rename.
///
/// Called from inside `init_schema_inner`, which itself runs either
/// standalone (via [`init_schema`], no enclosing transaction) or nested
/// inside `init_schema_with_label_mut`'s outer `BEGIN IMMEDIATE` (#984 F1
/// round 3) — so this uses a `SAVEPOINT` rather than a raw `BEGIN`, which
/// nests cleanly in either context.
fn migrate_enum_constraints(conn: &Connection) -> Result<(), MemoryError> {
    let existing_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(sql) = existing_sql.as_deref() {
        // SQLite preserves formatting in sqlite_schema. Canonical rebuild SQL
        // writes `CHECK (` and `source` on separate lines, so a raw substring
        // probe falsely rebuilt the table on every open and dropped its
        // triggers. Remove formatting whitespace before testing the shape.
        let compact_sql: String = sql
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect();
        let has_source_check = compact_sql.contains("CHECK(source");
        // #964: 'sticky' is the newest category value: check it (not 'eval')
        // so DBs stamped before the sticky category was added re-run this
        // rebuild once, same idempotent-sentinel pattern as every prior
        // category/source addition here.
        let has_latest_category_values = sql.contains("'sticky'");
        let has_latest_source_values = sql.contains("'foundry_recall_rerank_cache'");
        if has_source_check && has_latest_category_values && has_latest_source_values {
            return Ok(());
        }
    }

    conn.execute_batch("SAVEPOINT migrate_enum_constraints")?;
    let migration_result = (|| -> Result<(), MemoryError> {
        normalize_source(conn)?;
        normalize_category(conn)?;
        normalize_scope(conn)?;
        normalize_retention_policy(conn)?;
        backfill_retention_defaults(conn)?;
        rebuild_memories_with_check_constraints(conn)?;
        Ok(())
    })();

    match migration_result {
        Ok(()) => {
            conn.execute_batch("RELEASE migrate_enum_constraints")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO migrate_enum_constraints");
            let _ = conn.execute_batch("RELEASE migrate_enum_constraints");
            Err(e)
        }
    }
}

const CANONICAL_SOURCES: &[&str] = &[
    "manual",
    "extraction",
    "migration",
    "auto",
    "foundry_distill",
    "foundry_recall_rerank_cache",
    "handoff",
    "kanban",
    "wiki",
    "ghost",
    "ingest_event",
];

fn normalize_source(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET source = 'manual'
         WHERE source IS NULL OR trim(source) = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET source = 'manual'
         WHERE lower(source) IN ('test', 'unit_test')",
        [],
    )?;

    loop {
        let rows = fetch_ghost_source_batch(conn, 500)?;
        if rows.is_empty() {
            break;
        }
        for (id, source, metadata) in rows {
            let publisher = source
                .strip_prefix("ghost:")
                .unwrap_or("")
                .trim()
                .to_string();
            let new_meta = match serde_json::from_str::<serde_json::Value>(&metadata) {
                Ok(mut v) => {
                    if !publisher.is_empty() {
                        let obj = v.as_object_mut();
                        if let Some(map) = obj {
                            let ghost_entry = map
                                .entry("ghost".to_string())
                                .or_insert_with(|| serde_json::json!({}));
                            if let Some(g) = ghost_entry.as_object_mut() {
                                g.insert(
                                    "publisher".to_string(),
                                    serde_json::Value::String(publisher.clone()),
                                );
                            }
                        }
                    }
                    Some(v.to_string())
                }
                Err(_) => None,
            };
            match new_meta {
                Some(m) => {
                    conn.execute(
                        "UPDATE memories SET source='ghost', metadata=?1 WHERE id=?2",
                        rusqlite::params![m, id],
                    )?;
                }
                None => {
                    conn.execute(
                        "UPDATE memories SET source='ghost' WHERE id=?1",
                        rusqlite::params![id],
                    )?;
                }
            }
        }
    }

    conn.execute(
        "UPDATE memories SET source = lower(source)
         WHERE source IN ('Manual','Extraction','Migration','Auto','FoundryDistill','Handoff','Kanban','Wiki','Ghost','IngestEvent')
            OR source GLOB '*[A-Z]*'",
        [],
    )?;

    loop {
        let rows = fetch_noncanonical_source_batch(conn, 500)?;
        if rows.is_empty() {
            break;
        }
        let mut changed = 0usize;
        for (id, source) in rows {
            if CANONICAL_SOURCES.contains(&source.as_str()) {
                continue;
            }
            if let Some(suffix) = source.strip_prefix("external:") {
                let sanitized = sanitize_source_suffix_sql(suffix);
                let new_val = format!("external:{}", sanitized);
                if new_val != source {
                    conn.execute(
                        "UPDATE memories SET source=?1 WHERE id=?2",
                        rusqlite::params![new_val, id],
                    )?;
                    changed += 1;
                }
                continue;
            }
            let sanitized = sanitize_source_suffix_sql(&source);
            let new_val = format!("external:{}", sanitized);
            conn.execute(
                "UPDATE memories SET source=?1 WHERE id=?2",
                rusqlite::params![new_val, id],
            )?;
            changed += 1;
        }
        if changed == 0 {
            break;
        }
    }

    Ok(())
}

fn normalize_category(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET category = lower(category) WHERE category IS NOT NULL",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET category = 'other'
         WHERE category IS NULL OR category = ''
            OR category NOT IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki','guide','eval','sticky')",
        [],
    )?;
    Ok(())
}

fn normalize_scope(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET scope = lower(scope) WHERE scope IS NOT NULL",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET scope = 'general'
         WHERE scope IS NULL OR scope NOT IN ('user','project','general')",
        [],
    )?;
    Ok(())
}

fn normalize_retention_policy(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET retention_policy = lower(retention_policy)
         WHERE retention_policy IS NOT NULL",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET retention_policy = NULL
         WHERE retention_policy IS NOT NULL
           AND retention_policy NOT IN ('ephemeral','durable','permanent','pinned')",
        [],
    )?;
    Ok(())
}

fn backfill_retention_defaults(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories SET retention_policy = 'pinned'
         WHERE retention_policy IS NULL
           AND (path LIKE '/handoff%' OR path LIKE '/kanban%')",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET retention_policy = 'permanent'
         WHERE retention_policy IS NULL AND (path LIKE '/wiki%' OR path LIKE '/guide%')",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET retention_policy = 'permanent'
         WHERE retention_policy IS NULL AND source = 'foundry_distill'",
        [],
    )?;
    Ok(())
}

fn rebuild_memories_with_check_constraints(conn: &Connection) -> Result<(), MemoryError> {
    let scored_count_expr = if has_column(conn, "memories", "scored_count")? {
        "COALESCE(scored_count, 0)"
    } else {
        "0"
    };
    let recall_count_expr = if has_column(conn, "memories", "recall_count")? {
        "COALESCE(recall_count, 0)"
    } else {
        "0"
    };
    let query_diversity_expr = if has_column(conn, "memories", "query_diversity")? {
        "COALESCE(query_diversity, 0)"
    } else {
        "0"
    };
    let tier_expr = if has_column(conn, "memories", "tier")? {
        "COALESCE(tier, 'raw')"
    } else {
        "'raw'"
    };
    let idless_identity_expr = if has_column(conn, "memories", "idless_identity")? {
        "idless_identity"
    } else {
        "NULL"
    };
    // tachi#1446. This rebuild runs on EVERY fresh DB (BASE_SCHEMA_SQL creates
    // `memories` without the CHECK constraints, so `migrate_enum_constraints`'
    // shape probe misses and calls through to here), and it is a DROP + CREATE
    // of the whole table from this literal column list. A column that exists
    // only in `ddl.rs` + `ensure_column` would therefore be added at
    // `init_schema_inner` and then dropped again a few statements later, before
    // the connection is ever used. Same guarded-expression shape as
    // `idless_identity` above, for the same reason.
    let last_use_at_expr = if has_column(conn, "memories", "last_use_at")? {
        "last_use_at"
    } else {
        "NULL"
    };
    // tachi#1459, on the `access_count` / `last_access` / `recall_count` /
    // `query_diversity` columns carried through the rebuild below: those
    // counters observe the search path only; reads through path-listing routes
    // do not increment them. See `ddl.rs`'s BASE_SCHEMA_SQL, where the same
    // columns carry the full note, and `db::record_access_with_updates`, their
    // single production writer. The rebuild preserves the stored values
    // verbatim — it neither widens nor narrows what they measure.
    let rebuild_sql = r#"
        CREATE TABLE memories_new (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            valid_from   TEXT NOT NULL DEFAULT '',
            valid_until  TEXT,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
            access_count INTEGER NOT NULL DEFAULT 0,
            scored_count INTEGER NOT NULL DEFAULT 0,
            last_access  TEXT,
            last_use_at  TEXT,
            revision     INTEGER NOT NULL DEFAULT 1,
             metadata     TEXT NOT NULL DEFAULT '{}',
             retention_policy TEXT,
             domain       TEXT,
             superseded_by TEXT,
             idless_identity TEXT,
             recall_count    INTEGER NOT NULL DEFAULT 0,
             query_diversity INTEGER NOT NULL DEFAULT 0,
             tier            TEXT NOT NULL DEFAULT 'raw',
             CHECK (category IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki','guide','eval','sticky')),
            CHECK (scope IN ('user','project','general')),
            CHECK (retention_policy IS NULL OR retention_policy IN ('ephemeral','durable','permanent','pinned')),
            CHECK (
                source IN ('manual','extraction','migration','auto','foundry_distill','foundry_recall_rerank_cache','handoff','kanban','wiki','ghost','ingest_event')
                OR source LIKE 'external:%'
            )
        );

        INSERT INTO memories_new
            (id, path, summary, text, importance, timestamp, valid_from, valid_until,
             category, topic, keywords, entities, source, scope, archived,
              created_at, updated_at, access_count, scored_count, last_access, last_use_at, revision,
             metadata, retention_policy, domain, superseded_by, idless_identity,
             recall_count, query_diversity, tier)
        SELECT
             id, path, summary, text, importance, timestamp,
             COALESCE(NULLIF(valid_from, ''), timestamp), NULLIF(valid_until, ''),
             category, topic, keywords, entities, source, scope, archived,
              created_at, updated_at, access_count, __SCORED_COUNT_EXPR__, last_access, __LAST_USE_AT_EXPR__, revision,
             metadata, retention_policy, domain, superseded_by, __IDLESS_IDENTITY_EXPR__,
             __RECALL_COUNT_EXPR__, __QUERY_DIVERSITY_EXPR__, __TIER_EXPR__
        FROM memories;

        DROP TABLE memories;
        ALTER TABLE memories_new RENAME TO memories;

        CREATE INDEX IF NOT EXISTS idx_memories_path        ON memories(path);
        CREATE INDEX IF NOT EXISTS idx_memories_importance  ON memories(importance DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_timestamp   ON memories(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_archived    ON memories(archived);
        CREATE INDEX IF NOT EXISTS idx_memories_last_access ON memories(last_access DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_valid_time  ON memories(valid_from, valid_until);
        CREATE INDEX IF NOT EXISTS idx_memories_retention_policy ON memories(retention_policy);
        CREATE INDEX IF NOT EXISTS idx_memories_domain ON memories(domain);
        CREATE INDEX IF NOT EXISTS idx_memories_superseded ON memories(superseded_by);
        CREATE INDEX IF NOT EXISTS idx_memories_tier ON memories(tier);
        CREATE INDEX IF NOT EXISTS idx_memories_recall ON memories(recall_count DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_path_active_ts ON memories(path, timestamp DESC) WHERE archived = 0 AND superseded_by IS NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_idless_identity_active
            ON memories(idless_identity)
            WHERE idless_identity IS NOT NULL AND archived = 0 AND superseded_by IS NULL;

        "#
        .replace("__RECALL_COUNT_EXPR__", recall_count_expr)
        .replace("__SCORED_COUNT_EXPR__", scored_count_expr)
        .replace("__QUERY_DIVERSITY_EXPR__", query_diversity_expr)
        .replace("__TIER_EXPR__", tier_expr)
        .replace("__IDLESS_IDENTITY_EXPR__", idless_identity_expr)
        .replace("__LAST_USE_AT_EXPR__", last_use_at_expr);
    conn.execute_batch(&rebuild_sql)?;
    Ok(())
}

fn fetch_ghost_source_batch(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, source, metadata FROM memories
         WHERE source LIKE 'ghost:%'
         ORDER BY id
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn fetch_noncanonical_source_batch(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, source FROM memories
         WHERE source NOT IN ('manual','extraction','migration','auto','foundry_distill','foundry_recall_rerank_cache','handoff','kanban','wiki','ghost','ingest_event')
           AND (
             source NOT LIKE 'external:%'
             OR source = 'external:'
             OR source != lower(source)
             OR source GLOB '*[^a-z0-9_:-]*'
             OR length(substr(source, 10)) > 64
           )
         ORDER BY id
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// SQL-side equivalent of `sanitize_source_suffix` in types.rs.
/// Lowercases, replaces non-`[a-z0-9_-]` with `_`, truncates to 64 chars.
fn sanitize_source_suffix_sql(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(64));
    for c in s.chars() {
        let mapped = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            c
        } else if c.is_ascii_uppercase() {
            c.to_ascii_lowercase()
        } else {
            '_'
        };
        out.push(mapped);
        if out.len() >= 64 {
            break;
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), MemoryError> {
    if has_column(conn, table, column)? {
        return Ok(());
    }

    let table_name = table.to_string();
    let column_name = column.to_string();
    let table = quote_sql_identifier(table)?;
    let column = quote_sql_identifier(column)?;
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
    match execute_retry(conn, &sql) {
        Ok(_) => {}
        Err(MemoryError::Sqlite(error))
            if is_duplicate_column_error(&error)
                && has_column(conn, &table_name, &column_name)? => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let table = quote_sql_identifier(table)?;
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn quote_sql_identifier(identifier: &str) -> Result<String, MemoryError> {
    if identifier.is_empty()
        || !identifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(MemoryError::InvalidArg(format!(
            "invalid SQL identifier: {identifier}"
        )));
    }
    Ok(format!("\"{identifier}\""))
}

fn ensure_fts_backfilled(conn: &Connection) -> Result<(), MemoryError> {
    // (The stray `vault_entries.allowed_agents` ensure_column that used to sit
    // here moved to `init_product_schema_columns` in #1585 D3: it is a product
    // table and would `no such table`-crash a PortableKernel init, and it never
    // had anything to do with FTS backfill.)
    let memories_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
    if memories_count == 0 {
        return Ok(());
    }

    let mut projection_changes = conn.execute(
        "DELETE FROM memories_fts WHERE id NOT IN (SELECT id FROM memories)",
        [],
    )?;

    projection_changes += conn.execute(
        r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
           SELECT
             m.id,
             m.path,
             m.summary,
             m.text,
             trim(replace(replace(replace(m.keywords, '[', ' '), ']', ' '), '"', ' ')),
             trim(replace(replace(replace(m.entities, '[', ' '), ']', ' '), '"', ' '))
           FROM memories m
           WHERE NOT EXISTS (SELECT 1 FROM memories_fts f WHERE f.id = m.id)"#,
        [],
    )?;

    // Symbolic trigram index (#1331): insert-missing drift repair only.
    // Content refreshes after path/data migrations are owned by v22's full
    // rebuild (`migrate_v22_memories_symbolic_fts`) so this early backfill
    // cannot permanently freeze pre-migration field values.
    let symbolic_fts_present: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories_symbolic_fts'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if symbolic_fts_present {
        projection_changes += conn.execute(
            "DELETE FROM memories_symbolic_fts WHERE id NOT IN (SELECT id FROM memories)",
            [],
        )?;
        projection_changes += conn.execute(
            r#"INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
               SELECT m.id, m.path, m.summary, m.text, m.keywords, m.entities, m.topic
               FROM memories m
               WHERE NOT EXISTS (SELECT 1 FROM memories_symbolic_fts f WHERE f.id = m.id)"#,
            [],
        )?;
    }

    if projection_changes > 0 {
        crate::db::search_generation::bump_search_generation(conn)?;
    }

    Ok(())
}

// tachi#1585 D5: `crate::kernel_policy::migration_backup_retain_count` — a
// `OnceLock<usize>` defaulting to the historical literal (3) with zero env
// reads, injectable once by an adapter via `set_migration_backup_retain`.
// This call site fires mid schema-init, before any `MemoryStore` exists to
// carry a per-instance `KernelPolicy` on, so unlike the other three D5 sites
// it cannot read `self`; see `kernel_policy.rs` module docs for why this one
// knob stays process-wide instead.
fn migration_backup_retain_count() -> usize {
    crate::kernel_policy::migration_backup_retain_count()
}

fn migration_marker_path(db_path: &Path) -> PathBuf {
    let mut s = db_path.as_os_str().to_owned();
    s.push(".migration-marker");
    PathBuf::from(s)
}

fn migration_schema_fingerprint(conn: &Connection) -> Result<String, MemoryError> {
    let sv: i64 = conn.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
    Ok(format!("{}:{}", env!("CARGO_PKG_VERSION"), sv))
}

fn maybe_backup_before_migration(
    conn: &Connection,
    db_path: &Path,
) -> Result<Option<PathBuf>, MemoryError> {
    let current_fp = migration_schema_fingerprint(conn)?;
    // schema_version == 0: the file was just created and has no schema yet.
    // There is nothing to back up.
    if current_fp.ends_with(":0") {
        return Ok(None);
    }

    // #1180: the 2026-07-17 v18->v19 live deploy migrated `PRAGMA
    // user_version` forward under an authorized `MigrationAuthority::Allow`
    // open (the exact event the `SchemaMigrationOptInRequired` refusal
    // promises `<db>.migration-bak.<ts>` + `<db>.migration-marker` for) but
    // left NEITHER file. Root cause: this function's "skip if unchanged"
    // heuristic below keys off `PRAGMA schema_version` (a SQLite-internal DDL
    // cookie) + the binary's crate version — a proxy for "did the schema
    // shape change since our last successful init", used to avoid redundant
    // backups on ordinary same-version daemon restarts. `PRAGMA user_version
    // = N` does not touch `schema_version`, so a long-lived process whose
    // marker was last written on a PRIOR restart (no DDL has run since) can
    // have a marker that coincidentally still matches `current_fp` at the
    // moment a REAL `stored < EXPECTED_SCHEMA_VERSION` migration begins —
    // silently skipping the backup this function exists to guarantee.
    //
    // The authoritative signal for "is this open crossing the migration
    // threshold" is the version STAMP, not the fingerprint heuristic: this
    // function is only ever reached after `check_db_open_context_gate` has
    // already refused an unauthorized `1 <= stored < EXPECTED` open, so
    // `is_version_migration` here can only be true under
    // `MigrationAuthority::Allow`. When it is true, always back up — the
    // fingerprint heuristic is downgraded to its original purpose (skip
    // redundant backups on a plain same-version reopen) and must never
    // suppress a genuine, authorized migration's trail.
    let stored = crate::db::migrations::read_schema_version(conn)?;
    let is_version_migration =
        (1..crate::db::migrations::EXPECTED_SCHEMA_VERSION).contains(&stored);

    if !is_version_migration {
        let marker = migration_marker_path(db_path);
        if std::fs::read_to_string(&marker).ok().as_deref() == Some(current_fp.as_str()) {
            return Ok(None);
        }
    }

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let backup_path = sibling_with_suffix(db_path, &format!("migration-bak.{ts}"));

    {
        let mut dst = Connection::open(&backup_path)?;
        let backup = rusqlite::backup::Backup::new(conn, &mut dst)?;
        backup.run_to_completion(128, Duration::from_millis(100), None)?;
    }

    retain_recent_migration_backups(db_path);

    Ok(Some(backup_path))
}

fn remember_migration_fingerprint(conn: &Connection, db_path: &Path) -> Result<(), MemoryError> {
    let fp = migration_schema_fingerprint(conn)?;
    if let Err(e) = std::fs::write(migration_marker_path(db_path), &fp) {
        eprintln!(
            "[migration] warning: failed to write migration marker: {e}; \
             next startup will back up again"
        );
    }
    Ok(())
}

fn retain_recent_migration_backups(db_path: &Path) {
    let Some(dir) = db_path.parent() else {
        return;
    };
    let Some(name) = db_path.file_name() else {
        return;
    };
    let prefix = format!("{}.migration-bak.", name.to_string_lossy());

    let mut backups: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .collect(),
        Err(_) => return,
    };

    // Lexicographic descending = newest first (ISO 8601 timestamp in filename).
    backups.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

    for old in backups.into_iter().skip(migration_backup_retain_count()) {
        let _ = std::fs::remove_file(old.path());
    }
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod recall_impression_schema_tests {
    use std::ffi::{c_char, c_int, c_void};

    use super::*;

    unsafe extern "C" fn deny_schema_reads(
        _state: *mut c_void,
        action: c_int,
        _arg1: *const c_char,
        _arg2: *const c_char,
        _database: *const c_char,
        _accessor: *const c_char,
    ) -> c_int {
        if action == rusqlite::ffi::SQLITE_READ {
            rusqlite::ffi::SQLITE_DENY
        } else {
            rusqlite::ffi::SQLITE_OK
        }
    }

    #[test]
    fn recall_attribution_schema_validations_propagate_sqlite_query_errors() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        install_recall_impression_ledger_schema(&conn).expect("install valid ledger schema");
        install_typo_fallback_attribution_schema(&conn)
            .expect("install valid typo attribution schema");
        let install_result = unsafe {
            rusqlite::ffi::sqlite3_set_authorizer(
                conn.handle(),
                Some(deny_schema_reads),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(install_result, rusqlite::ffi::SQLITE_OK);

        let error = validate_recall_impression_ledger_schema(&conn)
            .expect_err("SQLite authorization errors must not become missing-object errors");
        assert!(
            matches!(error, MemoryError::Sqlite(_)),
            "expected propagated SQLite error, got: {error}"
        );

        let error = validate_typo_fallback_attribution_schema(&conn)
            .expect_err("SQLite authorization errors must not become missing-column errors");
        assert!(
            matches!(error, MemoryError::Sqlite(_)),
            "expected propagated SQLite error, got: {error}"
        );
    }
}

#[cfg(test)]
mod idless_identity_tests {
    use super::*;

    #[test]
    fn enum_rebuild_preserves_modern_idless_identity_constraint() {
        let _ = libsimple::enable_auto_extension();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, text, timestamp, idless_identity)
             VALUES ('modern-idless', '/modern', 'identity survives rebuild', '2026-07-16T00:00:00Z', 'identity')",
            [],
        )
        .unwrap();

        rebuild_memories_with_check_constraints(&conn).unwrap();

        let identity: Option<String> = conn
            .query_row(
                "SELECT idless_identity FROM memories WHERE id = 'modern-idless'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(identity.as_deref(), Some("identity"));
        assert!(
            conn.execute(
                "INSERT INTO memories (id, path, text, timestamp, idless_identity)
                 VALUES ('second-modern-idless', '/modern', 'another text', '2026-07-16T00:00:00Z', 'identity')",
                [],
            )
            .is_err(),
            "the active identity constraint must survive the enum rebuild"
        );
    }
}

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
mod migration_backup_tests;
