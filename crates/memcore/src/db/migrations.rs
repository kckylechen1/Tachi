//! One-shot, idempotent data migrations for legacy memory DBs.
//!
//! Each migration is gated by a sentinel row in `hard_state`
//! (namespace='migrations', key=<migration_id>). Once written, the migration
//! is skipped on subsequent runs.
//!
//! See docs/audit-2026-04-30.md (PR-3) for the bugs each migration fixes:
//! - v1: H1 path normalization
//! - v2: H4 scope normalization (defense-in-depth on top of PR-1's enum migration)
//! - v3: H6 handoff path standardization
//! - v4: B4/B11 cross-DB pollution quarantine
//! - v5: drop HyperTachi legacy columns (`indexed_tags`, `domain_key`) after bridge
//! - v6: fold non-empty `persons` JSON into `entities`, then clear `persons`
//! - v7: reconcile half-migrated DBs (re-bridge + drop `indexed_tags`/`domain_key`, ensure `location`)
//! - v8: drop the legacy physical `persons` column after folding it into `entities`
//! - v9: relocate non-empty `location` into `path` / metadata, then drop `location`
//! - v10: drop retired skill-pack tables (`packs`, `agent_projections`)
//! - v11: drop retired `domains` registry table (#757)
//! - v12: `session_claims` identity-triple UNIQUE index (#1001 round 2)
//! - v13: add `idx_hard_state_ns_updated` on `hard_state(namespace, updated_at
//!   DESC)` — collapses `list_state`'s full temp-B-tree sort (perf pack;
//!   numbered after #1007's v12, see #1017)
//! - v14: `dispatch_outcomes.reported_outcome` column (#773 Layer-2 ②) —
//!   dual-truth: raw self-report vs machine-resolved verdict.
//! - v15: `exec_envs.env_class` column (#894 S2c) — provisioning policy class
//!   (edit-only | build-ticketed | build-private); legacy rows land on the
//!   fail-safe `edit-only`.
//! - v16: `dispatch_outcomes.identity_receipt` column (#1065) — frozen
//!   requested/effective identity copied from the dispatch receipt.
//! - v17: `dispatch_outcomes.identity_attribution_basis` column (#1065 D) —
//!   frozen evidence class of the flat identity columns, back-filled from
//!   each row's receipt.
//! - v18: `dispatch_adjudications` + `dispatch_adjudication_signatures`
//!   tables (#1035) — append-only leader terminal-judgment events linked
//!   to a dispatch outcome by `outcome_id`.
//! - v19: `memories.idless_identity` column + unique partial index (#1115) —
//!   reserves a DB-level identity for new id-less saves only.
//! - v20: `mirror_eval_runs` + `mirror_eval_observations` +
//!   `mirror_eval_adjudications` tables (#1066) — first-class mirror eval
//!   intake for harness-native subagents (register/observe/adjudicate/get).
//! - v21: AgentIdentity / WorkClaim holder-evidence spine (#1253).
//! - v22: `memories_symbolic_fts` trigram projection create + full rebuild
//!   (#1331) — versioned so stamped-v21 DBs cannot silently acquire the
//!   table via idempotent init DDL without a migration stamp/authority gate.
//! - v23: canonical reserved evidence-reference guard triggers — versioned so
//!   a legitimate v22 DB upgrades under migration authority while a damaged
//!   v23 inventory is refused rather than silently repaired.
//! - v24: `memories.scored_count` scorer-only diagnostic counter (#1459).
//! - v25: sampled recall-impression ledger tables and indexes (#1447).
//! - v26: SHA-256 query fingerprints and complete replay-policy identity for
//!   recall impressions; v25 groups remain honestly unversioned (#1447).
//!
//! ## Schema version stamp (#984)
//!
//! In addition to the sentinel-row idempotency above, the DB carries an
//! integer version stamp in `PRAGMA user_version` (SQLite's canonical slot
//! for this; unused anywhere else in this codebase prior to #984). This is a
//! *coarse* hard-fail gate, orthogonal to the fine-grained sentinel
//! migrations: it exists so a downstream reader (e.g. HyperMem) opening a DB
//! written by a newer kernel fails loudly instead of silently proceeding
//! against data/columns it doesn't understand yet.
//!
//! [`EXPECTED_SCHEMA_VERSION`] counts the sentinel migration sequence above.
//! Bump this const (and add a `vN` doc line above) whenever a new migration is
//! appended to [`run_data_migrations`].
//!
//! ### Compatibility transaction widened to cover `init_schema_inner` (#984 F1 round 3)
//!
//! The `BEGIN IMMEDIATE`/stamp boundary described above was originally scoped
//! to just this module's sentinel migrations. That left a hole:
//! `schema::init_schema_with_label_mut` called `schema::init_schema_inner`
//! (DDL + `bridge_hypertachi_memory_columns` + the standalone v6/v8/v9
//! legacy-column work in `legacy_columns.rs`) BEFORE opening this module's
//! transaction — so a crash between that legacy work committing and this
//! module's stamp being written left newer schema/data on disk under an old
//! or zero `user_version`, exactly what the gate exists to prevent. Round 3
//! fixes this by having `init_schema_with_label_mut` open ONE outer
//! transaction that covers `init_schema_inner` AND
//! [`run_data_migrations_in_tx`] AND the final stamp — see that function's
//! doc comment in `schema.rs`.

use std::path::Path;

use rusqlite::Connection;

use crate::error::MemoryError;

use super::common::now_utc_iso;

/// Current schema version stamp, persisted via `PRAGMA user_version`.
///
/// See the module doc comment ("Schema version stamp (#984)") for what this
/// counts and when to bump it.
pub const EXPECTED_SCHEMA_VERSION: u32 = 26;

mod basic;
mod cross_db;
mod dispatch_adjudications;
mod dispatch_outcomes_attribution_basis;
mod dispatch_outcomes_identity_receipt;
mod dispatch_outcomes_reported;
mod domain_retire;
mod exec_env_class;
mod hard_state_index;
mod identity_workclaim_spine;
mod idless_identity;
mod legacy_columns;
mod mirror_eval;
mod pack_retire;
mod sentinel;
mod session_claims_identity;
mod symbolic_fts;

use basic::*;
use cross_db::*;
use dispatch_adjudications::*;
use dispatch_outcomes_attribution_basis::*;
use dispatch_outcomes_identity_receipt::*;
use dispatch_outcomes_reported::*;
use domain_retire::*;
use exec_env_class::*;
use hard_state_index::*;
use identity_workclaim_spine::*;
use idless_identity::*;
use legacy_columns::*;
pub use legacy_columns::{
    fold_and_drop_legacy_persons_column, migrate_v9_relocate_and_drop_location,
};
use mirror_eval::*;
use pack_retire::*;
use sentinel::*;
pub(in crate::db) use session_claims_identity::dedupe_session_claims_identity_conflicts;
use session_claims_identity::*;
pub use symbolic_fts::rebuild_memories_symbolic_fts;
use symbolic_fts::*;

const MIGRATION_NS: &str = "migrations";
const SANITY_QUARANTINE_FRACTION: f64 = 0.5;

/// Canonical sentinel inventory for a database stamped at
/// [`EXPECTED_SCHEMA_VERSION`]. A current stamp is a claim that every
/// migration completed; an absent sentinel is corruption, never permission to
/// rerun migration work during an ordinary same-version open.
pub(crate) const MIGRATION_SENTINEL_KEYS: &[&str] = &[
    "v1_path_normalize_legacy",
    "v2_scope_self_normalize",
    "v3_handoff_path_standardize",
    "v4_quarantine_cross_db_rows",
    "v5_drop_hypertachi_legacy_columns",
    "v6_fold_persons_into_entities",
    "v7_reconcile_legacy_memory_columns",
    "v8_drop_legacy_persons_column",
    "v9_relocate_and_drop_location",
    "v10_drop_pack_tables",
    "v11_drop_domains_table",
    "v12_session_claims_unique_identity",
    "v13_hard_state_ns_updated_index",
    "v14_dispatch_outcomes_reported_outcome",
    "v15_exec_envs_env_class",
    "v16_dispatch_outcomes_identity_receipt",
    "v17_dispatch_outcomes_attribution_basis",
    "v18_dispatch_adjudications",
    "v19_idless_memory_identity",
    "v20_mirror_eval",
    "v21_identity_workclaim_spine",
    "v22_memories_symbolic_fts",
    "v23_reserved_reference_guards",
    "v24_memories_scored_count",
    "v25_recall_impression_ledger",
    "v26_recall_impression_replay_identity",
];

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct MigrationReport {
    pub paths_normalized: usize,
    pub scopes_fixed: usize,
    pub handoff_paths_standardized: usize,
    pub quarantined: usize,
    pub quarantine_skipped_sanity_guard: bool,
    pub hypertachi_legacy_columns_dropped: usize,
    pub persons_folded_into_entities: usize,
    pub legacy_columns_reconciled: usize,
    pub persons_columns_dropped: usize,
    pub locations_relocated: usize,
    pub location_columns_dropped: usize,
    pub pack_tables_dropped: usize,
    pub domains_table_dropped: usize,
    pub session_claims_duplicates_deduped: usize,
    pub hard_state_index_added: usize,
    pub dispatch_outcomes_reported_outcome_added: usize,
    pub exec_envs_env_class_added: usize,
    pub dispatch_outcomes_identity_receipt_added: usize,
    pub dispatch_outcomes_attribution_basis_backfilled: usize,
    pub dispatch_adjudications_created: usize,
    pub idless_identity_constraint_added: usize,
    pub mirror_eval_tables_created: usize,
    pub identity_workclaim_columns_added: usize,
    pub memories_symbolic_fts_rows: usize,
    pub reserved_reference_guards_installed: usize,
    pub scored_count_column_added: usize,
    pub recall_impression_schema_objects_created: usize,
    pub recall_impression_replay_identity_columns_added: usize,
}

#[cfg(test)]
pub(crate) mod test_hooks {
    use crate::error::MemoryError;
    use std::cell::Cell;

    thread_local! {
        static FAIL_AFTER_V23_GUARD_INSTALL: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn arm_fail_after_v23_guard_install() {
        FAIL_AFTER_V23_GUARD_INSTALL.with(|flag| flag.set(true));
    }

    pub(super) fn fail_after_v23_guard_install() -> Result<(), MemoryError> {
        let armed = FAIL_AFTER_V23_GUARD_INSTALL.with(|flag| flag.replace(false));
        if armed {
            return Err(MemoryError::InvalidArg(
                "test_hooks: injected failure after v23 guard install".to_string(),
            ));
        }
        Ok(())
    }
}

/// Read the schema version stamp (`PRAGMA user_version`). Absent/fresh DBs
/// read back `0`.
pub fn read_schema_version(conn: &Connection) -> Result<u32, MemoryError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(version.max(0) as u32)
}

/// Inspect a database's version without creating or mutating it. Runtime
/// write routers use this only to recognize the one narrowly authorized
/// v22-to-v23 evidence-guard upgrade before opening a named project store.
pub fn read_schema_version_at_path(db_path: &Path) -> Result<u32, MemoryError> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    read_schema_version(&conn)
}

/// Persist `version` as the schema version stamp (`PRAGMA user_version`).
///
/// `PRAGMA` statements don't accept bound parameters, so the value is
/// interpolated directly; it is always a `u32` we control (never
/// attacker-controlled input), so this is not a SQL-injection surface.
fn write_schema_version(conn: &Connection, version: u32) -> Result<(), MemoryError> {
    conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    Ok(())
}

/// Stamp the current [`EXPECTED_SCHEMA_VERSION`]. Exposed to `schema.rs` so
/// `init_schema_with_label_mut` can write the final stamp itself, inside the
/// SAME outer transaction it opened for `init_schema_inner`'s legacy work —
/// see that function's doc comment (#984 F1 round 3) for why the stamp can no
/// longer be delegated to the public [`run_data_migrations`] wrapper (that
/// wrapper opens its OWN transaction, which would either double-BEGIN or
/// leave `init_schema_inner`'s earlier legacy mutations outside the window
/// this stamp is meant to cover).
pub(crate) fn write_schema_version_stamp(conn: &Connection) -> Result<(), MemoryError> {
    write_schema_version(conn, EXPECTED_SCHEMA_VERSION)
}

/// Hard-fail gate: refuse to open/operate on a DB stamped with a schema
/// version newer than this kernel supports. Called at the top of
/// [`run_data_migrations`] (i.e. from `init_schema_with_label_mut`'s entry
/// path), before any migration touches the DB.
///
/// - stamped version > `EXPECTED_SCHEMA_VERSION` → hard error, never proceed.
/// - stamped version <= `EXPECTED_SCHEMA_VERSION` (including the `0` fresh/
///   absent case) → caller proceeds to run migrations and re-stamp.
pub fn check_schema_version_gate(conn: &Connection) -> Result<(), MemoryError> {
    let stored = read_schema_version(conn)?;
    if stored > EXPECTED_SCHEMA_VERSION {
        return Err(MemoryError::InvalidArg(format!(
            "db schema version {stored} newer than supported {EXPECTED_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

/// Fail closed when a database claims the current schema version but lacks
/// evidence or persistent objects required by that claim. This is a read-only
/// preflight: callers run it before backup, connection PRAGMAs, transactions,
/// idempotent DDL, migration execution, or version stamping.
pub(crate) fn validate_current_schema_integrity(conn: &Connection) -> Result<(), MemoryError> {
    if read_schema_version(conn)? != EXPECTED_SCHEMA_VERSION {
        return Ok(());
    }

    for key in MIGRATION_SENTINEL_KEYS {
        if !was_run(conn, key)? {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete current schema v{EXPECTED_SCHEMA_VERSION}: required migration sentinel '{key}' is missing"
            )));
        }
    }
    crate::db::schema::validate_recall_impression_ledger_schema(conn)
}

/// #1119 typed schema-migration gate. Runs at the DB-open funnel
/// (`schema::init_schema_with_label_mut`), AFTER [`check_schema_version_gate`]
/// (which owns the `stored > EXPECTED` refusal and is called first by every
/// entry point) and BEFORE any DDL / data migration / version stamp mutates
/// the DB. Decides purely from the caller's typed [`DbOpenContext`] and the
/// stored `PRAGMA user_version` — **never from DB content, never from process
/// env**. This is the single production choke point that closes the #1119
/// incident: an unauthorized process can no longer forward-migrate a live DB
/// a deployed daemon still depends on.
///
/// Decision table (`stored > EXPECTED` already refused upstream):
///
/// | intent        | stored == 0 | 1 ≤ stored < EXPECTED       | stored == EXPECTED |
/// |---------------|-------------|-----------------------------|--------------------|
/// | `CreateFresh` | build (Ok)  | refuse: create-on-existing  | refuse: create-on-existing |
/// | `OpenExisting`| build (Ok)  | Deny→refuse / Allow→migrate  | Ok (current)       |
///
/// `CreateFresh` succeeds ONLY on an unstamped (`stored == 0`) file — even one
/// whose tables `init_schema` already built (owner ruling A: that product IS
/// fresh; the discriminator is the version stamp, never DB content). Any
/// *stamped* DB (`stored >= 1`, older or current) is a pre-existing
/// operational DB, not a create target, and is refused with
/// [`MemoryError::DbCreateTargetExists`].
///
/// `stored == 0` is a *build*, not a *migration*, under both intents: the
/// #1119 incident was a **stamped** older DB (17 → 18); an unstamped
/// (`user_version == 0`) file has no deployed daemon depending on a prior
/// stamp, so building schema onto it strands nobody. Crucially, this decision
/// reads only the version stamp — it does NOT inspect `sqlite_master` to guess
/// "fresh vs legacy" (the reverted wrong-layer discriminator: `init_schema`
/// produces a full-table DB that still reads `user_version == 0`, so content
/// cannot distinguish the two). The `1 ≤ stored < EXPECTED` band is the only
/// place authority matters, and it is exactly the accident's shape.
pub fn check_db_open_context_gate(
    conn: &Connection,
    db_path: &Path,
    ctx: &crate::db::DbOpenContext,
) -> Result<(), MemoryError> {
    use crate::db::{MigrationAuthority, OpenIntent};

    let stored = read_schema_version(conn)?;
    match ctx.intent {
        OpenIntent::CreateFresh => {
            // Provisioning succeeds ONLY on an unstamped file (build fresh, no
            // authority). Any stamped DB (older OR current) is a pre-existing
            // operational DB, not a create target — refuse. Use OpenExisting
            // (with authority) to open/migrate an existing DB.
            if stored == 0 {
                Ok(())
            } else {
                Err(MemoryError::DbCreateTargetExists {
                    stored,
                    db_path: db_path.display().to_string(),
                })
            }
        }
        OpenIntent::OpenExisting => {
            if stored >= EXPECTED_SCHEMA_VERSION {
                // == EXPECTED (current); > EXPECTED refused upstream.
                return Ok(());
            }
            if stored == 0 {
                // No stamp: a build, not a migration — see the doc comment.
                return Ok(());
            }
            // 1 ≤ stored < EXPECTED: a real older DB. THE migration decision.
            match &ctx.migration {
                MigrationAuthority::Allow { approved_by } => {
                    eprintln!(
                        "{}",
                        schema_migration_success_log_line(stored, db_path, approved_by)
                    );
                    Ok(())
                }
                MigrationAuthority::Deny => {
                    Err(schema_migration_opt_in_required_error(stored, db_path))
                }
            }
        }
    }
}

/// The audit log line the #1119 incident report asked for when an authorized
/// migration proceeds: who authorized it (`approved_by`) plus this binary's
/// own version + pid (so an operator grepping logs after the fact can tell
/// which process performed the migration), from/to version, and the DB path.
/// Factored out of the `eprintln!` call site so it is directly unit-testable
/// without capturing real stderr.
fn schema_migration_success_log_line(stored: u32, db_path: &Path, approved_by: &str) -> String {
    format!(
        "[migration] authorized by {approved_by}: binary={} pid={} migrating db={} schema {stored} -> {EXPECTED_SCHEMA_VERSION}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        db_path.display()
    )
}

/// Build the typed [`MemoryError::SchemaMigrationOptInRequired`] refusal. The
/// backup/marker hints name THIS db's actual sibling paths (not a `<db>`
/// template) — the marker path is exact (deterministic naming); the backup
/// path's timestamp suffix is the one component genuinely unknown until a real
/// migration attempt runs (see `schema::maybe_backup_before_migration`).
fn schema_migration_opt_in_required_error(stored: u32, db_path: &Path) -> MemoryError {
    let db_path_str = db_path.display().to_string();
    MemoryError::SchemaMigrationOptInRequired {
        stored,
        expected: EXPECTED_SCHEMA_VERSION,
        db_path: db_path_str.clone(),
        backup_hint: format!("{db_path_str}.migration-bak.<UTC-timestamp-of-this-attempt>"),
        marker_hint: format!("{db_path_str}.migration-marker"),
    }
}

/// Run all data-fix migrations in order. Idempotent.
///
/// `db_label` is the manifest role/project label for this DB ("global",
/// "wiki", a project name, or "unknown"). `current_db_path` is the canonical
/// filesystem path to this DB file (used by v4 to detect rows whose
/// `metadata.provenance.db_path` points elsewhere).
///
/// Enforces the [`EXPECTED_SCHEMA_VERSION`] hard-fail gate on entry (a stored
/// version newer than this kernel supports errors out before any migration
/// runs) and stamps the current version on successful exit — fresh DBs
/// (version 0/absent), DBs at an older version, and DBs already at the
/// current version all end this call stamped at `EXPECTED_SCHEMA_VERSION`.
///
/// This function is `pub` (downstream callers may invoke it directly, outside
/// `init_schema_with_label_mut`), so it is unconditionally transactional on
/// its own: it opens its own `BEGIN IMMEDIATE` here and commits only after
/// every migration and the version stamp have succeeded, exactly as it did
/// before #984 F1 round 3. What changed in round 3 is the PUBLIC ENTRY POINT
/// (`init_schema_with_label_mut`): that function no longer calls this
/// function. It calls [`run_data_migrations_in_tx`] directly against its OWN
/// outer transaction — one that also covers `init_schema_inner`'s legacy
/// v6/v8/v9 work, which used to commit standalone before this transaction
/// even opened (the original compatibility hole this round fixes). Called
/// standalone (this function), the boundary here still only covers the
/// sentinel migrations, not `init_schema_inner`'s DDL/legacy work — exactly
/// as before; the difference is that `init_schema_with_label_mut` no longer
/// takes this path.
///
/// ## Transactional compatibility boundary (#984 F1)
///
/// The migration effects (sentinel writes and the final `user_version` stamp
/// included) all run inside a single `BEGIN IMMEDIATE` transaction opened
/// here and committed only after every migration and the version stamp have
/// succeeded. A crash or error partway through rolls the *entire* call back
/// — there is no window where the DB is left partially migrated but still
/// carrying an old/zero version stamp (which would let an older kernel pass
/// [`check_schema_version_gate`] against data it doesn't understand).
/// `PRAGMA user_version` is a page in the database header and is journaled
/// like any other write, so it participates in the same rollback as the
/// schema/data changes (see `stamp_and_migration_effects_roll_back_together`
/// in the test module for a fault-injection proof).
pub fn run_data_migrations(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<MigrationReport, MemoryError> {
    check_schema_version_gate(conn)?;
    validate_current_schema_integrity(conn)?;

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let report = run_data_migrations_in_tx(&tx, db_label, current_db_path)?;
    write_schema_version_stamp(&tx)?;
    tx.commit()?;

    Ok(report)
}

/// The actual migration sequence, run against an already-open transaction.
/// Split out from [`run_data_migrations`] so tests can inject a failure
/// between individual migration steps and the final stamp while still
/// exercising the real transaction boundary.
///
/// Also called directly by `schema.rs`'s `init_schema_with_label_mut` (#984
/// F1 round 3), which opens its OWN outer `BEGIN IMMEDIATE` covering
/// `init_schema_inner`'s legacy work as well, and drives this + the final
/// stamp against that same transaction — see that function's doc comment.
pub(crate) fn run_data_migrations_in_tx(
    conn: &Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<MigrationReport, MemoryError> {
    let mut report = MigrationReport::default();

    if !was_run(conn, "v1_path_normalize_legacy")? {
        report.paths_normalized = migrate_v1_path_normalize(conn)?;
        mark_run(conn, "v1_path_normalize_legacy")?;
    }

    if !was_run(conn, "v2_scope_self_normalize")? {
        report.scopes_fixed = migrate_v2_scope_normalize(conn)?;
        mark_run(conn, "v2_scope_self_normalize")?;
    }

    if !was_run(conn, "v3_handoff_path_standardize")? {
        report.handoff_paths_standardized = migrate_v3_handoff_standardize(conn)?;
        mark_run(conn, "v3_handoff_path_standardize")?;
    }

    if !was_run(conn, "v4_quarantine_cross_db_rows")? {
        let (quarantined, skipped) =
            migrate_v4_quarantine_cross_db(conn, db_label, current_db_path)?;
        report.quarantined = quarantined;
        report.quarantine_skipped_sanity_guard = skipped;
        // Even on sanity-guard skip, mark run so we don't loop on every startup.
        mark_run(conn, "v4_quarantine_cross_db_rows")?;
    }

    if !was_run(conn, "v5_drop_hypertachi_legacy_columns")? {
        report.hypertachi_legacy_columns_dropped = migrate_v5_drop_hypertachi_legacy_columns(conn)?;
        mark_run(conn, "v5_drop_hypertachi_legacy_columns")?;
    }

    if !was_run(conn, "v6_fold_persons_into_entities")? {
        report.persons_folded_into_entities = migrate_v6_fold_persons_into_entities(conn)?;
        mark_run(conn, "v6_fold_persons_into_entities")?;
    }

    if !was_run(conn, "v7_reconcile_legacy_memory_columns")? {
        report.legacy_columns_reconciled = migrate_v7_reconcile_legacy_memory_columns(conn)?;
        mark_run(conn, "v7_reconcile_legacy_memory_columns")?;
    }

    if !was_run(conn, "v8_drop_legacy_persons_column")? {
        report.persons_columns_dropped = fold_and_drop_legacy_persons_column(conn)?;
        mark_run(conn, "v8_drop_legacy_persons_column")?;
    }

    if !was_run(conn, "v9_relocate_and_drop_location")? {
        let (relocated, dropped) = migrate_v9_relocate_and_drop_location(conn)?;
        report.locations_relocated = relocated;
        report.location_columns_dropped = dropped;
        mark_run(conn, "v9_relocate_and_drop_location")?;
    }

    report.pack_tables_dropped =
        apply_versioned_migration(conn, "v10_drop_pack_tables", migrate_v10_drop_pack_tables)?
            .unwrap_or(0);

    report.domains_table_dropped = apply_versioned_migration(
        conn,
        "v11_drop_domains_table",
        migrate_v11_drop_domains_table,
    )?
    .unwrap_or(0);

    report.session_claims_duplicates_deduped = apply_versioned_migration(
        conn,
        "v12_session_claims_unique_identity",
        migrate_v12_session_claims_unique_identity,
    )?
    .unwrap_or(0);

    report.hard_state_index_added = apply_versioned_migration(
        conn,
        "v13_hard_state_ns_updated_index",
        migrate_v13_add_hard_state_index,
    )?
    .unwrap_or(0);

    report.dispatch_outcomes_reported_outcome_added = apply_versioned_migration(
        conn,
        "v14_dispatch_outcomes_reported_outcome",
        migrate_v14_dispatch_outcomes_reported_outcome,
    )?
    .unwrap_or(0);

    report.exec_envs_env_class_added = apply_versioned_migration(
        conn,
        "v15_exec_envs_env_class",
        migrate_v15_exec_envs_env_class,
    )?
    .unwrap_or(0);

    report.dispatch_outcomes_identity_receipt_added = apply_versioned_migration(
        conn,
        "v16_dispatch_outcomes_identity_receipt",
        migrate_v16_dispatch_outcomes_identity_receipt,
    )?
    .unwrap_or(0);

    report.dispatch_outcomes_attribution_basis_backfilled = apply_versioned_migration(
        conn,
        "v17_dispatch_outcomes_attribution_basis",
        migrate_v17_dispatch_outcomes_attribution_basis,
    )?
    .unwrap_or(0);

    report.dispatch_adjudications_created = apply_versioned_migration(
        conn,
        "v18_dispatch_adjudications",
        migrate_v18_dispatch_adjudications,
    )?
    .unwrap_or(0);

    report.idless_identity_constraint_added = apply_versioned_migration(
        conn,
        "v19_idless_memory_identity",
        migrate_v19_add_idless_memory_identity,
    )?
    .unwrap_or(0);

    report.mirror_eval_tables_created =
        apply_versioned_migration(conn, "v20_mirror_eval", migrate_v20_mirror_eval)?.unwrap_or(0);
    report.identity_workclaim_columns_added = apply_versioned_migration(
        conn,
        "v21_identity_workclaim_spine",
        migrate_v21_identity_workclaim_spine,
    )?
    .unwrap_or(0);

    // After every earlier migration that can rewrite indexed memory fields
    // (path/summary/text/keywords/…), rebuild the symbolic trigram projection
    // from live `memories` so upgrade cannot leave permanently stale rows
    // (#1331 BUG 4). Fresh DBs still get the table from BASE_SCHEMA_SQL; this
    // sentinel is the authority/version story for stamped-v21 → v22.
    report.memories_symbolic_fts_rows = apply_versioned_migration(
        conn,
        "v22_memories_symbolic_fts",
        migrate_v22_memories_symbolic_fts,
    )?
    .unwrap_or(0);

    report.reserved_reference_guards_installed = apply_versioned_migration(
        conn,
        "v23_reserved_reference_guards",
        migrate_v23_reserved_reference_guards,
    )?
    .unwrap_or(0);
    report.scored_count_column_added = apply_versioned_migration(
        conn,
        "v24_memories_scored_count",
        migrate_v24_memories_scored_count,
    )?
    .unwrap_or(0);
    report.recall_impression_schema_objects_created = apply_versioned_migration(
        conn,
        "v25_recall_impression_ledger",
        migrate_v25_recall_impression_ledger,
    )?
    .unwrap_or(0);
    report.recall_impression_replay_identity_columns_added = apply_versioned_migration(
        conn,
        "v26_recall_impression_replay_identity",
        migrate_v26_recall_impression_replay_identity,
    )?
    .unwrap_or(0);

    Ok(report)
}

fn migrate_v23_reserved_reference_guards(conn: &Connection) -> Result<usize, MemoryError> {
    crate::db::schema::install_reserved_reference_guard(conn)?;
    #[cfg(test)]
    test_hooks::fail_after_v23_guard_install()?;
    crate::db::validate_persistent_trigger_inventory(conn, true)?;
    Ok(2)
}

fn migrate_v24_memories_scored_count(conn: &Connection) -> Result<usize, MemoryError> {
    crate::db::schema::ensure_memories_scored_count(conn)?;
    Ok(1)
}

fn migrate_v25_recall_impression_ledger(conn: &Connection) -> Result<usize, MemoryError> {
    crate::db::schema::install_v25_recall_impression_ledger_schema(conn)?;
    Ok(6)
}

fn migrate_v26_recall_impression_replay_identity(conn: &Connection) -> Result<usize, MemoryError> {
    crate::db::schema::migrate_recall_impression_ledger_to_v26(conn)?;
    Ok(7)
}

/// Run a single sentinel-gated migration: skip if `key`'s sentinel is
/// already set, otherwise run `migrate`, and — only on success — mark the
/// sentinel run. Returns `Ok(None)` when skipped (already run), `Ok(Some(_))`
/// with the migration's result when it actually ran.
///
/// This is the REAL caller-gating code path (#978): a test driving a
/// failing `migrate` closure through this helper exercises the same
/// "propagate the error, do not mark the sentinel" logic that
/// `run_data_migrations` relies on for every versioned migration, rather
/// than a parallel reimplementation of the gate.
fn apply_versioned_migration<T>(
    conn: &Connection,
    key: &str,
    migrate: impl FnOnce(&Connection) -> Result<T, MemoryError>,
) -> Result<Option<T>, MemoryError> {
    if was_run(conn, key)? {
        return Ok(None);
    }
    let result = migrate(conn)?;
    mark_run(conn, key)?;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_schema, register_sqlite_vec, try_load_sqlite_vec};
    use rusqlite::{params, Connection};
    use serde_json::json;

    fn open_test_db() -> (Connection, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let _ = crate::db::enable_simple_auto_extension();
        register_sqlite_vec();
        let conn = Connection::open(tmp.path()).expect("open");
        let _ = try_load_sqlite_vec(&conn);
        crate::db::schema::init_unversioned_schema_for_migration_tests(&conn)
            .expect("init unversioned migration fixture");
        (conn, tmp)
    }

    #[test]
    fn migration_table_has_column_rejects_dynamic_sql_identifiers() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute("CREATE TABLE memories (id TEXT PRIMARY KEY)", [])
            .expect("create minimal table");

        let err = table_has_column(&conn, "memories'); DROP TABLE memories; --", "id")
            .expect_err("dynamic table identifier should be rejected");

        assert!(err.to_string().contains("invalid SQL identifier"));
    }

    fn insert_row(conn: &Connection, id: &str, path: &str, scope: &str, metadata: &str) {
        if table_has_column(conn, "memories", "location").unwrap() {
            conn.execute(
                "INSERT INTO memories
              (id, path, summary, text, importance, timestamp, category, topic,
               keywords, entities, location, source, scope, archived,
               created_at, updated_at, access_count, last_access, revision,
               metadata, retention_policy, domain)
             VALUES (?1, ?2, '', '', 0.5, '2026-01-01T00:00:00Z', 'fact', '',
                     '[]', '[]', '', 'manual', ?3, 0,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, NULL, 1,
                     ?4, NULL, NULL)",
                params![id, path, scope, metadata],
            )
            .unwrap();
        } else {
            conn.execute(
                "INSERT INTO memories
              (id, path, summary, text, importance, timestamp, category, topic,
               keywords, entities, source, scope, archived,
               created_at, updated_at, access_count, last_access, revision,
               metadata, retention_policy, domain)
             VALUES (?1, ?2, '', '', 0.5, '2026-01-01T00:00:00Z', 'fact', '',
                     '[]', '[]', 'manual', ?3, 0,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, NULL, 1,
                     ?4, NULL, NULL)",
                params![id, path, scope, metadata],
            )
            .unwrap();
        }
    }

    #[test]
    fn was_run_mark_run_roundtrip() {
        let (conn, _tmp) = open_test_db();
        assert!(!was_run(&conn, "v1_test").unwrap());
        mark_run(&conn, "v1_test").unwrap();
        assert!(was_run(&conn, "v1_test").unwrap());
        // Idempotent re-mark.
        mark_run(&conn, "v1_test").unwrap();
        assert!(was_run(&conn, "v1_test").unwrap());
    }

    #[test]
    fn v1_normalizes_legacy_paths() {
        let (mut conn, tmp) = open_test_db();
        insert_row(&conn, "a", "/Wiki//foo//", "general", "{}");
        insert_row(&conn, "b", "wiki/Bar", "general", "{}");
        insert_row(&conn, "c", "/handoff/agent", "general", "{}");

        let report = run_data_migrations(&mut conn, "wiki", tmp.path()).unwrap();
        assert_eq!(report.paths_normalized, 2);

        let pa: String = conn
            .query_row("SELECT path FROM memories WHERE id='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pa, "/wiki/foo");
        let pb: String = conn
            .query_row("SELECT path FROM memories WHERE id='b'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pb, "/wiki/Bar");

        // Idempotent on second run.
        let report2 = run_data_migrations(&mut conn, "wiki", tmp.path()).unwrap();
        assert_eq!(report2.paths_normalized, 0);
    }

    #[test]
    fn v3_standardizes_bare_handoff() {
        let (mut conn, tmp) = open_test_db();
        // PR-1 migration runs on init_schema; it normalizes scope. We insert
        // POST-migration so we satisfy CHECK constraints.
        insert_row(&conn, "h1", "/handoff", "general", "{}");
        insert_row(&conn, "h2", "/handoff/agent-x", "general", "{}");
        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.handoff_paths_standardized, 1);
        let p1: String = conn
            .query_row("SELECT path FROM memories WHERE id='h1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(p1, "/handoff/unknown");
    }

    #[test]
    fn v4_quarantines_cross_db_rows() {
        let (mut conn, tmp) = open_test_db();
        // Row whose provenance.db_path points elsewhere → quarantined.
        let other_meta = serde_json::json!({
            "provenance": { "db_path": "/some/other/db.sqlite" }
        })
        .to_string();
        insert_row(&conn, "x", "/hapi/foo", "general", &other_meta);
        // Row with provenance pointing at this DB → kept.
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        let self_meta = serde_json::json!({
            "provenance": { "db_path": canonical.display().to_string() }
        })
        .to_string();
        insert_row(&conn, "y", "/hapi/bar", "general", &self_meta);
        // Pad with extra clean rows so the bad row is <50% of total.
        for i in 0..5 {
            insert_row(&conn, &format!("z{i}"), "/hapi/clean", "general", "{}");
        }

        let report = run_data_migrations(&mut conn, "hapi", tmp.path()).unwrap();
        assert_eq!(report.quarantined, 1);
        assert!(!report.quarantine_skipped_sanity_guard);

        let px: String = conn
            .query_row("SELECT path FROM memories WHERE id='x'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(px, "/_quarantine/cross-db/hapi/foo");
        let qmeta: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='x'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&qmeta).unwrap();
        assert_eq!(v["quarantine"]["reason"], "cross_db_pollution");
        assert_eq!(v["quarantine"]["original_path"], "/hapi/foo");
    }

    #[test]
    fn v4_sanity_guard_aborts_when_majority_would_move() {
        let (mut conn, tmp) = open_test_db();
        let bad_meta = serde_json::json!({
            "provenance": { "db_path": "/elsewhere.db" }
        })
        .to_string();
        // 3 polluted rows, 1 clean → would move 75% → guard aborts.
        for i in 0..3 {
            insert_row(&conn, &format!("p{i}"), "/proj/foo", "general", &bad_meta);
        }
        insert_row(&conn, "ok", "/proj/clean", "general", "{}");
        let report = run_data_migrations(&mut conn, "proj", tmp.path()).unwrap();
        assert_eq!(report.quarantined, 0);
        assert!(report.quarantine_skipped_sanity_guard);
        // Migration is still marked run so we don't retry every startup.
        assert!(was_run(&conn, "v4_quarantine_cross_db_rows").unwrap());
    }

    #[test]
    fn v12_dedupes_pre_v21_legacy_db_via_standalone_run_data_migrations() {
        // #1289 Claim5, end-to-end through the PUBLIC entry point: the
        // standalone `run_data_migrations` path (NOT `init_schema_inner`, which
        // pre-ensures `mode`) runs v12 BEFORE v21 adds `session_claims.mode`.
        // On a legacy DB carrying duplicate active claims, v12's dedup must not
        // reference `mode` or it crashes with `no such column: mode`, rolling
        // the whole migration back. We reconstruct that on-disk state by
        // downgrading a freshly-inited DB to its pre-v21 shape — drop the
        // mode-scoped identity index, drop the `mode` column, and clear the
        // v12/v21 sentinels so both replay — then re-drive the public entry.
        let (mut conn, tmp) = open_test_db();

        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_session_claims_identity_active;
             ALTER TABLE session_claims DROP COLUMN mode;",
        )
        .unwrap();
        conn.execute(
            "DELETE FROM hard_state WHERE namespace = 'migrations' \
             AND key IN ('v12_session_claims_unique_identity', \
                         'v21_identity_workclaim_spine')",
            [],
        )
        .unwrap();
        assert!(
            !table_has_column(&conn, "session_claims", "mode").unwrap(),
            "fixture precondition: the mode column must be absent (pre-v21 shape)"
        );

        // Two duplicate active claims for one identity; the index is gone so the
        // mode-less inserts are not blocked.
        for (id, hb) in [
            ("old-dup", "2026-07-11T00:00:00Z"),
            ("new-dup", "2026-07-11T00:10:00Z"),
        ] {
            conn.execute(
                "INSERT INTO session_claims
                 (claim_id, session_client, issue_ref, flow_id, branch, state, created_at, heartbeat_at)
                 VALUES (?1, 'claude-code', 'org/repo#1289', 'flow-1', 'feat/x', 'active', ?2, ?2)",
                params![id, hb],
            )
            .unwrap();
        }

        let report = run_data_migrations(&mut conn, "global", tmp.path()).expect(
            "standalone run_data_migrations must not crash on a pre-v21 legacy DB (#1289 Claim5)",
        );
        assert_eq!(
            report.session_claims_duplicates_deduped, 1,
            "v12 must release exactly the older modeless duplicate"
        );

        // Convergence: v21 re-added `mode` and rebuilt the mode-scoped index; the
        // newest heartbeat survived active, the older is released.
        assert!(
            table_has_column(&conn, "session_claims", "mode").unwrap(),
            "v21 must re-add the mode column after v12"
        );
        let old_state: String = conn
            .query_row(
                "SELECT state FROM session_claims WHERE claim_id = 'old-dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_state, "released");
        let new_state: String = conn
            .query_row(
                "SELECT state FROM session_claims WHERE claim_id = 'new-dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_state, "active");
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='idx_session_claims_identity_active'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            idx, 1,
            "the identity index must be rebuilt after convergence"
        );
    }

    #[test]
    fn v7_reconciles_indexed_tags_after_v5_sentinel() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                keywords TEXT NOT NULL DEFAULT '[]',
                indexed_tags TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]',
                domain TEXT,
                domain_key TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE hard_state (
                namespace TEXT NOT NULL,
                key TEXT NOT NULL,
                value_json TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (namespace, key)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, keywords, indexed_tags, entities) VALUES ('m1', '[]', '[\"rust\"]', '[]')",
            [],
        )
        .unwrap();
        mark_run(&conn, "v5_drop_hypertachi_legacy_columns").unwrap();

        let actions = migrate_v7_reconcile_legacy_memory_columns(&conn).unwrap();
        assert!(actions > 0);
        assert!(!table_has_column(&conn, "memories", "indexed_tags").unwrap());
        assert!(!table_has_column(&conn, "memories", "persons").unwrap());

        let keywords: String = conn
            .query_row("SELECT keywords FROM memories WHERE id='m1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(keywords.contains("rust"));
    }

    #[test]
    fn v6_folds_persons_into_entities() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                persons TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]'
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES (?1, ?2, ?3)",
            params!["m1", r#"["Kyle"]"#, r#"["Sigil"]"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES ('m2', '[]', '[]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, persons, entities) VALUES (?1, ?2, ?3)",
            params!["m3", r#"["sigil"]"#, r#"["Sigil"]"#],
        )
        .unwrap();

        let folded = migrate_v6_fold_persons_into_entities(&conn).unwrap();
        assert_eq!(folded, 2);

        let (persons, entities): (String, String) = conn
            .query_row(
                "SELECT persons, entities FROM memories WHERE id='m1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(persons, "[]");
        let ents: Vec<String> = serde_json::from_str(&entities).unwrap();
        assert!(ents.iter().any(|e| e == "Kyle"));
        assert!(ents.iter().any(|e| e == "user"));
        assert!(ents.iter().any(|e| e == "Sigil"));

        let duplicate_persons: String = conn
            .query_row("SELECT persons FROM memories WHERE id='m3'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(duplicate_persons, "[]");
    }

    #[test]
    fn v5_drops_hypertachi_legacy_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                indexed_tags TEXT NOT NULL DEFAULT '[]',
                domain_key TEXT NOT NULL DEFAULT ''
            );",
        )
        .unwrap();
        let dropped = migrate_v5_drop_hypertachi_legacy_columns(&conn).unwrap();
        assert_eq!(dropped, 2);
        assert!(!table_has_column(&conn, "memories", "indexed_tags").unwrap());
        assert!(!table_has_column(&conn, "memories", "domain_key").unwrap());
        assert_eq!(migrate_v5_drop_hypertachi_legacy_columns(&conn).unwrap(), 0);
    }

    #[test]
    fn v9_relocates_path_like_location_and_drops_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL DEFAULT '/',
                location TEXT NOT NULL DEFAULT '',
                metadata TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, ?4)",
            params!["m1", "/", "/scratch/hyperion", "{}"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, ?4)",
            params!["m2", "/notes/x", "/code-review/sigil", "{}"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, ?4)",
            params!["m3", "/facts/y", "Shanghai", "{}"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, ?4)",
            params!["m4", "/facts/z", "Paris", r#"["legacy"]"#],
        )
        .unwrap();

        let (relocated, dropped) = migrate_v9_relocate_and_drop_location(&conn).unwrap();
        assert_eq!(relocated, 4);
        assert_eq!(dropped, 1);
        assert!(!table_has_column(&conn, "memories", "location").unwrap());

        let path: String = conn
            .query_row("SELECT path FROM memories WHERE id='m1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(path, "/scratch/hyperion");

        let metadata: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='m2'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(meta["context_path"], "/code-review/sigil");

        let metadata: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='m3'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(meta["geo"], "Shanghai");

        let metadata: String = conn
            .query_row("SELECT metadata FROM memories WHERE id='m4'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(meta["geo"], "Paris");
        assert_eq!(meta["legacy_metadata"], json!(["legacy"]));
    }

    #[test]
    fn v9_relocates_many_location_rows_in_batches() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL DEFAULT '/',
                location TEXT NOT NULL DEFAULT '',
                metadata TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();

        let batch = conn.transaction().unwrap();
        {
            let mut stmt = batch
                .prepare(
                    "INSERT INTO memories (id, path, location, metadata) VALUES (?1, ?2, ?3, '{}')",
                )
                .unwrap();
            for i in 0..1200 {
                let id = format!("row-{i:04}");
                stmt.execute(params![id, "/", format!("/scratch/batch-{i}")])
                    .unwrap();
            }
        }
        batch.commit().unwrap();

        let (relocated, dropped) = migrate_v9_relocate_and_drop_location(&conn).unwrap();
        assert_eq!(relocated, 1200);
        assert_eq!(dropped, 1);
        assert!(!table_has_column(&conn, "memories", "location").unwrap());

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE trim(path) = '/'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn v9_relocate_location_rows_handles_empty_batch() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL DEFAULT '/',
                location TEXT NOT NULL DEFAULT '',
                metadata TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();

        let relocated = relocate_location_rows(&conn).unwrap();
        assert_eq!(relocated, 0);
    }

    fn table_present(conn: &Connection, name: &str) -> bool {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                rusqlite::params![name],
                |row| row.get(0),
            )
            .unwrap();
        n > 0
    }

    #[test]
    fn v10_drops_legacy_pack_tables() {
        let (mut conn, tmp) = open_test_db();
        // init_schema no longer creates the pack tables; emulate a legacy DB
        // that still carries them by creating them manually.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS packs (id TEXT PRIMARY KEY, name TEXT);
             CREATE TABLE IF NOT EXISTS agent_projections (agent TEXT, pack_id TEXT);",
        )
        .unwrap();
        assert!(table_present(&conn, "packs"));
        assert!(table_present(&conn, "agent_projections"));

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.pack_tables_dropped, 2);
        assert!(!table_present(&conn, "packs"));
        assert!(!table_present(&conn, "agent_projections"));

        // Idempotent: re-running is a no-op (sentinel guards it).
        let report2 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report2.pack_tables_dropped, 0);
    }

    #[test]
    fn v10_is_a_noop_on_db_without_pack_tables() {
        let (mut conn, tmp) = open_test_db();
        assert!(!table_present(&conn, "packs"));
        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.pack_tables_dropped, 0);
        assert!(!table_present(&conn, "packs"));
    }

    #[test]
    fn v11_drops_legacy_domains_table() {
        let (mut conn, tmp) = open_test_db();
        // init_schema no longer creates the `domains` registry table; emulate
        // a legacy DB that still carries it (and a row) by creating it
        // manually.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS domains (
                name TEXT PRIMARY KEY,
                description TEXT NOT NULL DEFAULT ''
            );
            INSERT INTO domains (name, description) VALUES ('legacy', 'old registry row');",
        )
        .unwrap();
        assert!(table_present(&conn, "domains"));

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.domains_table_dropped, 1);
        assert!(!table_present(&conn, "domains"));

        // The live free-text `memories.domain` column is untouched.
        assert!(table_has_column(&conn, "memories", "domain").unwrap());

        // Idempotent: re-running is a no-op (sentinel guards it).
        let report2 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report2.domains_table_dropped, 0);
    }

    #[test]
    fn v11_is_a_noop_on_db_without_domains_table() {
        let (mut conn, tmp) = open_test_db();
        assert!(!table_present(&conn, "domains"));
        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.domains_table_dropped, 0);
        assert!(!table_present(&conn, "domains"));
    }

    #[test]
    fn v13_adds_hard_state_namespace_updated_index() {
        let (mut conn, tmp) = open_test_db();
        // #1289 ruling A: `init_schema`'s product IS the complete current
        // schema, so a freshly built DB already carries this index (it now
        // lives in ddl.rs's MIGRATED_INDEXES_SQL, not only in the v13 sentinel
        // migration). The v13 migration is therefore an idempotent no-op on a
        // fresh DB and only does real creation work on a legacy DB predating
        // the index — exercised explicitly at the end of this test.
        assert!(
            index_present(&conn, "idx_hard_state_ns_updated"),
            "init_schema must carry idx_hard_state_ns_updated (#1289 ruling A)"
        );

        // init_schema runs DDL only, not run_data_migrations, so the v13
        // sentinel is unset and the migration still runs — as an
        // IF NOT EXISTS no-op — reporting it ran once. The index stays.
        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.hard_state_index_added, 1);
        assert!(index_present(&conn, "idx_hard_state_ns_updated"));

        // The query plan for list_state's WHERE+ORDER BY now uses the new
        // index instead of falling back to a temp B-tree sort.
        let plan = query_plan(
            &conn,
            "SELECT key, value_json, version, updated_at FROM hard_state \
             WHERE namespace = 'orchestrator' ORDER BY updated_at DESC, key ASC",
        );
        assert!(
            plan.iter()
                .any(|line| line.contains("idx_hard_state_ns_updated")),
            "expected plan to use idx_hard_state_ns_updated, got: {plan:?}"
        );
        // The index provides updated_at DESC order directly, so SQLite no
        // longer needs a full sort of the result set — at most a tiny
        // "LAST TERM OF ORDER BY" tie-break sort among rows sharing the same
        // updated_at (the secondary `key ASC` term). A full/bare
        // "USE TEMP B-TREE FOR ORDER BY" (sorting on every term) would mean
        // the index isn't actually satisfying the primary sort key.
        assert!(
            !plan
                .iter()
                .any(|line| line == "USE TEMP B-TREE FOR ORDER BY"),
            "expected no full temp B-tree sort once indexed, got: {plan:?}"
        );

        // Idempotent: re-running is a no-op (sentinel guards it), index stays.
        let report2 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report2.hard_state_index_added, 0);
        assert!(index_present(&conn, "idx_hard_state_ns_updated"));

        // v13's creation behavior on a GENUINELY pre-v13 legacy DB, through the
        // REAL sentinel-gated `run_data_migrations` path — NOT the migration
        // helper in isolation (#1289 Claim4). Calling the helper directly proves
        // only its `CREATE INDEX IF NOT EXISTS`; it bypasses the sentinel gate,
        // so it never shows that the migration, as invoked in production, fires
        // on an index-absent DB. And `hard_state_index_added == 1` above is not
        // creation evidence: `migrate_v13_add_hard_state_index` returns a
        // constant 1 whenever it runs, and there the index already existed.
        //
        // Reconstruct the real pre-v13 state — index absent AND v13 sentinel
        // unset — then drive the public entry. Only if the gate actually re-runs
        // v13 does the index reappear.
        conn.execute_batch("DROP INDEX IF EXISTS idx_hard_state_ns_updated;")
            .unwrap();
        conn.execute(
            "DELETE FROM hard_state WHERE namespace = 'migrations' \
             AND key = 'v13_hard_state_ns_updated_index'",
            [],
        )
        .unwrap();
        write_schema_version(&conn, 12).unwrap();
        assert!(!index_present(&conn, "idx_hard_state_ns_updated"));
        assert!(
            !was_run(&conn, "v13_hard_state_ns_updated_index").unwrap(),
            "fixture precondition: v13 sentinel cleared so the gate re-runs it"
        );

        let report3 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(
            report3.hard_state_index_added, 1,
            "the sentinel-gated v13 migration must fire on a pre-v13 (index-absent) DB"
        );
        assert!(
            index_present(&conn, "idx_hard_state_ns_updated"),
            "v13 must create the index on a legacy DB that lacks it"
        );
    }

    fn index_present(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(3)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    // --- #984: schema version stamp / hard-fail gate ---------------------

    #[test]
    fn schema_version_gate_errors_on_db_stamped_newer_than_supported() {
        let (conn, _tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION + 1).unwrap();

        let err = check_schema_version_gate(&conn).expect_err("newer stamp must hard-fail");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!(
                "db schema version {} newer than supported {}",
                EXPECTED_SCHEMA_VERSION + 1,
                EXPECTED_SCHEMA_VERSION
            )),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn fresh_db_migrates_and_ends_stamped_at_expected_version() {
        let (mut conn, tmp) = open_test_db();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);

        run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    #[test]
    fn private_fresh_init_installs_v25_and_v26_through_migration_once() {
        let _ = crate::db::enable_simple_auto_extension();
        register_sqlite_vec();
        let conn = Connection::open_in_memory().expect("open in-memory");
        let _ = try_load_sqlite_vec(&conn);

        init_schema(&conn).expect("initialize current private schema");
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
        let sentinel_version: i64 = conn
            .query_row(
                "SELECT version FROM hard_state
                 WHERE namespace = ?1 AND key = 'v25_recall_impression_ledger'",
                [MIGRATION_NS],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sentinel_version, 1);
        let v26_sentinel_version: i64 = conn
            .query_row(
                "SELECT version FROM hard_state
                 WHERE namespace = ?1 AND key = 'v26_recall_impression_replay_identity'",
                [MIGRATION_NS],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v26_sentinel_version, 1);
        crate::db::schema::validate_recall_impression_ledger_schema(&conn).unwrap();

        init_schema(&conn).expect("valid current private schema reopens idempotently");
        let sentinel_version_after: i64 = conn
            .query_row(
                "SELECT version FROM hard_state
                 WHERE namespace = ?1 AND key = 'v25_recall_impression_ledger'",
                [MIGRATION_NS],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sentinel_version_after, 1, "v25 migration must run once");
        let v26_sentinel_version_after: i64 = conn
            .query_row(
                "SELECT version FROM hard_state
                 WHERE namespace = ?1 AND key = 'v26_recall_impression_replay_identity'",
                [MIGRATION_NS],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v26_sentinel_version_after, 1, "v26 migration must run once");
    }

    #[test]
    fn older_stamped_db_migrates_forward_and_re_stamps() {
        let (mut conn, tmp) = open_test_db();
        // A DB with an explicit older-than-current stamp (but no legacy data
        // and no sentinels — see `unstamped_db_with_existing_sentinels_skips_and_restamps`
        // below for the genuine "already migrated by an older kernel" case)
        // must still migrate forward (no-op, nothing to touch) and re-stamp.
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        // No legacy tables/rows to touch on a freshly-initialized DB, so the
        // report is all-zero; the assertion under test is the re-stamp.
        assert_eq!(report.domains_table_dropped, 0);
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    #[test]
    fn v19_open_pipeline_preserves_legacy_duplicates_and_adds_identity_constraint() {
        let (conn, tmp) = open_test_db();
        conn.execute_batch(
            "DROP INDEX idx_memories_idless_identity_active;
             ALTER TABLE memories DROP COLUMN idless_identity;
             INSERT INTO memories (id, path, text, timestamp)
             VALUES
                 ('legacy-idless-a', '/legacy/duplicate', 'same legacy text', '2026-07-16T00:00:00Z'),
                 ('legacy-idless-b', '/legacy/duplicate', 'same legacy text', '2026-07-16T00:00:00Z');",
        )
        .unwrap();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();
        drop(conn);

        let path = tmp.path().to_str().unwrap();
        let ctx = DbOpenContext::open_existing_allow("test:#1115");
        let store = crate::MemoryStore::open_with_context(path, &ctx).unwrap();

        assert_eq!(
            read_schema_version(store.connection()).unwrap(),
            EXPECTED_SCHEMA_VERSION
        );
        assert!(
            table_has_column(store.connection(), "memories", "idless_identity").unwrap(),
            "the v19 open pipeline must add the modern identity column"
        );
        assert!(
            index_present(store.connection(), "idx_memories_idless_identity_active"),
            "the v19 open pipeline must add the modern identity constraint"
        );
        let legacy_rows: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memories
                 WHERE path = '/legacy/duplicate'
                   AND text = 'same legacy text'
                   AND idless_identity IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            legacy_rows, 2,
            "migration must leave legacy duplicates untouched"
        );
    }

    /// #984 F3(a): a genuine fixture for "DB last migrated by an older
    /// kernel" — sentinel rows actually exist (inserted directly via
    /// `mark_run`, the same mechanism the migrations themselves use) for
    /// every migration, and the version stamp is left at 0 (as it would be
    /// for any real DB written before #984 introduced the stamp). Unlike
    /// `older_stamped_db_migrates_forward_and_re_stamps`, this proves the
    /// sentinel-skip path itself, not just "nothing to migrate on a fresh DB".
    #[test]
    fn unstamped_db_with_existing_sentinels_skips_and_restamps() {
        let (mut conn, tmp) = open_test_db();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);

        for key in MIGRATION_SENTINEL_KEYS {
            assert!(!was_run(&conn, key).unwrap(), "sentinel {key} pre-seeded?");
            mark_run(&conn, key).unwrap();
        }

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        // Every migration was already marked run, so every counted field is
        // zero — the sentinels actually skipped the work, not "there was no
        // work regardless".
        assert_eq!(report.paths_normalized, 0);
        assert_eq!(report.scopes_fixed, 0);
        assert_eq!(report.handoff_paths_standardized, 0);
        assert_eq!(report.quarantined, 0);
        assert_eq!(report.hypertachi_legacy_columns_dropped, 0);
        assert_eq!(report.persons_folded_into_entities, 0);
        assert_eq!(report.legacy_columns_reconciled, 0);
        assert_eq!(report.persons_columns_dropped, 0);
        assert_eq!(report.locations_relocated, 0);
        assert_eq!(report.location_columns_dropped, 0);
        assert_eq!(report.pack_tables_dropped, 0);
        assert_eq!(report.domains_table_dropped, 0);
        assert_eq!(report.session_claims_duplicates_deduped, 0);

        // Sentinels skipped the data work, but the version stamp — which is
        // independent of the sentinel mechanism — still advances.
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    #[test]
    fn gate_is_checked_before_any_migration_runs() {
        let (mut conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION + 5).unwrap();

        // init_schema_with_label_mut (the real entry point) must refuse too.
        let result = crate::db::init_schema_with_label_mut(
            &mut conn,
            "global",
            tmp.path(),
            &crate::db::DbOpenContext::create_fresh(),
        );
        assert!(
            result.is_err(),
            "gate must reject via the schema.rs entry point too"
        );
    }

    // --- #1119: typed DbOpenContext migration gate ------------------------

    use crate::db::DbOpenContext;

    fn is_opt_in_required(result: Result<(), MemoryError>) -> bool {
        matches!(
            result,
            Err(MemoryError::SchemaMigrationOptInRequired { .. })
        )
    }

    fn is_create_target_exists(result: Result<(), MemoryError>) -> bool {
        matches!(result, Err(MemoryError::DbCreateTargetExists { .. }))
    }

    /// The frozen assertion this REPLACES (old route:
    /// `opt_in_gate_with_is_noop_for_fresh_db`, which inferred "fresh" from a
    /// `sqlite_master` table count). New semantics: a full-schema DB that still
    /// reads `user_version == 0` — exactly what the unversioned migration-test
    /// fixture produces — is a build, not a migration, under `CreateFresh`. Intent
    /// carries "I am creating", so no authority is needed and DB content is
    /// never consulted (owner ruling A: `init_schema`'s product IS fresh).
    #[test]
    fn create_fresh_needs_no_authority_regardless_of_db_content() {
        let (conn, tmp) = open_test_db();
        // Full pre-versioned tables are present but user_version==0.
        assert_eq!(read_schema_version(&conn).unwrap(), 0);
        let ctx = DbOpenContext::create_fresh();
        check_db_open_context_gate(&conn, tmp.path(), &ctx)
            .expect("CreateFresh builds a fresh (v0) DB with no authority");
    }

    #[test]
    fn open_existing_deny_allows_fresh_zero_stamped_db() {
        // stored == 0 is a build, not the incident (which was a stamped 17→18).
        let (conn, tmp) = open_test_db();
        let ctx = DbOpenContext::open_existing_deny();
        check_db_open_context_gate(&conn, tmp.path(), &ctx)
            .expect("a v0 file has no deployed daemon to strand");
    }

    #[test]
    fn open_existing_deny_refuses_stamped_older_db() {
        // The #1119 incident path: a stamped older DB (e.g. 17) opened by an
        // 18-binary without authority MUST refuse before migrating in place.
        let (conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();
        let ctx = DbOpenContext::open_existing_deny();
        assert!(
            is_opt_in_required(check_db_open_context_gate(&conn, tmp.path(), &ctx)),
            "Deny must refuse a stamped older DB"
        );
    }

    #[test]
    fn open_existing_allow_migrates_stamped_older_db() {
        let (conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();
        let ctx = DbOpenContext::open_existing_allow("test:deploy");
        check_db_open_context_gate(&conn, tmp.path(), &ctx)
            .expect("Allow authorizes migrating a stamped older DB");
    }

    #[test]
    fn create_fresh_refuses_stamped_older_db_as_existing() {
        // A real older operational DB is not a create target — refuse (use
        // OpenExisting+Allow to migrate it).
        let (conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();
        let ctx = DbOpenContext::create_fresh();
        assert!(
            is_create_target_exists(check_db_open_context_gate(&conn, tmp.path(), &ctx)),
            "CreateFresh must refuse a stamped older DB as already-existing"
        );
    }

    /// Authorization must NOT travel through the process environment. Even
    /// with the legacy opt-in env var (`TACHI_ALLOW_SCHEMA_MIGRATION`) set to a
    /// truthy value, a `Deny` context still refuses a stamped older DB — the
    /// gate consults only the typed `DbOpenContext`, never the environment.
    /// Nothing else in the codebase reads this var, so setting it here cannot
    /// affect any other test.
    #[test]
    fn authority_does_not_travel_through_process_env() {
        let (conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();

        std::env::set_var(crate::db::SCHEMA_MIGRATION_LEGACY_ENV, "1");
        let refused = is_opt_in_required(check_db_open_context_gate(
            &conn,
            tmp.path(),
            &DbOpenContext::open_existing_deny(),
        ));
        std::env::remove_var(crate::db::SCHEMA_MIGRATION_LEGACY_ENV);

        assert!(
            refused,
            "Deny must refuse even when the legacy opt-in env var is set — \
             authority is typed, not ambient"
        );
    }

    #[test]
    fn open_existing_deny_allows_current_version() {
        let (conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION).unwrap();
        check_db_open_context_gate(&conn, tmp.path(), &DbOpenContext::open_existing_deny())
            .expect("OpenExisting at current version is a no-op");
    }

    #[test]
    fn create_fresh_refuses_current_version_db_as_existing() {
        // "遇已存在库 → 拒绝": a current (stamped) operational DB is not a
        // create target either — only stored == 0 is a fresh build.
        let (conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION).unwrap();
        let ctx = DbOpenContext::create_fresh();
        assert!(
            is_create_target_exists(check_db_open_context_gate(&conn, tmp.path(), &ctx)),
            "CreateFresh must refuse a stamped current DB as already-existing"
        );
    }

    // --- #984 F2: read-only opens are gated too ---------------------------

    #[test]
    fn read_only_open_rejects_db_stamped_newer_than_supported() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        {
            let _ = crate::db::enable_simple_auto_extension();
            register_sqlite_vec();
            let conn = Connection::open(tmp.path()).expect("open");
            let _ = try_load_sqlite_vec(&conn);
            init_schema(&conn).expect("init_schema");
            write_schema_version(&conn, EXPECTED_SCHEMA_VERSION + 1).unwrap();
        }

        let path = tmp.path().to_str().expect("utf8 tmp path");
        // Match rather than expect_err: MemoryStore (the Ok variant) is not Debug,
        // and we don't want to derive Debug on a struct holding live connections.
        let err = match crate::MemoryStore::open_read_only(path) {
            Ok(_) => panic!("read-only open of a newer-stamped DB must hard-fail"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains(&format!(
                "db schema version {} newer than supported {}",
                EXPECTED_SCHEMA_VERSION + 1,
                EXPECTED_SCHEMA_VERSION
            )),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn read_only_open_refuses_older_stamped_db_until_migrated() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        {
            let _ = crate::db::enable_simple_auto_extension();
            register_sqlite_vec();
            let conn = Connection::open(tmp.path()).expect("open");
            let _ = try_load_sqlite_vec(&conn);
            init_schema(&conn).expect("init_schema");
            write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();
        }

        let path = tmp.path().to_str().expect("utf8 tmp path");
        let err = match crate::MemoryStore::open_read_only(path) {
            Ok(_) => panic!("read-only open must not consume an older schema"),
            Err(error) => error,
        };
        assert!(
            matches!(
                err,
                MemoryError::SchemaMigrationOptInRequired {
                    stored,
                    expected,
                    ..
                } if stored + 1 == expected
            ),
            "older read-only DB must fail as migration-required, got: {err}"
        );
    }

    #[test]
    fn read_only_open_permits_db_at_current_version() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        {
            let _ = crate::db::enable_simple_auto_extension();
            register_sqlite_vec();
            let conn = Connection::open(tmp.path()).expect("open");
            let _ = try_load_sqlite_vec(&conn);
            init_schema(&conn).expect("init_schema");
            write_schema_version(&conn, EXPECTED_SCHEMA_VERSION).unwrap();
        }

        let path = tmp.path().to_str().expect("utf8 tmp path");
        crate::MemoryStore::open_read_only(path)
            .expect("read-only open at the current version must succeed");
    }

    // --- #984 F1: transactional compatibility boundary ---------------------

    /// Fault-injection proof that the migration effects, sentinel writes, and
    /// final `user_version` stamp are one atomic unit: force a failure after
    /// several migrations have run (and been marked) but before the final
    /// stamp, and assert BOTH the schema/data changes and the stamp are
    /// rolled back together — not just the stamp.
    #[test]
    fn stamp_and_migration_effects_roll_back_together() {
        let (mut conn, tmp) = open_test_db();
        // Seed a legacy `packs` table so v10 has real, observable work to do
        // (and roll back) rather than being a no-op.
        conn.execute_batch("CREATE TABLE IF NOT EXISTS packs (id TEXT PRIMARY KEY, name TEXT);")
            .unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);

        // Run migrations for real up through a known point, then simulate a
        // crash by rolling back the outer transaction ourselves instead of
        // letting `run_data_migrations` commit — this stands in for "the
        // process dies between the migrations and the final PRAGMA write",
        // which we cannot deterministically inject through the public API
        // without a fault-injection connection wrapper. What this proves:
        // the migration effects (sentinel rows, `packs` table drop) and the
        // version stamp live in the SAME transaction, so any rollback of
        // that transaction — crash or otherwise — takes both together. If
        // they were separate autocommit statements (the pre-fix behavior),
        // this rollback would be a no-op on already-committed sub-steps.
        {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            let report = run_data_migrations_in_tx(&tx, "global", tmp.path()).unwrap();
            assert_eq!(
                report.pack_tables_dropped, 1,
                "v10 should have real work here"
            );
            write_schema_version(&tx, EXPECTED_SCHEMA_VERSION).unwrap();
            // Do NOT commit — roll back instead, simulating the crash.
            tx.rollback().unwrap();
        }

        // Both the data effect (packs table still present) and the sentinel
        // (v10 not marked run) and the version stamp (still 0) must have
        // rolled back together.
        assert_eq!(
            read_schema_version(&conn).unwrap(),
            0,
            "version stamp must roll back with the migration effects"
        );
        assert!(
            !was_run(&conn, "v10_drop_pack_tables").unwrap(),
            "sentinel must roll back with the migration effects"
        );
        let packs_still_present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='packs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            packs_still_present, 1,
            "packs table drop must roll back with the version stamp"
        );

        // And a real run (commit path) now proceeds cleanly from scratch.
        let report2 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report2.pack_tables_dropped, 1);
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    // --- #984 F3(e): EXPECTED_SCHEMA_VERSION invariant ----------------------

    /// Ties `EXPECTED_SCHEMA_VERSION` to the migration count the runner
    /// *itself* produces — not a hand-maintained duplicate list — by running
    /// the real `run_data_migrations` against a fresh DB and counting the
    /// sentinel rows it actually wrote to `hard_state`. Appending a
    /// `v13_...` migration to `run_data_migrations_in_tx` (with its own
    /// `mark_run` call, as every migration above does) increases this count
    /// automatically; forgetting to bump `EXPECTED_SCHEMA_VERSION` to match
    /// then fails this test — silently under-stamping newly-migrated DBs
    /// would otherwise defeat the #984 gate for the new migration.
    ///
    /// `MIGRATION_SENTINEL_KEYS` is also exercised by current-schema preflight;
    /// this count check stays independent so runner additions cannot be hidden
    /// by forgetting to update both the stamp and inventory.
    #[test]
    fn expected_schema_version_matches_migration_count() {
        let (mut conn, tmp) = open_test_db();

        run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        let sentinel_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM hard_state WHERE namespace = ?1",
                params![MIGRATION_NS],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(
            sentinel_count, EXPECTED_SCHEMA_VERSION,
            "EXPECTED_SCHEMA_VERSION ({EXPECTED_SCHEMA_VERSION}) must equal the number of \
             sentinel migrations run_data_migrations actually marks run ({sentinel_count}) — \
             bump the const (and add a vN doc line) when a new migration is appended"
        );
        assert_eq!(
            MIGRATION_SENTINEL_KEYS.len(),
            EXPECTED_SCHEMA_VERSION as usize,
            "current-schema sentinel inventory must cover every version"
        );
        for key in MIGRATION_SENTINEL_KEYS {
            assert!(
                was_run(&conn, key).unwrap(),
                "current-schema preflight key '{key}' is not written by the migration runner"
            );
        }
    }

    /// #1331 BUG 3: stamped-v21 DBs must not silently acquire
    /// `memories_symbolic_fts` under Deny; Allow must create+backfill v22 and
    /// continue through the current schema stamp.
    #[test]
    fn v21_to_v22_symbolic_fts_requires_authority_and_stamps() {
        use crate::db::{init_schema_with_label_mut, DbOpenContext};

        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        let path_str = path.to_string_lossy().to_string();
        {
            let _ = crate::db::enable_simple_auto_extension();
            register_sqlite_vec();
            let mut conn = Connection::open(&path).expect("open");
            let _ = try_load_sqlite_vec(&conn);
            init_schema_with_label_mut(&mut conn, "global", &path, &DbOpenContext::create_fresh())
                .expect("provision fresh");
            // Downgrade to a stamped-v21 shape without the symbolic projection.
            const V22_SENTINEL: &str = "v22_memories_symbolic_fts";
            conn.execute(
                "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
                params![MIGRATION_NS, V22_SENTINEL],
            )
            .unwrap();
            conn.execute_batch("DROP TABLE IF EXISTS memories_symbolic_fts;")
                .unwrap();
            write_schema_version(&conn, 21).unwrap();
            insert_row(&conn, "legacy-row", "/notes/v21", "general", "{}");
        }

        // Deny must refuse before DDL recreates the table.
        let deny_err = match crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "global",
            &DbOpenContext::open_existing_deny(),
        ) {
            Ok(_) => panic!("Deny must refuse stamped-v21 → v22"),
            Err(e) => e,
        };
        assert!(
            matches!(
                deny_err,
                MemoryError::SchemaMigrationOptInRequired { stored: 21, .. }
            ),
            "unexpected deny error: {deny_err}"
        );
        {
            let inspect = Connection::open(&path).unwrap();
            let present: bool = inspect
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories_symbolic_fts'",
                    [],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            assert!(
                !present,
                "Deny must not silently CREATE memories_symbolic_fts on a stamped-v21 DB"
            );
            assert_eq!(read_schema_version(&inspect).unwrap(), 21);
        }

        let _store = crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "global",
            &DbOpenContext::open_existing_allow("test:1331-v22"),
        )
        .expect("Allow must migrate v21 → v22");
        let verify = Connection::open(&path).unwrap();
        assert_eq!(
            read_schema_version(&verify).unwrap(),
            EXPECTED_SCHEMA_VERSION
        );
        assert!(was_run(&verify, "v22_memories_symbolic_fts").unwrap());
        let rows: i64 = verify
            .query_row("SELECT COUNT(*) FROM memories_symbolic_fts", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            rows >= 1,
            "v22 must backfill existing memories into the symbolic projection"
        );
    }

    #[test]
    fn v22_to_current_installs_reserved_reference_guards_and_scored_count() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        let path_str = path.to_string_lossy().to_string();
        {
            let store =
                crate::MemoryStore::open_with_context(&path_str, &DbOpenContext::create_fresh())
                    .expect("provision legacy fixture");
            drop(store);
            let conn = Connection::open(&path).expect("open legacy fixture");
            conn.execute(
                "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
                params![MIGRATION_NS, "v23_reserved_reference_guards"],
            )
            .unwrap();
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
                 DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;
                 PRAGMA user_version = 22;",
            )
            .unwrap();
        }

        let store = crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "guide-project",
            &DbOpenContext::open_existing_allow("test:v23-evidence-guards"),
        )
        .expect("authorized v22 migration must install current schema additions");
        drop(store);

        let verify = Connection::open(&path).expect("verify migrated DB");
        assert_eq!(
            read_schema_version(&verify).unwrap(),
            EXPECTED_SCHEMA_VERSION
        );
        assert!(was_run(&verify, "v23_reserved_reference_guards").unwrap());
        assert!(was_run(&verify, "v24_memories_scored_count").unwrap());
        let trigger_count: i64 = verify
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'trigger'
                   AND name IN (
                       'memories_reserved_refs_insert_guard',
                       'memories_reserved_refs_update_guard'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(trigger_count, 2, "v23 canonical guard inventory");
        let scored_count_column: bool = verify
            .query_row(
                "SELECT 1 FROM pragma_table_info('memories') WHERE name = 'scored_count'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(
            scored_count_column,
            "v24 scored_count column must be present"
        );
    }

    #[test]
    fn v23_to_v24_adds_scored_count_with_default_and_sentinel() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        let path_str = path.to_string_lossy().to_string();
        {
            let store =
                crate::MemoryStore::open_with_context(&path_str, &DbOpenContext::create_fresh())
                    .expect("provision fixture");
            drop(store);
            let conn = Connection::open(&path).expect("open fixture");
            conn.execute("ALTER TABLE memories DROP COLUMN scored_count", [])
                .expect("simulate v23 memories table");
            conn.execute(
                "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
                params![MIGRATION_NS, "v24_memories_scored_count"],
            )
            .unwrap();
            conn.execute_batch("PRAGMA user_version = 23;").unwrap();
        }

        let store = crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "guide-project",
            &DbOpenContext::open_existing_allow("test:v24-scored-count"),
        )
        .expect("authorized v23 migration must add scored_count");
        drop(store);
        let verify = Connection::open(&path).expect("verify migrated DB");
        assert_eq!(
            read_schema_version(&verify).unwrap(),
            EXPECTED_SCHEMA_VERSION
        );
        assert!(was_run(&verify, "v24_memories_scored_count").unwrap());
        assert!(was_run(&verify, "v25_recall_impression_ledger").unwrap());
        assert!(was_run(&verify, "v26_recall_impression_replay_identity").unwrap());
        let _reserved_reference_guard = crate::db::register_reserved_reference_write_guard(&verify)
            .expect("register trigger guard function");
        let default: i64 = verify
            .query_row(
                "INSERT INTO memories (id, timestamp) VALUES ('scored-default', '2026-01-01T00:00:00Z') RETURNING scored_count",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(default, 0);
    }

    /// #1447: v25 impression groups did not persist a unique fingerprint or a
    /// replay-policy identity. v26 must be explicitly authorized, retain the
    /// non-unique FNV bucket only under its legacy name, and leave unknown
    /// provenance NULL so replay refuses rather than guessing current math.
    #[test]
    fn v25_to_v26_recall_impressions_requires_authority_and_preserves_unknowns() {
        use crate::db::DbOpenContext;

        const V26_SENTINEL: &str = "v26_recall_impression_replay_identity";
        const V26_OBJECTS: &[(&str, &str)] = &[
            ("table", "recall_impression_groups"),
            ("table", "recall_impressions"),
            ("index", "idx_recall_impression_groups_created"),
            ("index", "idx_recall_impression_groups_fingerprint"),
            ("index", "idx_recall_impressions_memory"),
            ("index", "idx_recall_impressions_group_final_rank"),
        ];

        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        let path_str = path.to_string_lossy().to_string();
        {
            let store =
                crate::MemoryStore::open_with_context(&path_str, &DbOpenContext::create_fresh())
                    .expect("provision fixture");
            drop(store);
            let conn = Connection::open(&path).expect("open fixture");
            assert_eq!(
                read_schema_version(&conn).unwrap(),
                EXPECTED_SCHEMA_VERSION,
                "fresh provisioning must stamp v26"
            );
            conn.execute(
                "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
                params![MIGRATION_NS, V26_SENTINEL],
            )
            .unwrap();
            conn.execute_batch(
                "DROP TABLE recall_impressions;
                 DROP TABLE recall_impression_groups;",
            )
            .unwrap();
            crate::db::schema::install_v25_recall_impression_ledger_schema(&conn)
                .expect("build historical v25 ledger fixture");
            conn.execute(
                "INSERT INTO recall_impression_groups (group_id, created_at, query_hash, weights_profile, semantic_weight, fts_weight, symbolic_weight, decay_weight, use_rrf, rrf_k, top_k, candidate_count, displayed_count, scored_returned_count)
                 VALUES ('v25-group', '2026-07-29T00:00:00.000Z', 'deadbeef', 'default', 0.4, 0.3, 0.2, 0.1, 0, 20.0, 10, 0, 0, 0)",
                [],
            )
            .unwrap();
            conn.execute_batch("PRAGMA user_version = 25;").unwrap();
        }

        let deny_err = match crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "global",
            &DbOpenContext::open_existing_deny(),
        ) {
            Ok(_) => panic!("Deny must refuse stamped-v25 -> v26"),
            Err(error) => error,
        };
        assert!(
            matches!(
                deny_err,
                MemoryError::SchemaMigrationOptInRequired { stored: 25, .. }
            ),
            "unexpected deny error: {deny_err}"
        );
        {
            let inspect = Connection::open(&path).expect("inspect denied DB");
            assert_eq!(read_schema_version(&inspect).unwrap(), 25);
            assert!(!was_run(&inspect, V26_SENTINEL).unwrap());
            assert!(table_has_column(&inspect, "recall_impression_groups", "query_hash").unwrap());
            assert!(
                !table_has_column(&inspect, "recall_impression_groups", "query_fingerprint")
                    .unwrap()
            );
        }

        let store = crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "global",
            &DbOpenContext::open_existing_allow("test:1447-v26"),
        )
        .expect("Allow must migrate v25 -> v26");
        drop(store);

        let verify = Connection::open(&path).expect("verify migrated DB");
        assert_eq!(read_schema_version(&verify).unwrap(), 26);
        assert!(was_run(&verify, V26_SENTINEL).unwrap());
        for (object_type, name) in V26_OBJECTS {
            let present: bool = verify
                .query_row(
                    "SELECT 1 FROM sqlite_schema WHERE type = ?1 AND name = ?2",
                    params![object_type, name],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            assert!(present, "v26 must create {object_type} {name}");
        }
        assert!(
            !table_has_column(&verify, "recall_impression_groups", "query_hash").unwrap(),
            "v26 must not retain the ambiguous v25 column name"
        );
        for column in [
            "legacy_query_bucket",
            "query_fingerprint",
            "fusion_policy_version",
            "pre_boost_adjustment_version",
            "tie_break_policy_version",
            "candidate_policy_version",
            "schema_identity",
        ] {
            assert!(
                table_has_column(&verify, "recall_impression_groups", column).unwrap(),
                "v26 group schema must contain {column}"
            );
        }
        let (legacy_bucket, fingerprint, fusion, adjustment, tie_break, candidate, schema): (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = verify
            .query_row(
                "SELECT legacy_query_bucket, query_fingerprint, fusion_policy_version, pre_boost_adjustment_version, tie_break_policy_version, candidate_policy_version, schema_identity FROM recall_impression_groups WHERE group_id = 'v25-group'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(legacy_bucket, "deadbeef");
        assert_eq!(
            (
                fingerprint,
                fusion,
                adjustment,
                tie_break,
                candidate,
                schema
            ),
            (None, None, None, None, None, None),
            "v26 must not fabricate unavailable query or policy provenance"
        );
        let error = crate::replay_recall_impression_group(&verify, "v25-group")
            .expect_err("unversioned v25 group must not run current replay math");
        assert!(matches!(
            error,
            MemoryError::RecallReplayIncompatible {
                reason: crate::error::RecallReplayCompatibilityReason::LegacyUnversioned,
                ..
            }
        ));
    }

    fn current_schema_snapshot(path: &Path) -> (Vec<u8>, Vec<String>, u32, Vec<String>) {
        let bytes = std::fs::read(path).expect("read database bytes");
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open snapshot read-only");
        let mut schema_stmt = conn
            .prepare(
                "SELECT type || char(31) || name || char(31) || tbl_name || char(31) || COALESCE(sql, '')
                 FROM sqlite_schema ORDER BY type, name, tbl_name",
            )
            .unwrap();
        let schema = schema_stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(schema_stmt);
        let mut sentinel_stmt = conn
            .prepare(
                "SELECT key || char(31) || value_json || char(31) || version || char(31) || created_at || char(31) || updated_at
                 FROM hard_state WHERE namespace = ?1 ORDER BY key",
            )
            .unwrap();
        let sentinels = sentinel_stmt
            .query_map([MIGRATION_NS], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(sentinel_stmt);
        let version = read_schema_version(&conn).unwrap();
        (bytes, schema, version, sentinels)
    }

    type ExistingOpen = fn(&str) -> Result<crate::MemoryStore, MemoryError>;

    fn open_mutating_existing_deny(path: &str) -> Result<crate::MemoryStore, MemoryError> {
        crate::MemoryStore::open_with_label_and_context(
            path,
            "global",
            &DbOpenContext::open_existing_deny(),
        )
    }

    fn current_existing_openers() -> [(&'static str, ExistingOpen); 3] {
        [
            ("mutating", open_mutating_existing_deny),
            ("read-only", crate::MemoryStore::open_read_only),
            (
                "existing-read-write",
                crate::MemoryStore::open_existing_read_write,
            ),
        ]
    }

    fn assert_current_v26_corruption_is_not_repaired(corruption_sql: &str, expected: &str) {
        let mut unexpected_acceptances = Vec::new();
        for (surface, open) in current_existing_openers() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join(format!("{surface}.db"));
            let path_str = path.to_string_lossy().to_string();
            {
                let store = crate::MemoryStore::open_with_context(
                    &path_str,
                    &DbOpenContext::create_fresh(),
                )
                .expect("provision current v26 fixture");
                drop(store);
                let conn = Connection::open(&path).expect("open fixture for corruption");
                conn.execute_batch(corruption_sql).expect("corrupt fixture");
            }
            let before = current_schema_snapshot(&path);

            match open(&path_str) {
                Ok(store) => {
                    drop(store);
                    unexpected_acceptances.push(surface);
                }
                Err(error) => assert!(
                    error.to_string().contains(expected),
                    "unexpected {surface} corruption error: {error}"
                ),
            }

            let after = current_schema_snapshot(&path);
            assert_eq!(
                after.0, before.0,
                "failed {surface} current open must be byte-identical"
            );
            assert_eq!(
                after.1, before.1,
                "failed {surface} current open must not repair schema"
            );
            assert_eq!(
                after.2, before.2,
                "failed {surface} current open must not re-stamp"
            );
            assert_eq!(
                after.3, before.3,
                "failed {surface} current open must not repair migration sentinels"
            );
        }
        assert!(
            unexpected_acceptances.is_empty(),
            "stamped-current corrupt v26 DB was accepted by {unexpected_acceptances:?}"
        );
    }

    #[test]
    fn stamped_current_v26_missing_ledger_table_or_index_is_refused_without_repair() {
        assert_current_v26_corruption_is_not_repaired(
            "DROP TABLE recall_impressions;",
            "recall_impressions",
        );
        assert_current_v26_corruption_is_not_repaired(
            "DROP INDEX idx_recall_impression_groups_fingerprint;",
            "idx_recall_impression_groups_fingerprint",
        );
    }

    #[test]
    fn stamped_current_v26_malformed_replay_identity_is_refused_without_repair() {
        assert_current_v26_corruption_is_not_repaired(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO recall_impression_groups (
                 group_id, created_at,
                 fusion_policy_version, pre_boost_adjustment_version,
                 tie_break_policy_version, candidate_policy_version, schema_identity,
                 weights_profile, semantic_weight, fts_weight, symbolic_weight,
                 decay_weight, use_rrf, rrf_k, top_k, candidate_count,
                 displayed_count, scored_returned_count
             ) VALUES (
                 'malformed-current', '2026-07-29T00:00:00Z',
                 'fusion-v1', 'pre-boost-adjustment-v1',
                 'recall-rank-v1', 'candidate-set-v1', 'recall-impression-ledger-v26',
                 'default', 0.65, 0.35, 0.0, 0.0, 0, 60.0, 10, 1, 0, 0
             );
             PRAGMA ignore_check_constraints = OFF;",
            "malformed replay identity row",
        );
    }

    #[test]
    fn stamped_current_v26_missing_sentinel_is_refused_without_repair() {
        assert_current_v26_corruption_is_not_repaired(
            "DELETE FROM hard_state
             WHERE namespace = 'migrations' AND key = 'v26_recall_impression_replay_identity';",
            "v26_recall_impression_replay_identity",
        );
    }

    #[test]
    fn valid_stamped_current_v26_reopens_on_all_existing_surfaces() {
        for (surface, open) in current_existing_openers() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join(format!("{surface}.db"));
            let path_str = path.to_string_lossy().to_string();
            let store =
                crate::MemoryStore::open_with_context(&path_str, &DbOpenContext::create_fresh())
                    .expect("provision current v26 fixture");
            drop(store);
            let before = current_schema_snapshot(&path);

            let reopened = open(&path_str)
                .unwrap_or_else(|error| panic!("valid current v26 {surface} reopen: {error}"));
            drop(reopened);

            let after = current_schema_snapshot(&path);
            assert_eq!(
                after.1, before.1,
                "valid {surface} open must preserve schema"
            );
            assert_eq!(
                after.2, before.2,
                "valid {surface} open must preserve version"
            );
            assert_eq!(
                after.3, before.3,
                "valid {surface} open must preserve migration sentinels"
            );
        }
    }

    #[test]
    fn v23_guard_install_failure_rolls_back_triggers_sentinel_and_stamp() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        let path_str = path.to_string_lossy().to_string();
        {
            let store =
                crate::MemoryStore::open_with_context(&path_str, &DbOpenContext::create_fresh())
                    .expect("provision current fixture");
            drop(store);
            let conn = Connection::open(&path).expect("open legacy fixture");
            conn.execute(
                "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
                params![MIGRATION_NS, "v23_reserved_reference_guards"],
            )
            .unwrap();
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
                 DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;
                 PRAGMA user_version = 22;",
            )
            .unwrap();
        }

        test_hooks::arm_fail_after_v23_guard_install();
        let err = match crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "guide-project",
            &DbOpenContext::open_existing_allow("test:v23-rollback"),
        ) {
            Ok(_) => panic!("injected v23 guard failure must abort migration"),
            Err(error) => error,
        };
        assert!(
            err.to_string()
                .contains("injected failure after v23 guard install"),
            "unexpected injected migration error: {err}"
        );

        let verify = Connection::open(&path).expect("verify rolled-back DB");
        assert_eq!(read_schema_version(&verify).unwrap(), 22);
        assert!(!was_run(&verify, "v23_reserved_reference_guards").unwrap());
        let trigger_count: i64 = verify
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'trigger'
                   AND name IN (
                       'memories_reserved_refs_insert_guard',
                       'memories_reserved_refs_update_guard'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            trigger_count, 0,
            "failed migration must roll guard DDL back"
        );
    }

    #[test]
    fn stamped_v23_with_missing_guards_is_refused_even_with_migration_authority() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        let path_str = path.to_string_lossy().to_string();
        {
            let store =
                crate::MemoryStore::open_with_context(&path_str, &DbOpenContext::create_fresh())
                    .expect("provision current fixture");
            drop(store);
            let conn = Connection::open(&path).expect("open current fixture");
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
                 DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;
                 PRAGMA user_version = 23;",
            )
            .unwrap();
        }

        let err = match crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "guide-project",
            &DbOpenContext::open_existing_allow("test:must-not-repair-v23"),
        ) {
            Ok(_) => panic!("a damaged v23 DB must not be repaired during open"),
            Err(error) => error,
        };
        let message = err.to_string();
        assert!(
            message.contains("unsafe persistent trigger inventory")
                && message.contains("memories_reserved_refs_insert_guard"),
            "damaged current DB must fail as unsafe inventory, got: {err}"
        );

        let verify = Connection::open(&path).expect("verify refused DB");
        assert_eq!(read_schema_version(&verify).unwrap(), 23);
        let guard_trigger_count: i64 = verify
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'trigger'
                   AND name IN (
                       'memories_reserved_refs_insert_guard',
                       'memories_reserved_refs_update_guard'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            guard_trigger_count, 0,
            "refusal must not repair the missing evidence-guard inventory"
        );
        crate::db::search_generation(&verify)
            .expect("refusal must preserve the canonical search-generation trigger inventory");
    }

    #[test]
    fn stamped_current_with_missing_search_generation_trigger_is_refused_without_repair() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        let path_str = path.to_string_lossy().to_string();
        {
            let store =
                crate::MemoryStore::open_with_context(&path_str, &DbOpenContext::create_fresh())
                    .expect("provision current fixture");
            drop(store);
            let conn = Connection::open(&path).expect("open current fixture");
            assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
            let guard_trigger_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'trigger'
                       AND name IN (
                           'memories_reserved_refs_insert_guard',
                           'memories_reserved_refs_update_guard'
                       )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(guard_trigger_count, 2, "fixture keeps v23 guards intact");
            conn.execute_batch("DROP TRIGGER memory_search_generation_after_update")
                .expect("remove exactly one canonical search-generation trigger");
        }

        let err = match crate::MemoryStore::open(&path_str) {
            Ok(_) => panic!("a damaged current DB must not repair its search-generation trigger"),
            Err(error) => error,
        };
        let message = err.to_string();
        assert!(
            message.contains("memory search generation trigger")
                && message.contains("memory_search_generation_after_update")
                && message.contains("unsafe"),
            "damaged current DB must fail loudly before schema repair, got: {err}"
        );

        let verify = Connection::open(&path).expect("verify refused DB");
        assert_eq!(
            read_schema_version(&verify).unwrap(),
            EXPECTED_SCHEMA_VERSION
        );
        let trigger_present: bool = verify
            .query_row(
                "SELECT 1 FROM sqlite_schema
                 WHERE type = 'trigger' AND name = 'memory_search_generation_after_update'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(
            !trigger_present,
            "refusal must not repair the missing search-generation trigger"
        );
    }

    /// #1331 BUG 4: after a legacy path rewrite in the same upgrade, symbolic
    /// retrieval must use the post-migration path (v22 full rebuild).
    #[test]
    fn v22_rebuild_makes_post_migration_path_symbolically_retrievable() {
        let (mut conn, tmp) = open_test_db();
        // Clear path + symbolic sentinels so both replay in order.
        conn.execute(
            "DELETE FROM hard_state WHERE namespace = ?1 AND key IN \
             ('v3_handoff_path_standardize', 'v22_memories_symbolic_fts')",
            params![MIGRATION_NS],
        )
        .unwrap();
        write_schema_version(&conn, 21).unwrap();

        conn.execute(
            "INSERT INTO memories
              (id, path, summary, text, importance, timestamp, category, topic,
               keywords, entities, source, scope, archived,
               created_at, updated_at, access_count, last_access, revision,
               metadata, retention_policy, domain)
             VALUES ('handoff-row', '/handoff', 's', 'handoffuniqueterm body', 0.5,
                     '2026-01-01T00:00:00Z', 'fact', '', '[]', '[]', 'manual', 'general', 0,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, NULL, 1,
                     '{}', NULL, NULL)",
            [],
        )
        .unwrap();
        // Stale pre-rewrite projection (simulates ensure_fts_backfilled-before-v3).
        conn.execute(
            "INSERT INTO memories_symbolic_fts
             (id, path, summary, text, keywords, entities, topic)
             VALUES ('handoff-row', '/handoff', 's', 'handoffuniqueterm body', '[]', '[]', '')",
            [],
        )
        .unwrap();

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert!(
            report.handoff_paths_standardized >= 1
                || was_run(&conn, "v3_handoff_path_standardize").unwrap()
        );
        assert!(was_run(&conn, "v22_memories_symbolic_fts").unwrap());

        let path: String = conn
            .query_row(
                "SELECT path FROM memories_symbolic_fts WHERE id = 'handoff-row'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(path, "/handoff/unknown");

        let hits = crate::db::search_symbolic_candidates(
            &conn,
            "handoffuniqueterm",
            10,
            false,
            false,
            Some("/handoff/unknown"),
            None,
            None,
        )
        .unwrap();
        assert!(
            hits.iter().any(|e| e.id == "handoff-row"),
            "symbolic retrieval must use the post-v3 path; got {:?}",
            hits.iter().map(|e| e.id.as_str()).collect::<Vec<_>>()
        );
    }
    /// A deterministic stand-in for the transient lock/I/O/authorizer
    /// failure #978 describes: the existence-check QUERY itself errors,
    /// rather than legitimately finding the table absent.
    ///
    /// Two earlier mechanisms were tried and abandoned as non-deterministic
    /// or infeasible on this codebase:
    /// - Racing a real `BEGIN EXCLUSIVE` held on a second thread against a
    ///   `busy_timeout(0)` reader: `SQLITE_BUSY` was not reliably
    ///   reproducible against the specific `sqlite_master` read the
    ///   migration performs, and the lock-holder thread's own `.expect()`
    ///   calls could panic before the test body ran its assertion.
    /// - Corrupting the DB file's on-disk header bytes and reopening: this
    ///   codebase's `Connection::open` path auto-loads a SQLite extension
    ///   that reads the header eagerly, so `Connection::open` itself fails
    ///   immediately on a corrupted file — there is no window in which a
    ///   corrupt-but-openable connection exists to call the migration fn on.
    ///
    /// This version injects the failure at the QUERY boundary, one level
    /// below where the previous (refuted) version injected it:
    /// `exists_with_query` in `pack_retire.rs` / `domain_retire.rs` is real,
    /// unmodified production code — the `?`-propagation that decides
    /// "error propagates" vs. "collapses to absent" lives inside it. A test
    /// substitutes only `exists_with_query`'s `query: impl Fn(&Connection,
    /// &str) -> Result<i64, rusqlite::Error>` parameter with a closure that
    /// always errors, so reverting `exists_with_query`'s body to
    /// `query(conn, name).unwrap_or(0) > 0` flips these tests from green to
    /// RED (see the discrimination proof in the PR/commit description).
    fn injected_failing_query(_conn: &Connection, _name: &str) -> Result<i64, rusqlite::Error> {
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("simulated existence-check query failure (#978)".to_string()),
        ))
    }

    #[test]
    fn v10_propagates_existence_check_error_and_does_not_mark_sentinel() {
        let (conn, _tmp) = open_test_db();
        assert!(!was_run(&conn, "v10_drop_pack_tables").unwrap());

        let err = migrate_v10_inner(&conn, injected_failing_query)
            .expect_err("existence-check error must propagate, not collapse to absent");
        assert!(
            matches!(err, MemoryError::Sqlite(_)),
            "unexpected error shape: {err}"
        );

        assert!(
            !was_run(&conn, "v10_drop_pack_tables").unwrap(),
            "sentinel must stay unset after a failed existence-check so the migration retries"
        );
    }

    #[test]
    fn v11_propagates_existence_check_error_and_does_not_mark_sentinel() {
        let (conn, _tmp) = open_test_db();
        assert!(!was_run(&conn, "v11_drop_domains_table").unwrap());

        let err = migrate_v11_inner(&conn, injected_failing_query)
            .expect_err("existence-check error must propagate, not collapse to absent");
        assert!(
            matches!(err, MemoryError::Sqlite(_)),
            "unexpected error shape: {err}"
        );

        assert!(
            !was_run(&conn, "v11_drop_domains_table").unwrap(),
            "sentinel must stay unset after a failed existence-check so the migration retries"
        );
    }

    // The real-path (healthy DB, existence-check succeeds) behavior for
    // both migrations — success return value, tables actually dropped, and
    // sentinel written so a second run is a no-op — is already covered by
    // `v10_drops_legacy_pack_tables` and `v11_drops_legacy_domains_table`
    // above via the public `run_data_migrations` entry point; not
    // duplicated here.

    /// Proves `apply_versioned_migration` — the REAL helper
    /// `run_data_migrations` calls for v10/v11 — is the thing gating the
    /// sentinel, not a parallel test-only reimplementation of the gate
    /// (#978). A failing `migrate` closure must propagate the error AND
    /// leave the sentinel unset (so the migration retries on the next run);
    /// a subsequent succeeding call through the same helper must write the
    /// sentinel.
    #[test]
    fn apply_versioned_migration_gates_sentinel_on_real_runner_helper() {
        let (conn, _tmp) = open_test_db();
        let key = "v978_test_migration";
        assert!(!was_run(&conn, key).unwrap());

        let err = apply_versioned_migration(&conn, key, |_conn| {
            Err::<(), MemoryError>(
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("simulated migration failure (#978)".to_string()),
                )
                .into(),
            )
        })
        .expect_err("failing migrate closure must propagate through apply_versioned_migration");
        assert!(
            matches!(err, MemoryError::Sqlite(_)),
            "unexpected error shape: {err}"
        );
        assert!(
            !was_run(&conn, key).unwrap(),
            "sentinel must stay unset after apply_versioned_migration's migrate fails"
        );

        let ran = apply_versioned_migration(&conn, key, |_conn| Ok::<_, MemoryError>(42usize))
            .expect("succeeding migrate closure must return Ok");
        assert_eq!(ran, Some(42));
        assert!(
            was_run(&conn, key).unwrap(),
            "sentinel must be written after apply_versioned_migration's migrate succeeds"
        );

        // Idempotent: a second call with a closure that would panic if
        // invoked proves the sentinel-already-set path skips `migrate`
        // entirely.
        let skipped =
            apply_versioned_migration(&conn, key, |_conn| -> Result<usize, MemoryError> {
                panic!("migrate must not be invoked once the sentinel is already set")
            })
            .expect("already-run migration must short-circuit to Ok(None), not invoke migrate");
        assert_eq!(skipped, None);
    }
}
