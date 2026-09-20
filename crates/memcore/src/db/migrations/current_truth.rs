//! v37: install CurrentTruth's existing schema through the canonical product
//! migration boundary before the production adapter opens its typed handle.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v37_current_truth(
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
    fn v37_installs_current_truth_only_for_product_stores() {
        let product = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v37_current_truth(&product, StoreProfile::TachiFull).unwrap(),
            5
        );
        crate::db::schema::validate_current_truth_schema(&product).unwrap();

        let portable = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v37_current_truth(&portable, StoreProfile::PortableKernel).unwrap(),
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
    fn v37_validator_refuses_every_consumer_field_key_constraint_and_index_drift() {
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
    fn v37_validator_compatibility_is_formatting_only() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v37_current_truth(&conn, StoreProfile::TachiFull).unwrap();
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

    fn assert_drift_refused(object: &str, target: &str, replacement: &str) {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v37_current_truth(&conn, StoreProfile::TachiFull).unwrap();
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
