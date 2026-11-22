//! One-shot, idempotent schema/data migrations for memory DBs.
//!
//! Each migration is gated by a sentinel row in `hard_state`
//! (namespace='migrations', key=<migration_id>). Once written, the migration
//! is skipped on subsequent runs.
//!
//! ## Schema v39 epoch baseline
//!
//! Historical migrations `v1` through `v38` were squashed into
//! [`BASELINE_SCHEMA_VERSION`] (`39`), matching [`PORTABLE_COMPAT_FLOOR`].
//! Databases stamped with `0 < PRAGMA user_version < 39` fail closed at
//! [`check_schema_version_gate`] and must be bridged to `v39+` using an
//! archived pre-squash binary. Fresh empty databases (`user_version == 0`)
//! initialize the complete baseline schema directly and stamp
//! [`EXPECTED_SCHEMA_VERSION`] (`45` for Product; Portable remains `44`).
//!
//! Active shipped migrations:
//! - v39: four nullable, content-free mirror-eval identity/task metadata columns.
//! - v40: `session_claims.canonical_project` WorkClaim project binding.
//! - v41: host-owned `typed_records` and `typed_record_supersessions` tables (Portable).
//! - v42: `coordination/v1` scoped A2A mailbox (`canonical_project` on `identity_admissions` and `a2a_envelopes`).
//! - v43: adjudication provenance receipt & attribution basis columns +
//!   append-only trigger triples on `dispatch_adjudications` and
//!   `mirror_eval_adjudications` (#2012).
//! - v44: scoped erasure guards on typed revisions and lineage (Portable).
//! - v45: remote job rejection/replay headers and receipts in the Product A2A mailbox.
//!
//! ## Schema version stamp (#984)
//!
//! In addition to the sentinel-row idempotency above, the DB carries an
//! integer version stamp in `PRAGMA user_version`. This is a *coarse*
//! hard-fail gate, orthogonal to the fine-grained sentinel migrations: it
//! ensures both that a reader opening a DB written by a newer kernel fails
//! loudly and that pre-baseline (`< v39`) DBs are rejected rather than
//! silently half-migrated.
//!
//! [`EXPECTED_SCHEMA_VERSION`] and [`PORTABLE_EXPECTED_SCHEMA_VERSION`] are
//! derived from the single shipped catalogue. Append each new migration there
//! with its immutable scope, sentinel and body (and a `vN` doc line above).

use std::path::Path;

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

/// Minimum existing `PRAGMA user_version` supported by this kernel.
///
/// Migrations `v1-v38` are squashed into the `v39` epoch baseline; existing DBs
/// with `0 < user_version < 39` fail closed at [`check_schema_version_gate`]
/// and must be bridged via an archived pre-squash binary.
pub const BASELINE_SCHEMA_VERSION: u32 = 39;

/// Current schema version stamp, persisted via `PRAGMA user_version`.
///
/// See the module doc comment ("Schema version stamp (#984)") for what this
/// counts and when to bump it.
pub const EXPECTED_SCHEMA_VERSION: u32 = catalogue::expected_version(catalogue::MIGRATIONS);

/// Highest shipped migration touching the Portable kernel (D7).
pub const PORTABLE_EXPECTED_SCHEMA_VERSION: u32 =
    catalogue::portable_version(catalogue::MIGRATIONS);
/// Last schema stamp supported by binaries predating D7. Never advance this floor.
pub const PORTABLE_COMPAT_FLOOR: u32 = 39;

pub(crate) mod catalogue;

pub(crate) fn supported_schema_version() -> u32 {
    catalogue::expected_version(&catalogue::entries())
}

pub(crate) fn portable_schema_version() -> u32 {
    catalogue::portable_version(&catalogue::entries())
}

mod identity_workclaim_spine;
mod mirror_eval_identity;
mod sentinel;
mod symbolic_fts;

use mirror_eval_identity::*;
use sentinel::*;
pub use symbolic_fts::rebuild_memories_symbolic_fts;

const MIGRATION_NS: &str = "migrations";

/// Canonical sentinel inventory for a database stamped at
/// [`EXPECTED_SCHEMA_VERSION`]. A current stamp is a claim that every
/// migration completed; an absent sentinel is corruption, never permission to
/// rerun migration work during an ordinary same-version open.
pub(crate) const MIGRATION_SENTINEL_KEYS: &[&str] = catalogue::SENTINELS;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct MigrationReport {
    pub mirror_eval_identity_columns_added: usize,
    pub identity_workclaim_columns_added: usize,
}

/// Read the schema version stamp (`PRAGMA user_version`). Absent/fresh DBs
/// read back `0`.
pub fn read_schema_version(conn: &Connection) -> Result<u32, MemoryError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(version.max(0) as u32)
}

/// Inspect a database's version without creating or mutating it.
pub fn read_schema_version_at_path(db_path: &Path) -> Result<u32, MemoryError> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    read_schema_version(&conn)
}

/// Persist `version` as the schema version stamp (`PRAGMA user_version`).
///
/// `PRAGMA` statements don't accept bound parameters, so the value is
/// interpolated directly; it is always a `u32` we control (never
/// attacker-controlled input), so this is not a SQL-injection surface.
pub(crate) fn write_schema_version(conn: &Connection, version: u32) -> Result<(), MemoryError> {
    conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    Ok(())
}

/// Stamp the current [`EXPECTED_SCHEMA_VERSION`]. Exposed to `schema.rs` so
/// `init_schema_with_label_mut` can write the final stamp itself inside the
/// SAME outer transaction it opened for `init_schema_inner` and
/// [`run_data_migrations_in_tx`].
pub(crate) fn write_schema_version_stamp(conn: &Connection) -> Result<(), MemoryError> {
    write_schema_version(conn, supported_schema_version())
}

/// Hard-fail gate: refuse to open/operate on a DB stamped newer than this
/// kernel supports OR below [`BASELINE_SCHEMA_VERSION`] (`39`). Called at the
/// top of [`run_data_migrations`] and all DB open doors before any migration
/// or schema work touches the DB.
///
/// - `stored > EXPECTED_SCHEMA_VERSION` → hard error, never proceed.
/// - `0 < stored < BASELINE_SCHEMA_VERSION` → hard error instructing the
///   operator to bridge via an archived pre-squash binary.
/// - `stored == 0` or `BASELINE_SCHEMA_VERSION <= stored <= EXPECTED_SCHEMA_VERSION`
///   → caller proceeds through admission / migration / re-stamp.
pub fn check_schema_version_gate(conn: &Connection) -> Result<(), MemoryError> {
    let stored = read_schema_version(conn)?;
    let expected = supported_schema_version();
    if stored > expected {
        return Err(MemoryError::InvalidArg(format!(
            "db schema version {stored} newer than supported {expected}"
        )));
    }
    if (1..BASELINE_SCHEMA_VERSION).contains(&stored) {
        return Err(MemoryError::InvalidArg(format!(
            "db schema version {stored} is below minimum supported baseline {BASELINE_SCHEMA_VERSION}; \
             bridge it to v{BASELINE_SCHEMA_VERSION}+ with an archived binary before opening with this version"
        )));
    }
    Ok(())
}

/// Fail closed when a database claims the current schema version but lacks
/// evidence or persistent objects required by that claim. This is a read-only
/// preflight: callers run it before backup, connection PRAGMAs, transactions,
/// idempotent DDL, migration execution, or version stamping.
pub(crate) fn validate_current_schema_integrity(conn: &Connection) -> Result<(), MemoryError> {
    if read_schema_version(conn)? != supported_schema_version() {
        return Ok(());
    }
    validate_sentinels(conn, sentinel_keys().iter().copied())?;
    validate_current_schema_objects(conn)
}

fn sentinel_keys() -> std::borrow::Cow<'static, [&'static str]> {
    #[cfg(test)]
    if let Some(entries) = catalogue::test_support::override_entries() {
        return std::borrow::Cow::Owned(entries.iter().map(|entry| entry.sentinel).collect());
    }
    std::borrow::Cow::Borrowed(MIGRATION_SENTINEL_KEYS)
}

fn validate_sentinels<'a>(
    conn: &Connection,
    keys: impl Iterator<Item = &'a str>,
) -> Result<(), MemoryError> {
    for key in keys {
        if !was_run(conn, key)? {
            let expected = supported_schema_version();
            return Err(MemoryError::InvalidArg(format!(
                "incomplete current schema v{expected}: required migration sentinel '{key}' is missing"
            )));
        }
    }
    Ok(())
}

/// D7 band input and pending output share the exact existing object validators,
/// including the conditional Product branch when identity_admissions exists.
pub(crate) fn validate_portable_schema_integrity(
    conn: &Connection,
    stored: u32,
) -> Result<(), MemoryError> {
    let entries = catalogue::entries();
    validate_sentinels(
        conn,
        entries
            .iter()
            .filter(|migration| match migration.scope {
                catalogue::MigrationScope::Portable => migration.index <= portable_schema_version(),
                catalogue::MigrationScope::Product => {
                    migration.index <= stored.min(PORTABLE_COMPAT_FLOOR)
                }
            })
            .map(|migration| migration.sentinel),
    )?;
    validate_current_schema_objects(conn)
}

fn validate_current_schema_objects(conn: &Connection) -> Result<(), MemoryError> {
    crate::db::schema::validate_recall_impression_ledger_schema(conn)?;
    crate::db::schema::validate_typo_fallback_attribution_schema(conn)?;
    crate::db::schema::validate_wiki_recovery_ledgers_schema(conn)?;
    crate::db::schema::validate_memory_outbox_schema(conn)?;
    crate::db::schema::validate_memory_outbox_destination_apply_schema(conn)?;
    crate::db::schema::validate_harness_session_attachments_schema(conn)?;
    crate::db::schema::validate_harness_session_spine_schema(conn)?;
    crate::db::schema::validate_delivery_spine_schema(conn)?;
    let product_schema: i64 = conn.query_row(
        "SELECT COUNT(*) FROM main.sqlite_schema WHERE type='table' AND name='identity_admissions'",
        [],
        |row| row.get(0),
    )?;
    if product_schema > 0 {
        crate::db::schema::validate_a2a_mailbox_schema(conn)?;
        crate::db::schema::validate_mirror_eval_identity_schema(conn)?;
        crate::db::verified_admissions::validate_verified_admission_schema(conn)?;
        if supported_schema_version() >= 43 && read_schema_version(conn)? >= 43 {
            crate::db::adjudication_provenance::validate_adjudication_provenance_schema(conn)?;
        }
        crate::db::schema::validate_current_truth_schema(conn)?;
    }
    Ok(())
}

/// #1119 typed schema-migration gate. Runs at the DB-open funnel
/// (`schema::init_schema_with_label_mut`), AFTER [`check_schema_version_gate`]
/// (which owns the `stored > EXPECTED` and `0 < stored < BASELINE` refusals
/// and is called first by every entry point) and BEFORE any DDL / data
/// migration / version stamp mutates the DB. Decides purely from the caller's
/// typed [`DbOpenContext`] and the stored `PRAGMA user_version` and the
/// read-only profile stamp probe — never inferred from table contents or
/// process env.
pub fn check_db_open_context_gate(
    conn: &Connection,
    db_path: &Path,
    ctx: &crate::db::DbOpenContext,
) -> Result<(), MemoryError> {
    let decision = evaluate_db_open_context_gate(conn, db_path, ctx)?;
    log_authorized_migration(
        decision,
        db_path,
        ctx,
        crate::db::version_policy::VersionHeader::read(conn)?,
    );
    Ok(())
}

/// What [`check_db_open_context_gate`] admitted, for callers that must act on
/// (or log) the decision separately from evaluating it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenContextDecision {
    /// `stored == 0`: a build under either intent.
    Build,
    /// `OpenExisting` of a `stored == EXPECTED` database.
    Current,
    /// `OpenExisting + Allow` of a `BASELINE_SCHEMA_VERSION <= stored < EXPECTED` database.
    AuthorizedMigration { stored: u32 },
}

/// The side-effect-free core of [`check_db_open_context_gate`]: the same
/// decision table, without the audit log line. The open funnel evaluates it
/// twice (a pre-transaction preflight and the authoritative in-transaction
/// re-evaluation) and logs only the authoritative one.
pub(crate) fn evaluate_db_open_context_gate(
    conn: &Connection,
    db_path: &Path,
    ctx: &crate::db::DbOpenContext,
) -> Result<OpenContextDecision, MemoryError> {
    use crate::db::{MigrationAuthority, OpenIntent};

    let header = crate::db::version_policy::VersionHeader::read(conn)?;
    let stored = header.stored;
    match ctx.intent {
        OpenIntent::CreateFresh => {
            if stored == 0 {
                Ok(OpenContextDecision::Build)
            } else {
                Err(MemoryError::DbCreateTargetExists {
                    stored,
                    db_path: db_path.display().to_string(),
                })
            }
        }
        OpenIntent::OpenExisting => {
            if stored >= header.projection() {
                // Current profile band; newer is refused upstream.
                return Ok(OpenContextDecision::Current);
            }
            if stored == 0 {
                // No stamp: a build, not a migration.
                return Ok(OpenContextDecision::Build);
            }
            // BASELINE <= stored < EXPECTED: a real older DB.
            match &ctx.migration {
                MigrationAuthority::Allow { .. } => {
                    Ok(OpenContextDecision::AuthorizedMigration { stored })
                }
                MigrationAuthority::Deny => {
                    let target =
                        if header.probe == crate::db::version_policy::ProfileProbe::Portable {
                            crate::db::version_policy::portable_output_stamp(stored)
                        } else {
                            return Err(schema_migration_opt_in_required_error(stored, db_path));
                        };
                    Err(schema_migration_opt_in_required_error_for_target(
                        stored, db_path, target,
                    ))
                }
            }
        }
    }
}

/// Emit the #1119 audit line for an authorized migration decision; a no-op for
/// every other decision.
pub(crate) fn log_authorized_migration(
    decision: OpenContextDecision,
    db_path: &Path,
    ctx: &crate::db::DbOpenContext,
    header: crate::db::version_policy::VersionHeader,
) {
    if let (OpenContextDecision::AuthorizedMigration { stored }, Some(approved_by)) =
        (decision, ctx.approved_by())
    {
        let target = if header.probe == crate::db::version_policy::ProfileProbe::Portable {
            crate::db::version_policy::portable_output_stamp(stored)
        } else {
            supported_schema_version()
        };
        let line = if target == supported_schema_version() {
            schema_migration_success_log_line(stored, db_path, approved_by)
        } else {
            schema_migration_log_line_for_target(stored, target, db_path, approved_by)
        };
        eprintln!("{line}");
    }
}

fn schema_migration_success_log_line(stored: u32, db_path: &Path, approved_by: &str) -> String {
    schema_migration_log_line_for_target(stored, supported_schema_version(), db_path, approved_by)
}

fn schema_migration_log_line_for_target(
    stored: u32,
    target: u32,
    db_path: &Path,
    approved_by: &str,
) -> String {
    format!(
        "[migration] authorized by {approved_by}: binary={} pid={} migrating db={} schema {stored} -> {target}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        db_path.display()
    )
}

/// Build the typed [`MemoryError::SchemaMigrationOptInRequired`] refusal.
fn schema_migration_opt_in_required_error(stored: u32, db_path: &Path) -> MemoryError {
    schema_migration_opt_in_required_error_for_target(stored, db_path, supported_schema_version())
}

fn schema_migration_opt_in_required_error_for_target(
    stored: u32,
    db_path: &Path,
    expected: u32,
) -> MemoryError {
    let db_path_str = db_path.display().to_string();
    MemoryError::SchemaMigrationOptInRequired {
        stored,
        expected,
        db_path: db_path_str.clone(),
        backup_hint: format!("{db_path_str}.migration-bak.<UTC-timestamp-of-this-attempt>"),
        marker_hint: format!("{db_path_str}.migration-marker"),
    }
}

/// Run all data-fix migrations in order. Idempotent.
///
/// Enforces the [`EXPECTED_SCHEMA_VERSION`] and [`BASELINE_SCHEMA_VERSION`]
/// hard-fail gate on entry and stamps the current version on successful exit.
pub fn run_data_migrations(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<MigrationReport, MemoryError> {
    run_data_migrations_with_profile(conn, db_label, current_db_path, StoreProfile::default())
}

/// [`run_data_migrations`] for a caller that knows the store's effective
/// profile (#1585 D3).
pub fn run_data_migrations_with_profile(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
    profile: StoreProfile,
) -> Result<MigrationReport, MemoryError> {
    check_schema_version_gate(conn)?;
    validate_current_schema_integrity(conn)?;

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let report = run_data_migrations_in_tx(&tx, db_label, current_db_path, profile)?;
    write_schema_version_stamp(&tx)?;
    tx.commit()?;

    Ok(report)
}

/// The actual migration sequence, run against an already-open transaction.
pub(crate) fn run_data_migrations_in_tx(
    conn: &Connection,
    db_label: &str,
    current_db_path: &Path,
    profile: StoreProfile,
) -> Result<MigrationReport, MemoryError> {
    run_migration_catalogue(conn, db_label, current_db_path, profile, false)
}

/// D7's open funnel runs only Portable bodies, with Product sentinels at or
/// below the frozen rollback floor recorded vacuously.
pub(crate) fn run_projected_migrations_in_tx(
    conn: &Connection,
    db_label: &str,
    path: &Path,
    profile: StoreProfile,
) -> Result<MigrationReport, MemoryError> {
    run_migration_catalogue(
        conn,
        db_label,
        path,
        profile,
        profile == StoreProfile::PortableKernel,
    )
}

fn run_migration_catalogue(
    conn: &Connection,
    db_label: &str,
    path: &Path,
    profile: StoreProfile,
    projected: bool,
) -> Result<MigrationReport, MemoryError> {
    let context = catalogue::MigrationContext {
        db_label,
        path,
        profile,
    };
    let mut report = MigrationReport::default();
    for migration in catalogue::entries().iter() {
        if projected && migration.scope == catalogue::MigrationScope::Product {
            if migration.index <= PORTABLE_COMPAT_FLOOR.min(supported_schema_version()) {
                apply_versioned_migration(conn, migration.sentinel, |_| Ok(()))?;
            }
            continue;
        }
        apply_versioned_migration(conn, migration.sentinel, |conn| {
            #[cfg(test)]
            catalogue::test_support::record_invocation();
            (migration.run)(conn, &context, &mut report)
        })?;
    }
    Ok(report)
}

/// Run a single sentinel-gated migration: skip if `key`'s sentinel is
/// already set, otherwise run `migrate`, and — only on success — mark the
/// sentinel run.
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
    use crate::db::{init_schema, register_sqlite_vec, try_load_sqlite_vec, DbOpenContext};
    use rusqlite::{params, Connection};

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

    fn index_present(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1",
            params![name],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn trigger_present(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            params![name],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn table_has_column(conn: &Connection, table: &str, column: &str) -> bool {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let found = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .iter()
            .any(|name| name == column);
        found
    }

    #[test]
    fn was_run_mark_run_roundtrip() {
        let (conn, _tmp) = open_test_db();
        assert!(!was_run(&conn, "v39_test").unwrap());
        mark_run(&conn, "v39_test").unwrap();
        assert!(was_run(&conn, "v39_test").unwrap());
        mark_run(&conn, "v39_test").unwrap();
        assert!(was_run(&conn, "v39_test").unwrap());
    }

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
    fn schema_version_gate_and_all_open_doors_refuse_pre_baseline_versions() {
        for legacy_version in [1_u32, 22, 28, 36, 38] {
            let tmp = tempfile::NamedTempFile::new().expect("tempfile");
            let path = tmp.path().to_path_buf();
            let path_str = path.to_str().expect("utf8 path");
            {
                let store =
                    crate::MemoryStore::open_with_context(path_str, &DbOpenContext::create_fresh())
                        .expect("provision fixture");
                drop(store);
                let conn = Connection::open(&path).expect("open offline");
                write_schema_version(&conn, legacy_version).unwrap();
            }
            let before = std::fs::read(&path).unwrap();

            let conn = Connection::open(&path).unwrap();
            let err = check_schema_version_gate(&conn)
                .expect_err("pre-baseline version must fail closed");
            let msg = err.to_string();
            assert!(
                msg.contains(&format!("db schema version {legacy_version} is below minimum supported baseline {BASELINE_SCHEMA_VERSION}"))
                    && msg.contains("archived binary"),
                "pre-baseline error must guide operator to bridge via archived binary: {msg}"
            );
            drop(conn);

            for ctx in [
                DbOpenContext::open_existing_deny(),
                DbOpenContext::open_existing_allow("test:pre-baseline-refusal"),
                DbOpenContext::create_fresh(),
            ] {
                let err = match crate::MemoryStore::open_with_context(path_str, &ctx) {
                    Ok(_) => panic!("open_with_context must refuse v{legacy_version}"),
                    Err(err) => err,
                };
                assert!(
                    err.to_string().contains("archived binary"),
                    "open_with_context refusal for v{legacy_version}: {err}"
                );
            }

            let ro_err = match crate::MemoryStore::open_read_only(path_str) {
                Ok(_) => panic!("open_read_only must refuse v{legacy_version}"),
                Err(err) => err,
            };
            assert!(
                ro_err.to_string().contains("archived binary"),
                "open_read_only refusal for v{legacy_version}: {ro_err}"
            );

            let rw_err = match crate::MemoryStore::open_existing_read_write(path_str) {
                Ok(_) => panic!("open_existing_read_write must refuse v{legacy_version}"),
                Err(err) => err,
            };
            assert!(
                rw_err.to_string().contains("archived binary"),
                "open_existing_read_write refusal for v{legacy_version}: {rw_err}"
            );

            assert!(matches!(
                crate::store_version_status(&path, StoreProfile::default().into()),
                crate::StoreVersionStatus::Refused(MemoryError::InvalidArg(ref m))
                    if m.contains("archived binary")
            ));

            let mut mut_conn = Connection::open(&path).unwrap();
            let mig_err = run_data_migrations(&mut mut_conn, "global", &path)
                .expect_err("run_data_migrations must refuse pre-baseline");
            assert!(mig_err.to_string().contains("archived binary"));
            drop(mut_conn);

            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "refused pre-baseline open must leave v{legacy_version} bytes untouched"
            );
        }
    }

    #[test]
    fn fresh_db_migrates_and_ends_stamped_at_expected_version() {
        let (mut conn, tmp) = open_test_db();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);

        run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    #[test]
    fn fresh_init_installs_baseline_and_shipped_sentinels_once() {
        let _ = crate::db::enable_simple_auto_extension();
        register_sqlite_vec();
        let conn = Connection::open_in_memory().expect("open in-memory");
        let _ = try_load_sqlite_vec(&conn);

        init_schema(&conn).expect("initialize current private schema");
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
        let sentinel_version = |key: &str| {
            conn.query_row(
                "SELECT version FROM hard_state WHERE namespace = ?1 AND key = ?2",
                params![MIGRATION_NS, key],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        for key in MIGRATION_SENTINEL_KEYS {
            assert_eq!(sentinel_version(key), 1, "sentinel {key} must be written");
        }
        crate::db::schema::validate_recall_impression_ledger_schema(&conn).unwrap();
        crate::db::schema::validate_typo_fallback_attribution_schema(&conn).unwrap();
        crate::db::schema::validate_wiki_recovery_ledgers_schema(&conn).unwrap();
        crate::db::schema::validate_memory_outbox_schema(&conn).unwrap();
        crate::db::schema::validate_memory_outbox_destination_apply_schema(&conn).unwrap();
        crate::db::schema::validate_harness_session_attachments_schema(&conn).unwrap();
        crate::db::schema::validate_harness_session_spine_schema(&conn).unwrap();
        crate::db::schema::validate_delivery_spine_schema(&conn).unwrap();
        crate::db::schema::validate_a2a_mailbox_schema(&conn).unwrap();
        crate::db::schema::validate_mirror_eval_identity_schema(&conn).unwrap();
        crate::db::verified_admissions::validate_verified_admission_schema(&conn).unwrap();
        crate::db::schema::validate_current_truth_schema(&conn).unwrap();
        crate::db::adjudication_provenance::validate_adjudication_provenance_schema(&conn).unwrap();

        init_schema(&conn).expect("valid current schema reopens idempotently");
        for key in MIGRATION_SENTINEL_KEYS {
            assert_eq!(sentinel_version(key), 1, "{key} must run once");
        }
    }

    #[test]
    fn older_stamped_db_migrates_forward_and_re_stamps() {
        let (mut conn, tmp) = open_test_db();
        mark_run(&conn, "v39_mirror_eval_identity").unwrap();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();

        assert_eq!(report.mirror_eval_identity_columns_added, 0);
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    #[test]
    fn v39_to_current_upgrades_cleanly_preserving_data() {
        let v39_dir = tempfile::tempdir().unwrap();
        let v39_path = v39_dir.path().join("v39.db");
        let v39_str = v39_path.to_str().unwrap();
        catalogue::test_support::with_prefix(39, || {
            let mut store =
                crate::MemoryStore::open_with_context(v39_str, &DbOpenContext::create_fresh())
                    .unwrap();
            let entry: crate::MemoryEntry = serde_json::from_value(serde_json::json!({
                "id": "v39-note",
                "text": "preserved across v39 to v43",
                "timestamp": "2026-10-01T00:00:00Z"
            }))
            .unwrap();
            store.upsert(&entry).unwrap();
            assert_eq!(read_schema_version(store.connection()).unwrap(), 39);
        });

        let denied =
            crate::MemoryStore::open_with_context(v39_str, &DbOpenContext::open_existing_deny());
        assert!(matches!(
            denied,
            Err(MemoryError::SchemaMigrationOptInRequired {
                stored: 39,
                expected,
                ..
            }) if expected == EXPECTED_SCHEMA_VERSION
        ));

        let upgraded = crate::MemoryStore::open_with_context(
            v39_str,
            &DbOpenContext::open_existing_allow("test:v39-to-current"),
        )
        .expect("Allow must upgrade v39 -> current");
        assert_eq!(
            read_schema_version(upgraded.connection()).unwrap(),
            EXPECTED_SCHEMA_VERSION
        );
        assert_eq!(
            upgraded.get("v39-note").unwrap().unwrap().text,
            "preserved across v39 to v43"
        );
        assert!(table_has_column(
            upgraded.connection(),
            "session_claims",
            "canonical_project"
        ));
    }

    #[test]
    fn stamped_current_v33_missing_attachment_index_fails_closed_without_repair() {
        let (mut conn, tmp) = open_test_db();
        run_data_migrations(&mut conn, "global", tmp.path()).expect("current fixture");
        conn.execute("DROP INDEX idx_harness_session_attachments_claim", [])
            .unwrap();

        let error = validate_current_schema_integrity(&conn)
            .expect_err("current schema with a missing v33 index must refuse");
        assert!(error
            .to_string()
            .contains("idx_harness_session_attachments_claim"));
        assert!(!index_present(
            &conn,
            "idx_harness_session_attachments_claim"
        ));
    }

    #[test]
    fn stamped_current_v37_missing_append_only_trigger_fails_closed_without_repair() {
        let (mut conn, tmp) = open_test_db();
        run_data_migrations(&mut conn, "global", tmp.path()).expect("current fixture");
        conn.execute("DROP TRIGGER identity_verification_receipts_no_update", [])
            .unwrap();

        let error = validate_current_schema_integrity(&conn)
            .expect_err("current schema without a v37 append-only trigger must refuse");
        assert!(error
            .to_string()
            .contains("identity_verification_receipts_no_update"));
        assert!(!trigger_present(
            &conn,
            "identity_verification_receipts_no_update"
        ));
    }

    #[test]
    fn unstamped_db_with_existing_sentinels_skips_and_restamps() {
        let (mut conn, tmp) = open_test_db();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);

        for key in MIGRATION_SENTINEL_KEYS {
            assert!(!was_run(&conn, key).unwrap(), "sentinel {key} pre-seeded?");
            mark_run(&conn, key).unwrap();
        }

        let report = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report.mirror_eval_identity_columns_added, 0);
        assert_eq!(report.identity_workclaim_columns_added, 0);
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    #[test]
    fn gate_is_checked_before_any_migration_runs() {
        let (mut conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION + 5).unwrap();

        let result = crate::db::init_schema_with_label_mut(
            &mut conn,
            "global",
            tmp.path(),
            &DbOpenContext::create_fresh(),
        );
        assert!(
            result.is_err(),
            "gate must reject via the schema.rs entry point too"
        );
    }

    // --- #1119: typed DbOpenContext migration gate ------------------------

    fn is_opt_in_required(result: Result<(), MemoryError>) -> bool {
        matches!(
            result,
            Err(MemoryError::SchemaMigrationOptInRequired { .. })
        )
    }

    fn is_create_target_exists(result: Result<(), MemoryError>) -> bool {
        matches!(result, Err(MemoryError::DbCreateTargetExists { .. }))
    }

    #[cfg(feature = "admin")]
    #[test]
    fn opt_in_required_remediation_text_points_admin_builds_at_tachi_server() {
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(
                crate::ADMIN_SURFACE_ENABLED,
                "this test pins the admin branch; the portable branch is pinned in \
                 portable-kernel's portable_contract test"
            );
        }
        let err = schema_migration_opt_in_required_error(39, Path::new("/data/legacy.db"));
        let text = err.to_string();
        assert!(
            text.contains("refusing to migrate db schema 39 -> "),
            "skeleton must survive the hint split: {text}"
        );
        assert!(
            text.contains(
                "pass --allow-schema-migration to tachi-server, which becomes a typed \
                 MigrationAuthority::Allow threaded to every DB open (never a process env var)"
            ),
            "admin builds keep the tachi-server deploy-ritual guidance: {text}"
        );
        assert!(
            !text.contains("hypermem migrate"),
            "admin builds must not carry the portable-shell remediation: {text}"
        );
        assert!(
            text.contains("/data/legacy.db.migration-bak.")
                && text.contains("/data/legacy.db.migration-marker"),
            "backup/marker trail hints must survive the hint split: {text}"
        );
    }

    #[test]
    fn create_fresh_needs_no_authority_regardless_of_db_content() {
        let (conn, tmp) = open_test_db();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);
        let ctx = DbOpenContext::create_fresh();
        check_db_open_context_gate(&conn, tmp.path(), &ctx)
            .expect("CreateFresh builds a fresh (v0) DB with no authority");
    }

    #[test]
    fn open_existing_deny_allows_fresh_zero_stamped_db() {
        let (conn, tmp) = open_test_db();
        let ctx = DbOpenContext::open_existing_deny();
        check_db_open_context_gate(&conn, tmp.path(), &ctx)
            .expect("a v0 file has no deployed daemon to strand");
    }

    #[test]
    fn open_existing_deny_refuses_stamped_older_db() {
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
        let (conn, tmp) = open_test_db();
        write_schema_version(&conn, EXPECTED_SCHEMA_VERSION - 1).unwrap();
        let ctx = DbOpenContext::create_fresh();
        assert!(
            is_create_target_exists(check_db_open_context_gate(&conn, tmp.path(), &ctx)),
            "CreateFresh must refuse a stamped older DB as already-existing"
        );
    }

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

    #[test]
    fn stamp_and_migration_effects_roll_back_together() {
        let (mut conn, tmp) = open_test_db();
        // Remove v40 column/sentinels so v40 has real DDL work to perform and roll back.
        conn.execute(
            "DELETE FROM hard_state WHERE namespace = ?1 AND key IN ('v40_work_claim_project_binding', 'v41_host_typed_records', 'v42_coordination_mailbox', 'v43_adjudication_provenance', 'v44_typed_record_erasure', 'v45_remote_job_mailbox')",
            params![MIGRATION_NS],
        )
        .unwrap();
        write_schema_version(&conn, 39).unwrap();
        assert!(!table_has_column(
            &conn,
            "session_claims",
            "canonical_project"
        ));

        {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            let report =
                run_data_migrations_in_tx(&tx, "global", tmp.path(), StoreProfile::TachiFull)
                    .unwrap();
            assert_eq!(report.identity_workclaim_columns_added, 1);
            write_schema_version(&tx, EXPECTED_SCHEMA_VERSION).unwrap();
            tx.rollback().unwrap();
        }

        assert_eq!(
            read_schema_version(&conn).unwrap(),
            39,
            "version stamp must roll back with the migration effects"
        );
        assert!(
            !was_run(&conn, "v40_work_claim_project_binding").unwrap(),
            "sentinel must roll back with the migration effects"
        );
        assert!(
            !table_has_column(&conn, "session_claims", "canonical_project"),
            "v40 DDL must roll back with the version stamp"
        );

        let report2 = run_data_migrations(&mut conn, "global", tmp.path()).unwrap();
        assert_eq!(report2.identity_workclaim_columns_added, 1);
        assert_eq!(read_schema_version(&conn).unwrap(), EXPECTED_SCHEMA_VERSION);
    }

    // --- #984 F3(e): EXPECTED_SCHEMA_VERSION invariant ----------------------

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

        let expected_sentinel_count = EXPECTED_SCHEMA_VERSION - BASELINE_SCHEMA_VERSION + 1;
        assert_eq!(
            sentinel_count, expected_sentinel_count,
            "EXPECTED_SCHEMA_VERSION ({EXPECTED_SCHEMA_VERSION}) - BASELINE_SCHEMA_VERSION ({BASELINE_SCHEMA_VERSION}) + 1 must equal sentinel count ({sentinel_count})"
        );
        assert_eq!(
            MIGRATION_SENTINEL_KEYS.len(),
            expected_sentinel_count as usize,
            "current-schema sentinel inventory must cover every post-baseline version"
        );
        for key in MIGRATION_SENTINEL_KEYS {
            assert!(
                was_run(&conn, key).unwrap(),
                "current-schema preflight key '{key}' is not written by the migration runner"
            );
        }
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

    fn assert_current_corruption_is_not_repaired(corruption_sql: &str, expected: &str) {
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
                .expect("provision current fixture");
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
            assert!(
                after.0 == before.0,
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
            "stamped-current corrupt DB was accepted by {unexpected_acceptances:?}"
        );
    }

    #[test]
    fn stamped_current_missing_ledger_table_or_index_is_refused_without_repair() {
        assert_current_corruption_is_not_repaired(
            "DROP TABLE recall_impressions;",
            "recall_impressions",
        );
        assert_current_corruption_is_not_repaired(
            "DROP INDEX idx_recall_impression_groups_fingerprint;",
            "idx_recall_impression_groups_fingerprint",
        );
    }

    #[test]
    fn stamped_current_malformed_replay_identity_is_refused_without_repair() {
        assert_current_corruption_is_not_repaired(
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
    fn stamped_current_missing_wiki_recovery_objects_is_refused_without_repair() {
        assert_current_corruption_is_not_repaired(
            "DROP TABLE rem_source_claims;",
            "rem_source_claims",
        );
        assert_current_corruption_is_not_repaired(
            "DROP INDEX idx_rem_source_claims_draft;",
            "idx_rem_source_claims_draft",
        );
        assert_current_corruption_is_not_repaired(
            "DROP TABLE exact_dedupe_apply_lineage;",
            "exact_dedupe_apply_lineage",
        );
    }

    #[test]
    fn stamped_current_malformed_wiki_recovery_shapes_are_refused_without_repair() {
        assert_current_corruption_is_not_repaired(
            "DROP TABLE rem_source_claims;
             CREATE TABLE rem_source_claims (
                 source_key TEXT PRIMARY KEY NOT NULL,
                 draft_id TEXT NOT NULL,
                 claimed_at TEXT NOT NULL
             );
             CREATE INDEX idx_rem_source_claims_draft
                 ON rem_source_claims(draft_id);",
            "rem_source_claims' has non-canonical column shape",
        );
        assert_current_corruption_is_not_repaired(
            "DROP TABLE exact_dedupe_apply_lineage;
             CREATE TABLE exact_dedupe_apply_lineage (
                 loser_id TEXT NOT NULL,
                 apply_id TEXT NOT NULL,
                 plan_digest TEXT NOT NULL,
                 winner_id TEXT NOT NULL,
                 before_revision INTEGER NOT NULL,
                 archived_revision INTEGER NOT NULL,
                 applied_at TEXT NOT NULL
             );",
            "exact_dedupe_apply_lineage' has non-canonical column shape",
        );
        assert_current_corruption_is_not_repaired(
            "DROP INDEX idx_rem_source_claims_draft;
             CREATE INDEX idx_rem_source_claims_draft
                 ON rem_source_claims(source_identity);",
            "idx_rem_source_claims_draft' has non-canonical shape",
        );
    }

    #[test]
    fn stamped_current_missing_v39_sentinel_is_refused_without_repair() {
        assert_current_corruption_is_not_repaired(
            "DELETE FROM hard_state
             WHERE namespace = 'migrations' AND key = 'v39_mirror_eval_identity';",
            "v39_mirror_eval_identity",
        );
    }

    #[test]
    fn stamped_current_missing_each_typo_fallback_column_is_refused_without_repair() {
        for (table, column) in [
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
        ] {
            assert_current_corruption_is_not_repaired(
                &format!("ALTER TABLE {table} DROP COLUMN {column};"),
                &format!("{table}.{column}"),
            );
        }
    }

    #[test]
    fn valid_stamped_current_reopens_on_all_existing_surfaces() {
        for (surface, open) in current_existing_openers() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join(format!("{surface}.db"));
            let path_str = path.to_string_lossy().to_string();
            let store =
                crate::MemoryStore::open_with_context(&path_str, &DbOpenContext::create_fresh())
                    .expect("provision current fixture");
            drop(store);
            let before = current_schema_snapshot(&path);

            let reopened = open(&path_str)
                .unwrap_or_else(|error| panic!("valid current {surface} reopen: {error}"));
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
    fn stamped_v39_with_missing_guards_is_refused_even_with_migration_authority() {
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
                 PRAGMA user_version = 39;",
            )
            .unwrap();
        }

        let err = match crate::MemoryStore::open_with_label_and_context(
            &path_str,
            "guide-project",
            &DbOpenContext::open_existing_allow("test:must-not-repair-v39"),
        ) {
            Ok(_) => panic!("a damaged v39 DB must not be repaired during open"),
            Err(error) => error,
        };
        let message = err.to_string();
        assert!(
            message.contains("unsafe persistent trigger inventory")
                && message.contains("memories_reserved_refs_insert_guard"),
            "damaged v39 DB must fail as unsafe inventory, got: {err}"
        );

        let verify = Connection::open(&path).expect("verify refused DB");
        assert_eq!(read_schema_version(&verify).unwrap(), 39);
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
            assert_eq!(
                guard_trigger_count, 2,
                "fixture keeps baseline guards intact"
            );
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

        let skipped =
            apply_versioned_migration(&conn, key, |_conn| -> Result<usize, MemoryError> {
                panic!("migrate must not be invoked once the sentinel is already set")
            })
            .expect("already-run migration must short-circuit to Ok(None), not invoke migrate");
        assert_eq!(skipped, None);
    }
}
