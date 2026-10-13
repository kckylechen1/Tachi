//! Test-only pinned pre-D7 policy fixture from main 9ee323134123f8d48c267ed81681113951804502.
//! Gate, integrity, backup and transaction order are copied from that object.
//! Unchanged maintenance, identity and object validators are shared, rather
//! than duplicated. This fixture runs through the real generic/private doors.
//! Mechanical adaptations: qualification, scoped expected version, local
//! backup receipt, and test phase receipt. Never enabled in production.

use super::*;
use crate::db::migrations::{read_schema_version, OpenContextDecision};
use std::cell::Cell;

thread_local! {
    static EXPECTED: Cell<Option<u32>> = const { Cell::new(None) };
    static TRANSACTION_ENTERED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn active_version() -> Option<u32> {
    EXPECTED.with(Cell::get)
}
fn expected() -> u32 {
    active_version().expect("scoped pinned pre-B fixture")
}
pub(crate) fn transaction_entered() -> bool {
    TRANSACTION_ENTERED.with(Cell::get)
}
pub(crate) fn with_policy<T>(version: u32, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<u32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            EXPECTED.with(|v| v.set(self.0));
        }
    }
    crate::db::migrations::catalogue::test_support::with_prefix(version, || {
        let _restore = Restore(EXPECTED.with(|v| v.replace(Some(version))));
        TRANSACTION_ENTERED.with(|v| v.set(false));
        run()
    })
}

struct BackupDecision {
    version: u32,
    outcome: BackupOutcome,
}
impl BackupDecision {
    fn covers_migration_of(&self, version: u32) -> bool {
        self.version == version && !matches!(self.outcome, BackupOutcome::SkippedMarkerMatch)
    }
}

fn schema_migration_opt_in_required_error(stored: u32, db_path: &Path) -> MemoryError {
    MemoryError::SchemaMigrationOptInRequired {
        stored,
        expected: expected(),
        db_path: db_path.display().to_string(),
        backup_hint: format!(
            "{}.migration-bak.<UTC-timestamp-of-this-attempt>",
            db_path.display()
        ),
        marker_hint: format!("{}.migration-marker", db_path.display()),
    }
}

fn was_run(conn: &Connection, key: &str) -> Result<bool, MemoryError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM hard_state WHERE namespace='migrations' AND key=?1)",
        [key],
        |row| row.get(0),
    )?)
}

fn validate_presence(
    conn: &Connection,
    path: &Path,
    requirement: crate::db::ProfileRequirement,
) -> Result<(), MemoryError> {
    if read_schema_version(conn)? != expected() {
        return Ok(());
    }
    current_store_admission::validate_current_schema_presence(conn, path, requirement)
}

const MIGRATION_SENTINEL_KEYS: &[&str] = &[
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
    "v27_typo_fallback_attribution",
    "v28_wiki_recovery_ledgers",
    "v29_memory_outbox",
    "v30_memory_outbox_destination_apply",
    "v31_a2a_mailbox",
    "v32_a2a_body_retention",
    "v33_harness_session_attachments",
    "v34_harness_session_spine",
    "v35_harness_session_spine_receipts",
    "v36_delivery_spine",
    "v37_verified_agent_admissions",
    "v38_current_truth",
    "v39_mirror_eval_identity",
];

pub fn check_schema_version_gate(conn: &Connection) -> Result<(), MemoryError> {
    let stored = read_schema_version(conn)?;
    if stored > expected() {
        return Err(MemoryError::InvalidArg(format!(
            "db schema version {stored} newer than supported {}",
            expected()
        )));
    }
    Ok(())
}

pub(crate) fn validate_current_schema_integrity(conn: &Connection) -> Result<(), MemoryError> {
    if read_schema_version(conn)? != expected() {
        return Ok(());
    }

    for key in MIGRATION_SENTINEL_KEYS {
        if !was_run(conn, key)? {
            return Err(MemoryError::InvalidArg(format!(
                "incomplete current schema v{}: required migration sentinel '{key}' is missing",
                expected()
            )));
        }
    }
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
        crate::db::schema::validate_current_truth_schema(conn)?;
    }
    Ok(())
}

pub(crate) fn evaluate_db_open_context_gate(
    conn: &Connection,
    db_path: &Path,
    ctx: &crate::db::DbOpenContext,
) -> Result<OpenContextDecision, MemoryError> {
    use crate::db::{MigrationAuthority, OpenIntent};

    let stored = read_schema_version(conn)?;
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
            if stored >= expected() {
                return Ok(OpenContextDecision::Current);
            }
            if stored == 0 {
                return Ok(OpenContextDecision::Build);
            }
            match &ctx.migration {
                MigrationAuthority::Allow { .. } => {
                    Ok(OpenContextDecision::AuthorizedMigration { stored })
                }
                MigrationAuthority::Deny => {
                    Err(schema_migration_opt_in_required_error(stored, db_path))
                }
            }
        }
    }
}

pub(super) fn init_schema_with_label_mut_inner(
    conn: &mut Connection,
    db_label: &str,
    current_db_path: &Path,
    ctx: &crate::db::DbOpenContext,
    funnel: SchemaInitFunnel<'_>,
) -> Result<SchemaInitOutcome, MemoryError> {
    crate::db::ensure_reserved_reference_write_guard(conn)?;
    check_schema_version_gate(conn)?;
    let preflight_version = crate::db::migrations::read_schema_version(conn)?;
    evaluate_db_open_context_gate(conn, current_db_path, ctx)?;
    validate_current_schema_integrity(conn)?;
    validate_presence(conn, current_db_path, ctx.required_profile)?;
    let preflight_fresh = preflight_version == 0;
    resolve_store_identity_in_tx(conn, db_label, current_db_path, ctx, preflight_fresh)?;
    funnel.admit_private_partition_stamp(conn)?;
    #[cfg(test)]
    test_hooks::run_window_hook(test_hooks::Window::BeforeBackupDecision, current_db_path);
    let backup_decision = if funnel.writes_filesystem_artifacts() {
        Some(maybe_backup_before_migration(conn, current_db_path)?)
    } else {
        None
    };
    apply_connection_pragmas(conn)?;

    #[cfg(test)]
    test_hooks::run_window_hook(test_hooks::Window::BeforeSchemaTransaction, current_db_path);
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    TRANSACTION_ENTERED.with(|v| v.set(true));
    let fresh = reevaluate_admission_in_tx(
        &tx,
        current_db_path,
        ctx,
        backup_decision.as_ref(),
        funnel.validates_input_trigger_inventory(),
    )?;
    let identity = resolve_store_identity_in_tx(&tx, db_label, current_db_path, ctx, fresh)?;
    funnel.admit_private_partition_stamp(&tx)?;
    init_schema_inner(&tx, identity.profile)?;
    stamp_store_identity_in_tx(&tx, &identity, ctx)?;
    #[cfg(test)]
    test_hooks::fail_after_legacy_work_before_stamp()?;
    let report = crate::db::migrations::run_data_migrations_in_tx(
        &tx,
        &identity.db_label,
        current_db_path,
        identity.profile,
    )?;
    crate::db::migrations::write_schema_version_stamp(&tx)?;
    crate::db::validate_persistent_trigger_inventory(&tx, true)?;
    validate_recall_impression_ledger_schema(&tx)?;
    validate_typo_fallback_attribution_schema(&tx)?;
    validate_wiki_recovery_ledgers_schema(&tx)?;
    validate_memory_outbox_schema(&tx)?;
    validate_memory_outbox_destination_apply_schema(&tx)?;
    validate_harness_session_attachments_schema(&tx)?;
    validate_harness_session_spine_schema(&tx)?;
    if identity.profile.includes_product() {
        validate_a2a_mailbox_schema(&tx)?;
        validate_mirror_eval_identity_schema(&tx)?;
    }
    #[cfg(test)]
    test_hooks::pause_after_schema_stamp_before_commit(&tx);
    funnel.check_before_commit(&tx)?;
    tx.commit()?;
    #[cfg(test)]
    test_hooks::run_window_hook(test_hooks::Window::AfterSchemaCommit, current_db_path);

    if funnel.writes_filesystem_artifacts() {
        remember_migration_fingerprint(conn, current_db_path)?;
    }
    Ok(SchemaInitOutcome { report, identity })
}

fn reevaluate_admission_in_tx(
    tx: &Connection,
    current_db_path: &Path,
    ctx: &crate::db::DbOpenContext,
    backup: Option<&BackupDecision>,
    input_inventory: bool,
) -> Result<bool, MemoryError> {
    use crate::db::migrations::{self, OpenContextDecision};

    if input_inventory {
        crate::db::validate_input_trigger_inventory(tx)?;
    }
    check_schema_version_gate(tx)?;
    let version = migrations::read_schema_version(tx)?;
    let decision = evaluate_db_open_context_gate(tx, current_db_path, ctx)?;
    validate_current_schema_integrity(tx)?;
    validate_presence(tx, current_db_path, ctx.required_profile)?;
    if let Some(backup) = backup {
        if matches!(decision, OpenContextDecision::AuthorizedMigration { .. })
            && !backup.covers_migration_of(version)
        {
            return Err(MemoryError::SchemaChangedDuringOpen {
                preflight: backup.version,
                current: version,
                db_path: current_db_path.display().to_string(),
            });
        }
    }
    migrations::log_authorized_migration(
        decision,
        current_db_path,
        ctx,
        crate::db::version_policy::VersionHeader::read(tx)?,
    );
    Ok(version == 0)
}

fn maybe_backup_before_migration(
    conn: &Connection,
    db_path: &Path,
) -> Result<BackupDecision, MemoryError> {
    let (cookie_empty, current_fp, stored) = {
        let snapshot = conn.unchecked_transaction()?;
        let read = (
            schema_cookie_is_empty(&snapshot)?,
            migration_schema_fingerprint(&snapshot)?,
            crate::db::migrations::read_schema_version(&snapshot)?,
        );
        snapshot.commit()?;
        read
    };
    if cookie_empty {
        return Ok(BackupDecision {
            version: stored,
            outcome: BackupOutcome::SkippedEmptySchema,
        });
    }

    let is_version_migration = (1..expected()).contains(&stored);

    if !is_version_migration {
        let marker = migration_marker_path(db_path);
        if std::fs::read_to_string(&marker).ok().as_deref() == Some(current_fp.as_str()) {
            return Ok(BackupDecision {
                version: stored,
                outcome: BackupOutcome::SkippedMarkerMatch,
            });
        }
    }

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let backup_path = sibling_with_suffix(db_path, &format!("migration-bak.{ts}"));

    let backed_up_version = {
        let mut dst = Connection::open(&backup_path)?;
        let backup = rusqlite::backup::Backup::new(conn, &mut dst)?;
        backup.run_to_completion(i32::MAX, Duration::from_millis(5), None)?;
        drop(backup);
        crate::db::migrations::read_schema_version(&dst)?
    };

    retain_recent_migration_backups(db_path);

    Ok(BackupDecision {
        version: backed_up_version,
        outcome: BackupOutcome::Written(backup_path),
    })
}
