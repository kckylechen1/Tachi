//! The shipped migration catalogue is the sole source of versions, scopes and sentinels.
//! Shipped scopes are immutable (D7); corrections require a new migration.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MigrationScope {
    Portable,
    Product,
}

pub(super) struct MigrationContext<'a> {
    pub db_label: &'a str,
    pub path: &'a Path,
    pub profile: StoreProfile,
}

#[derive(Clone, Copy)]
pub(super) struct Migration {
    pub index: u32,
    pub sentinel: &'static str,
    pub scope: MigrationScope,
    pub run:
        fn(&Connection, &MigrationContext<'_>, &mut MigrationReport) -> Result<(), MemoryError>,
}

macro_rules! catalogue {
    ($($index:literal, $key:literal, $scope:ident, $body:expr;)+) => {
        pub(super) const MIGRATIONS: &[Migration] = &[
            $(Migration { index: $index, sentinel: $key, scope: MigrationScope::$scope, run: $body },)+
        ];
        pub(super) const SENTINELS: &[&str] = &[$($key,)+];
    };
}

catalogue! {
    1, "v1_path_normalize_legacy", Portable, |conn, _context, report| { report.paths_normalized = migrate_v1_path_normalize(conn)?; Ok(()) };
    2, "v2_scope_self_normalize", Portable, |conn, _context, report| { report.scopes_fixed = migrate_v2_scope_normalize(conn)?; Ok(()) };
    3, "v3_handoff_path_standardize", Portable, |conn, _context, report| { report.handoff_paths_standardized = migrate_v3_handoff_standardize(conn)?; Ok(()) };
    4, "v4_quarantine_cross_db_rows", Portable, |conn, context, report| { let (quarantined, skipped) = migrate_v4_quarantine_cross_db(conn, context.db_label, context.path)?; report.quarantined = quarantined; report.quarantine_skipped_sanity_guard = skipped; Ok(()) };
    5, "v5_drop_hypertachi_legacy_columns", Portable, |conn, _context, report| { report.hypertachi_legacy_columns_dropped = migrate_v5_drop_hypertachi_legacy_columns(conn)?; Ok(()) };
    6, "v6_fold_persons_into_entities", Portable, |conn, _context, report| { report.persons_folded_into_entities = migrate_v6_fold_persons_into_entities(conn)?; Ok(()) };
    7, "v7_reconcile_legacy_memory_columns", Portable, |conn, _context, report| { report.legacy_columns_reconciled = migrate_v7_reconcile_legacy_memory_columns(conn)?; Ok(()) };
    8, "v8_drop_legacy_persons_column", Portable, |conn, _context, report| { report.persons_columns_dropped = fold_and_drop_legacy_persons_column(conn)?; Ok(()) };
    9, "v9_relocate_and_drop_location", Portable, |conn, _context, report| { let (relocated, dropped) = migrate_v9_relocate_and_drop_location(conn)?; report.locations_relocated = relocated; report.location_columns_dropped = dropped; Ok(()) };
    10, "v10_drop_pack_tables", Portable, |conn, _context, report| { report.pack_tables_dropped = migrate_v10_drop_pack_tables(conn)?; Ok(()) };
    11, "v11_drop_domains_table", Portable, |conn, _context, report| { report.domains_table_dropped = migrate_v11_drop_domains_table(conn)?; Ok(()) };
    12, "v12_session_claims_unique_identity", Product, |conn, context, report| { report.session_claims_duplicates_deduped = migrate_v12_session_claims_unique_identity(conn, context.profile)?; Ok(()) };
    13, "v13_hard_state_ns_updated_index", Portable, |conn, _context, report| { report.hard_state_index_added = migrate_v13_add_hard_state_index(conn)?; Ok(()) };
    14, "v14_dispatch_outcomes_reported_outcome", Product, |conn, context, report| { report.dispatch_outcomes_reported_outcome_added = migrate_v14_dispatch_outcomes_reported_outcome(conn, context.profile)?; Ok(()) };
    15, "v15_exec_envs_env_class", Product, |conn, context, report| { report.exec_envs_env_class_added = migrate_v15_exec_envs_env_class(conn, context.profile)?; Ok(()) };
    16, "v16_dispatch_outcomes_identity_receipt", Product, |conn, context, report| { report.dispatch_outcomes_identity_receipt_added = migrate_v16_dispatch_outcomes_identity_receipt(conn, context.profile)?; Ok(()) };
    17, "v17_dispatch_outcomes_attribution_basis", Product, |conn, context, report| { report.dispatch_outcomes_attribution_basis_backfilled = migrate_v17_dispatch_outcomes_attribution_basis(conn, context.profile)?; Ok(()) };
    18, "v18_dispatch_adjudications", Product, |conn, context, report| { report.dispatch_adjudications_created = migrate_v18_dispatch_adjudications(conn, context.profile)?; Ok(()) };
    19, "v19_idless_memory_identity", Portable, |conn, _context, report| { report.idless_identity_constraint_added = migrate_v19_add_idless_memory_identity(conn)?; Ok(()) };
    20, "v20_mirror_eval", Product, |conn, context, report| { report.mirror_eval_tables_created = migrate_v20_mirror_eval(conn, context.profile)?; Ok(()) };
    21, "v21_identity_workclaim_spine", Product, |conn, context, report| { report.identity_workclaim_columns_added = migrate_v21_identity_workclaim_spine(conn, context.profile)?; Ok(()) };
    22, "v22_memories_symbolic_fts", Portable, |conn, _context, report| { report.memories_symbolic_fts_rows = migrate_v22_memories_symbolic_fts(conn)?; Ok(()) };
    23, "v23_reserved_reference_guards", Portable, |conn, _context, report| { report.reserved_reference_guards_installed = migrate_v23_reserved_reference_guards(conn)?; Ok(()) };
    24, "v24_memories_scored_count", Portable, |conn, _context, report| { report.scored_count_column_added = migrate_v24_memories_scored_count(conn)?; Ok(()) };
    25, "v25_recall_impression_ledger", Portable, |conn, _context, report| { report.recall_impression_schema_objects_created = migrate_v25_recall_impression_ledger(conn)?; Ok(()) };
    26, "v26_recall_impression_replay_identity", Portable, |conn, _context, report| { report.recall_impression_replay_identity_columns_added = migrate_v26_recall_impression_replay_identity(conn)?; Ok(()) };
    27, "v27_typo_fallback_attribution", Portable, |conn, _context, report| { report.typo_fallback_attribution_columns_added = migrate_v27_typo_fallback_attribution(conn)?; Ok(()) };
    28, "v28_wiki_recovery_ledgers", Portable, |conn, _context, report| { report.wiki_recovery_schema_objects_created = migrate_v28_wiki_recovery_ledgers(conn)?; Ok(()) };
    29, "v29_memory_outbox", Portable, |conn, _context, report| { report.memory_outbox_schema_objects_created = migrate_v29_memory_outbox(conn)?; Ok(()) };
    30, "v30_memory_outbox_destination_apply", Portable, |conn, _context, report| { report.memory_outbox_destination_apply_schema_objects_created = migrate_v30_memory_outbox_destination_apply(conn)?; Ok(()) };
    31, "v31_a2a_mailbox", Product, |conn, context, report| { report.a2a_mailbox_schema_objects_created = migrate_v31_a2a_mailbox(conn, context.profile)?; Ok(()) };
    32, "v32_a2a_body_retention", Product, |conn, context, report| { report.a2a_body_retention_schema_objects_rebuilt = migrate_v32_a2a_body_retention(conn, context.profile)?; Ok(()) };
    33, "v33_harness_session_attachments", Portable, |conn, _context, report| { report.harness_session_attachments_schema_objects_created = migrate_v33_harness_session_attachments(conn)?; Ok(()) };
    34, "v34_harness_session_spine", Portable, |conn, _context, report| { report.harness_session_spine_schema_objects_created = migrate_v34_harness_session_spine(conn)?; Ok(()) };
    35, "v35_harness_session_spine_receipts", Portable, |conn, _context, report| { report.harness_session_spine_receipt_tables_rebuilt = migrate_v35_harness_session_spine_receipts(conn)?; Ok(()) };
    36, "v36_delivery_spine", Portable, |conn, _context, report| { report.delivery_spine_schema_objects_created = migrate_v36_delivery_spine(conn)?; Ok(()) };
    37, "v37_verified_agent_admissions", Product, |conn, context, report| { report.verified_admission_schema_objects_created = migrate_v37_verified_agent_admissions(conn, context.profile)?; Ok(()) };
    38, "v38_current_truth", Product, |conn, context, report| { report.current_truth_schema_objects_created = migrate_v38_current_truth(conn, context.profile)?; Ok(()) };
    39, "v39_mirror_eval_identity", Product, |conn, context, report| { report.mirror_eval_identity_columns_added = migrate_v39_mirror_eval_identity(conn, context.profile)?; Ok(()) };
}

pub(super) const fn expected_version(entries: &[Migration]) -> u32 {
    let mut index = 0;
    while index < entries.len() {
        assert!(
            entries[index].index == index as u32 + 1,
            "migration catalogue must be contiguous and ordered"
        );
        index += 1;
    }
    entries.len() as u32
}

pub(super) const fn portable_version(entries: &[Migration]) -> u32 {
    let mut version = 0;
    let mut index = 0;
    while index < entries.len() {
        if matches!(entries[index].scope, MigrationScope::Portable) {
            version = entries[index].index;
        }
        index += 1;
    }
    version
}

pub(super) fn entries() -> std::borrow::Cow<'static, [Migration]> {
    #[cfg(test)]
    if let Some(entries) = test_support::override_entries() {
        return std::borrow::Cow::Owned(entries);
    }
    std::borrow::Cow::Borrowed(MIGRATIONS)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::cell::{Cell, RefCell};

    thread_local! {
        static OVERRIDE: RefCell<Option<Vec<Migration>>> = const { RefCell::new(None) };
        static INVOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    pub(in crate::db::migrations) fn override_entries() -> Option<Vec<Migration>> {
        OVERRIDE.with(|entries| entries.borrow().clone())
    }

    pub(crate) fn record_invocation() {
        INVOCATIONS.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn take_invocations() -> usize {
        INVOCATIONS.with(|count| count.replace(0))
    }

    pub(crate) fn with_future_migration<T>(
        portable: bool,
        body: fn(&Connection) -> Result<(), MemoryError>,
        run: impl FnOnce() -> T,
    ) -> T {
        // This scoped catalogue affects only the current test thread. Real
        // production funnels read it; no runtime/environment knob is shipped.
        thread_local! {
            static FUTURE_BODY: Cell<Option<fn(&Connection) -> Result<(), MemoryError>>> = const { Cell::new(None) };
        }
        struct Restore(
            Option<Vec<Migration>>,
            Option<fn(&Connection) -> Result<(), MemoryError>>,
        );
        impl Drop for Restore {
            fn drop(&mut self) {
                OVERRIDE.with(|entries| *entries.borrow_mut() = self.0.take());
                FUTURE_BODY.with(|body| body.set(self.1.take()));
            }
        }
        let mut entries = MIGRATIONS.to_vec();
        entries.push(Migration {
            index: expected_version(MIGRATIONS) + 1,
            sentinel: "v40_test_future_migration",
            scope: if portable {
                MigrationScope::Portable
            } else {
                MigrationScope::Product
            },
            run: |conn, context, _report| {
                if !context.profile.includes_product()
                    && OVERRIDE.with(|entries| {
                        entries.borrow().as_ref().unwrap().last().unwrap().scope
                            == MigrationScope::Product
                    })
                {
                    return Ok(());
                }
                FUTURE_BODY.with(|body| body.get().expect("scoped future body"))(conn)
            },
        });
        let old = OVERRIDE.with(|slot| slot.replace(Some(entries)));
        let old_body = FUTURE_BODY.with(|slot| slot.replace(Some(body)));
        let _restore = Restore(old, old_body);
        run()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_scopes_and_projection_are_frozen() {
        // Independent literal pin: a future appended migration may extend the
        // catalogue, but cannot silently reclassify a shipped prefix.
        let product = [12, 14, 15, 16, 17, 18, 20, 21, 31, 32, 37, 38, 39];
        assert!(MIGRATIONS.len() >= 39);
        for migration in &MIGRATIONS[..39] {
            assert_eq!(
                migration.scope == MigrationScope::Product,
                product.contains(&migration.index),
                "shipped scope v{}",
                migration.index
            );
        }
        assert_eq!(portable_version(&MIGRATIONS[..39]), 36);
        assert_eq!(expected_version(&MIGRATIONS[..39]), 39);
        assert_eq!(PORTABLE_COMPAT_FLOOR, 39);
        let keys: std::collections::BTreeSet<_> =
            MIGRATIONS.iter().map(|entry| entry.sentinel).collect();
        assert_eq!(keys.len(), MIGRATIONS.len(), "sentinels are unique");
    }
}
