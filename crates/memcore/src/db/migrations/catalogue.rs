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
    39, "v39_mirror_eval_identity", Product, |conn, context, report| { report.mirror_eval_identity_columns_added = migrate_v39_mirror_eval_identity(conn, context.profile)?; Ok(()) };
    40, "v40_work_claim_project_binding", Product, |conn, context, report| { report.identity_workclaim_columns_added += identity_workclaim_spine::migrate_v40_work_claim_project_binding(conn, context.profile)?; Ok(()) };
    41, "v41_host_typed_records", Portable, |conn, _context, _report| { crate::db::typed_records::install(conn) };
    42, "v42_coordination_mailbox", Product, |conn, context, _report| { if context.profile.includes_product() { crate::db::schema::migrate_v42_coordination_mailbox(conn)?; } Ok(()) };
    43, "v43_adjudication_provenance", Product, |conn, context, _report| { crate::db::adjudication_provenance::migrate_v43_adjudication_provenance(conn, context.profile).map(drop) };
}

pub(super) const fn expected_version(entries: &[Migration]) -> u32 {
    assert!(!entries.is_empty(), "migration catalogue must not be empty");
    let mut index = 0;
    while index < entries.len() {
        assert!(
            entries[index].index == BASELINE_SCHEMA_VERSION + index as u32,
            "migration catalogue must be contiguous and start at BASELINE_SCHEMA_VERSION"
        );
        index += 1;
    }
    entries[entries.len() - 1].index
}

pub(super) const fn portable_version(entries: &[Migration]) -> u32 {
    let mut version = if entries.is_empty() {
        0
    } else {
        BASELINE_SCHEMA_VERSION
    };
    let mut index = 0;
    while index < entries.len() {
        if matches!(entries[index].scope, MigrationScope::Portable) {
            version = entries[index].index;
        }
        index += 1;
    }
    version
}

/// Only test-scoped catalogues (pre-B prefixes, synthetic futures) differ from
/// the shipped one; pinned schema baselines describe the shipped one alone.
pub(crate) fn is_shipped() -> bool {
    #[cfg(test)]
    if test_support::override_entries().is_some() {
        return false;
    }
    true
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

    pub(crate) fn with_prefix<T>(version: u32, run: impl FnOnce() -> T) -> T {
        struct Restore(Option<Vec<Migration>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                OVERRIDE.with(|s| *s.borrow_mut() = self.0.take());
            }
        }
        let mut prefix = entries().into_owned();
        assert!(
            version >= BASELINE_SCHEMA_VERSION && version <= expected_version(&prefix),
            "prefix version {version} out of catalogue bounds"
        );
        prefix.retain(|entry| entry.index <= version);
        let _restore = Restore(OVERRIDE.with(|s| s.replace(Some(prefix))));
        run()
    }

    pub(crate) fn with_product_then_portable<T>(run: impl FnOnce() -> T) -> T {
        with_future_migration(
            false,
            |conn| {
                conn.execute_batch("CREATE TABLE d7_product_40(value TEXT);")?;
                Ok(())
            },
            || {
                let mut future = entries().into_owned();
                future.push(Migration {
                index: 41,
                sentinel: "v41_test_portable_migration",
                scope: MigrationScope::Portable,
                run: |conn, _context, _report| {
                    conn.execute_batch("CREATE TABLE d7_portable_41(value TEXT); INSERT INTO d7_portable_41 VALUES('applied');")?;
                    Ok(())
                },
            });
                let old = OVERRIDE.with(|s| s.replace(Some(future)));
                struct Restore(Option<Vec<Migration>>);
                impl Drop for Restore {
                    fn drop(&mut self) {
                        OVERRIDE.with(|s| *s.borrow_mut() = self.0.take());
                    }
                }
                let _restore = Restore(old);
                run()
            },
        )
    }

    pub(crate) fn with_future_migration<T>(
        portable: bool,
        body: fn(&Connection) -> Result<(), MemoryError>,
        run: impl FnOnce() -> T,
    ) -> T {
        // This scoped catalogue affects only the current test thread. Real
        // production funnels read it; no runtime/environment knob is shipped.
        type FutureBody = fn(&Connection) -> Result<(), MemoryError>;
        thread_local! {
            static FUTURE_BODY: Cell<Option<FutureBody>> = const { Cell::new(None) };
        }
        struct Restore(Option<Vec<Migration>>, Option<FutureBody>);
        impl Drop for Restore {
            fn drop(&mut self) {
                OVERRIDE.with(|entries| *entries.borrow_mut() = self.0.take());
                FUTURE_BODY.with(|body| body.set(self.1.take()));
            }
        }
        // D7 exercises its historical E39 -> synthetic E40/E41 fixtures.
        // Later real migrations must not replace those pinned transitions.
        let mut entries = MIGRATIONS[..1].to_vec();
        entries.push(Migration {
            index: 40,
            sentinel: "v40_test_future_migration",
            scope: if portable {
                MigrationScope::Portable
            } else {
                MigrationScope::Product
            },
            run: |conn, context, _report| {
                if !context.profile.includes_product()
                    && OVERRIDE.with(|entries| {
                        entries.borrow().as_ref().unwrap()[1].scope == MigrationScope::Product
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
        // catalogue, but cannot silently reclassify a shipped suffix.
        assert_eq!(BASELINE_SCHEMA_VERSION, 39);
        assert_eq!(PORTABLE_COMPAT_FLOOR, 39);
        assert!(MIGRATIONS.len() >= 5);
        assert_eq!(MIGRATIONS[0].index, 39);
        let expected_scopes = [
            (39, MigrationScope::Product),
            (40, MigrationScope::Product),
            (41, MigrationScope::Portable),
            (42, MigrationScope::Product),
            (43, MigrationScope::Product),
        ];
        for (migration, (expected_index, expected_scope)) in
            MIGRATIONS.iter().zip(expected_scopes.iter())
        {
            assert_eq!(migration.index, *expected_index);
            assert_eq!(
                migration.scope, *expected_scope,
                "shipped scope v{}",
                migration.index
            );
        }
        assert_eq!(portable_version(&MIGRATIONS[..1]), 39);
        assert_eq!(expected_version(&MIGRATIONS[..1]), 39);
        assert_eq!(portable_version(MIGRATIONS), 41);
        assert_eq!(expected_version(MIGRATIONS), 43);
        let keys: std::collections::BTreeSet<_> =
            MIGRATIONS.iter().map(|entry| entry.sentinel).collect();
        assert_eq!(keys.len(), MIGRATIONS.len(), "sentinels are unique");
    }
}
