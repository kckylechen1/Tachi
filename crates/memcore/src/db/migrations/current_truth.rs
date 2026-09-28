//! v38: install CurrentTruth's existing schema through the canonical product
//! migration boundary before the production adapter opens its typed handle.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v38_current_truth(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    if !profile.includes_product() {
        return Ok(0);
    }
    crate::db::schema::install_current_truth_schema(conn)?;
    crate::db::schema::validate_current_truth_schema(conn)?;
    // Three tables plus two explicit indexes.
    Ok(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v38_installs_current_truth_only_for_product_stores() {
        let product = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v38_current_truth(&product, StoreProfile::TachiFull).unwrap(),
            5
        );
        crate::db::schema::validate_current_truth_schema(&product).unwrap();

        let portable = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v38_current_truth(&portable, StoreProfile::PortableKernel).unwrap(),
            0
        );
        let count: i64 = portable
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'current_truth_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn v38_validator_refuses_every_consumer_field_key_constraint_and_index_drift() {
        let field_cases = [
            ("current_truth_assertions", "assertion_id"),
            ("current_truth_assertions", "subject_repo"),
            ("current_truth_assertions", "subject_kind"),
            ("current_truth_assertions", "subject_id"),
            ("current_truth_assertions", "predicate"),
            ("current_truth_assertions", "value_json"),
            ("current_truth_assertions", "issuer"),
            ("current_truth_assertions", "authority"),
            ("current_truth_assertions", "source_id"),
            ("current_truth_assertions", "source_revision"),
            ("current_truth_assertions", "observed_at"),
            ("current_truth_assertions", "effective_at"),
            ("current_truth_assertions", "supersedes"),
            ("current_truth_assertions", "evidence_json"),
            ("current_truth_assertions", "review_state"),
            ("current_truth_assertions", "visibility"),
            ("current_truth_assertions", "content_digest"),
            ("current_truth_assertions", "recorded_at"),
            ("current_truth_projection", "repo"),
            ("current_truth_projection", "generation"),
            ("current_truth_projection", "built_at"),
            ("current_truth_projection", "view_json"),
            ("current_truth_refresh", "repo"),
            ("current_truth_refresh", "subject_token"),
            ("current_truth_refresh", "fresh"),
            ("current_truth_refresh", "last_fresh_revision"),
            ("current_truth_refresh", "last_fresh_at"),
            ("current_truth_refresh", "last_attempt_at"),
            ("current_truth_refresh", "unavailable_reason"),
            ("current_truth_refresh", "repository_visibility"),
            ("current_truth_refresh", "repository_visibility_at"),
        ];
        for (object, field) in field_cases {
            assert_drift_refused(object, field, &format!("drift_{field}"));
        }
        for (object, target, replacement) in [
            (
                "idx_ct_assertions_subject",
                "subject_repo, subject_kind, subject_id",
                "subject_kind, subject_repo, subject_id",
            ),
            ("idx_ct_assertions_predicate", "predicate", "subject_repo"),
            (
                "current_truth_assertions",
                "UNIQUE (subject_repo, subject_kind, subject_id, predicate,",
                "UNIQUE (subject_repo, subject_kind, predicate, subject_id,",
            ),
            (
                "current_truth_refresh",
                "CHECK (fresh IN (0, 1))",
                "CHECK (fresh IN (0, 1, 2))",
            ),
            (
                "current_truth_refresh",
                "CHECK (repository_visibility IN ('public', 'private'))",
                "CHECK (repository_visibility IN ('public', 'private', 'unknown'))",
            ),
            (
                "current_truth_refresh",
                "PRIMARY KEY (repo, subject_token)",
                "PRIMARY KEY (subject_token, repo)",
            ),
        ] {
            assert_drift_refused(object, target, replacement);
        }
    }

    #[test]
    fn v38_validator_compatibility_is_formatting_only() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v38_current_truth(&conn, StoreProfile::TachiFull).unwrap();
        conn.execute_batch("PRAGMA writable_schema = ON;").unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE sqlite_schema
                 SET sql = replace(sql, 'CREATE TABLE', 'CREATE    TABLE')
                 WHERE name = 'current_truth_refresh'",
                [],
            )
            .unwrap(),
            1
        );
        conn.execute_batch("PRAGMA writable_schema = OFF;").unwrap();
        crate::db::schema::validate_current_truth_schema(&conn)
            .expect("ASCII-whitespace-only formatting remains compatible");
    }

    #[test]
    fn v38_validator_rejects_unicode_whitespace_in_consumer_columns() {
        for whitespace in ['\u{00a0}', '\u{2003}', '\u{202f}'] {
            for (table, field) in [
                ("current_truth_assertions", "observed_at"),
                ("current_truth_projection", "generation"),
                ("current_truth_refresh", "last_attempt_at"),
            ] {
                let conn = Connection::open_in_memory().unwrap();
                migrate_v38_current_truth(&conn, StoreProfile::TachiFull).unwrap();
                let sql: String = conn
                    .query_row(
                        "SELECT sql FROM sqlite_schema WHERE name = ?1",
                        [table],
                        |row| row.get(0),
                    )
                    .unwrap();
                let indexes: Vec<String> = conn.prepare("SELECT sql FROM sqlite_schema WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL")
                    .unwrap().query_map([table], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
                conn.execute_batch(&format!("DROP TABLE {table};")).unwrap();
                let target = format!("{field} TEXT");
                assert!(sql.contains(&target));
                let malformed = sql.replace(&target, &format!("{field}{whitespace} TEXT"));
                conn.execute_batch(&malformed).unwrap();
                for index in indexes {
                    conn.execute_batch(&index).unwrap();
                }
                assert!(
                    conn.prepare(&format!("SELECT {field} FROM {table}"))
                        .is_err(),
                    "SQLite must treat U+{:04X} as part of the consumer column",
                    whitespace as u32
                );
                crate::db::schema::validate_current_truth_schema(&conn).expect_err(
                    "a missing consumer column must never pass canonical DDL validation",
                );
            }
        }
    }

    #[test]
    fn v38_fresh_profiles_upgrade_from_prior_stamps_and_reopen() {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        use crate::db::migrations::{read_schema_version, validate_current_schema_integrity};
        use crate::db::{init_schema_with_label_mut, DbOpenContext};
        for (profile, previous) in [
            (StoreProfile::TachiFull, 37),
            (StoreProfile::PortableKernel, 36),
            (StoreProfile::PortableKernel, 37),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("profile.sqlite");
            let mut conn = Connection::open(&path).unwrap();
            let mut create = DbOpenContext::create_fresh();
            create.required_profile = profile.into();
            init_schema_with_label_mut(&mut conn, "global", &path, &create).unwrap();
            assert_eq!(
                read_schema_version(&conn).unwrap(),
                crate::db::migrations::EXPECTED_SCHEMA_VERSION
            );
            validate_current_schema_integrity(&conn).unwrap();
            let object_count = |conn: &Connection, table: &str| -> i64 {
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap()
            };
            for table in [
                "identity_admission_verification_receipts",
                "current_truth_assertions",
            ] {
                assert_eq!(
                    object_count(&conn, table),
                    i64::from(profile.includes_product())
                );
            }
            if profile.includes_product() {
                conn.execute_batch("DROP TABLE current_truth_assertions; DROP TABLE current_truth_projection; DROP TABLE current_truth_refresh;").unwrap();
            }
            conn.execute("DELETE FROM hard_state WHERE namespace = 'migrations' AND key = 'v38_current_truth'", []).unwrap();
            if previous == 36 {
                conn.execute("DELETE FROM hard_state WHERE namespace = 'migrations' AND key = 'v37_verified_agent_admissions'", []).unwrap();
            }
            crate::db::migrations::write_schema_version(&conn, previous).unwrap();
            drop(conn);
            let mut conn = Connection::open(&path).unwrap();
            let mut deny = DbOpenContext::open_existing_deny();
            deny.required_profile = profile.into();
            init_schema_with_label_mut(&mut conn, "global", &path, &deny)
                .expect_err("an older store requires explicit migration authority");
            assert_eq!(read_schema_version(&conn).unwrap(), previous);
            let mut allow = DbOpenContext::open_existing_allow("test:v38-profile-upgrade");
            allow.required_profile = profile.into();
            init_schema_with_label_mut(&mut conn, "global", &path, &allow).unwrap();
            assert_eq!(
                read_schema_version(&conn).unwrap(),
                crate::db::migrations::EXPECTED_SCHEMA_VERSION
            );
            for key in ["v37_verified_agent_admissions", "v38_current_truth"] {
                let count: i64 = conn.query_row("SELECT COUNT(*) FROM hard_state WHERE namespace = 'migrations' AND key = ?1", [key], |row| row.get(0)).unwrap();
                assert_eq!(count, 1);
            }
            for table in [
                "identity_admission_verification_receipts",
                "current_truth_assertions",
            ] {
                assert_eq!(
                    object_count(&conn, table),
                    i64::from(profile.includes_product())
                );
            }
            validate_current_schema_integrity(&conn).unwrap();
            drop(conn);
            let mut conn = Connection::open(&path).unwrap();
            init_schema_with_label_mut(&mut conn, "global", &path, &deny).unwrap();
            validate_current_schema_integrity(&conn).unwrap();
            if profile.includes_product() {
                crate::db::verified_admissions::validate_verified_admission_schema(&conn).unwrap();
                crate::db::schema::validate_current_truth_schema(&conn).unwrap();
            }
        }
    }

    #[test]
    fn v38_current_stamp_refuses_both_inventory_drifts_without_repair() {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        for index in [
            "idx_ct_assertions_predicate",
            "idx_identity_admissions_verified_binding",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("drift.sqlite");
            let mut conn = Connection::open(&path).unwrap();
            crate::db::init_schema_with_label_mut(
                &mut conn,
                "global",
                &path,
                &crate::db::DbOpenContext::create_fresh(),
            )
            .unwrap();
            conn.execute_batch(&format!("DROP INDEX {index};")).unwrap();
            let before: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get(0))
                .unwrap();
            drop(conn);
            let mut conn = Connection::open(&path).unwrap();
            crate::db::init_schema_with_label_mut(
                &mut conn,
                "global",
                &path,
                &crate::db::DbOpenContext::open_existing_allow("test:v38-drift"),
            )
            .expect_err(
                "a stamped current inventory must fail closed even with migration authority",
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                before
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1",
                    [index],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            assert_eq!(
                crate::db::migrations::read_schema_version(&conn).unwrap(),
                crate::db::migrations::EXPECTED_SCHEMA_VERSION
            );
        }
    }

    fn assert_drift_refused(object: &str, target: &str, replacement: &str) {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v38_current_truth(&conn, StoreProfile::TachiFull).unwrap();
        conn.execute_batch("PRAGMA writable_schema = ON;").unwrap();
        let changed = conn
            .execute(
                "UPDATE sqlite_schema SET sql = replace(sql, ?1, ?2) WHERE name = ?3",
                rusqlite::params![target, replacement, object],
            )
            .unwrap();
        assert_eq!(changed, 1, "drift fixture must target {object}.{target}");
        conn.execute_batch("PRAGMA writable_schema = OFF;").unwrap();
        crate::db::schema::validate_current_truth_schema(&conn).unwrap_err();
    }
}
