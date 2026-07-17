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
//! [`EXPECTED_SCHEMA_VERSION`] counts the migration sequence above: 18
//! sentinel migrations (v1..v18) plus the pre-sentinel baseline schema (v0),
//! so the current stamp is 18. Bump this const (and add a `vN` doc line
//! above) whenever a new migration is appended to [`run_data_migrations`].
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
pub const EXPECTED_SCHEMA_VERSION: u32 = 20;

mod basic;
mod cross_db;
mod dispatch_adjudications;
mod dispatch_outcomes_attribution_basis;
mod dispatch_outcomes_identity_receipt;
mod dispatch_outcomes_reported;
mod domain_retire;
mod exec_env_class;
mod hard_state_index;
mod idless_identity;
mod legacy_columns;
mod mirror_eval;
mod pack_retire;
mod sentinel;
mod session_claims_identity;

use basic::*;
use cross_db::*;
use dispatch_adjudications::*;
use dispatch_outcomes_attribution_basis::*;
use dispatch_outcomes_identity_receipt::*;
use dispatch_outcomes_reported::*;
use domain_retire::*;
use exec_env_class::*;
use hard_state_index::*;
use idless_identity::*;
use legacy_columns::*;
pub use legacy_columns::{
    fold_and_drop_legacy_persons_column, migrate_v9_relocate_and_drop_location,
};
use mirror_eval::*;
use pack_retire::*;
use sentinel::*;
use session_claims_identity::*;

const MIGRATION_NS: &str = "migrations";
const SANITY_QUARANTINE_FRACTION: f64 = 0.5;

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
}

/// Read the schema version stamp (`PRAGMA user_version`). Absent/fresh DBs
/// read back `0`.
pub fn read_schema_version(conn: &Connection) -> Result<u32, MemoryError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(version.max(0) as u32)
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

    Ok(report)
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
        init_schema(&conn).expect("init_schema");
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
        assert!(!index_present(&conn, "idx_hard_state_ns_updated"));

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

        for key in ALL_MIGRATION_SENTINEL_KEYS {
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
    /// reads `user_version == 0` — exactly what `init_schema` / `open_test_db`
    /// produces — is a build, not a migration, under `CreateFresh`. Intent
    /// carries "I am creating", so no authority is needed and DB content is
    /// never consulted (owner ruling A: `init_schema`'s product IS fresh).
    #[test]
    fn create_fresh_needs_no_authority_regardless_of_db_content() {
        let (conn, tmp) = open_test_db();
        // Full tables present (open_test_db ran init_schema) but user_version==0.
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
    fn read_only_open_permits_older_stamped_db() {
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
        // Read-only opens never migrate; an older-stamped DB must still be
        // readable (only NEWER-than-supported is fatal).
        crate::MemoryStore::open_read_only(path)
            .expect("read-only open of an older-stamped DB must succeed");
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

    /// All sentinel keys `run_data_migrations` gates on, in the same order
    /// the runner checks them. Shared by the "genuine existing sentinels"
    /// fixture above and the invariant test below so both stay in lockstep
    /// with the runner's actual migration list.
    const ALL_MIGRATION_SENTINEL_KEYS: &[&str] = &[
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
    ];

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
    /// `ALL_MIGRATION_SENTINEL_KEYS` above is a separate, hand-maintained
    /// list used only to seed the "genuine existing sentinels" fixture; this
    /// test intentionally does not depend on it being complete or in sync.
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
