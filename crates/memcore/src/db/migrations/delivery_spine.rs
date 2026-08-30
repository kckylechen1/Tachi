//! v36: durable delivery spine for #1679 — one durable delivery intent per
//! terminal result plus an append-only delivery event ledger.
//!
//! Additive only. The spine stays receipts-only: no table here stores raw
//! private result content, and no table touches execution or adjudication
//! truth.

use rusqlite::Connection;

use crate::error::MemoryError;

pub(super) fn migrate_v36_delivery_spine(conn: &Connection) -> Result<usize, MemoryError> {
    crate::db::schema::install_delivery_spine_schema(conn)?;
    crate::db::schema::validate_delivery_spine_schema(conn)?;
    // Two tables plus three indexes; CREATE IF NOT EXISTS keeps the receipt
    // count stable on replay.
    Ok(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v36_creates_the_delivery_spine_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrate_v36_delivery_spine(&conn).unwrap(),
            5,
            "migration receipt count is stable on replay"
        );
        crate::db::schema::validate_delivery_spine_schema(&conn).unwrap();
    }

    #[test]
    fn v36_spine_schema_is_provider_neutral() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v36_delivery_spine(&conn).unwrap();
        let names: Vec<String> = conn
            .prepare(
                "SELECT name FROM main.sqlite_schema WHERE type = 'table'
                 AND name LIKE 'delivery_%'",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!names.is_empty());
        for name in names {
            let lowered = name.to_ascii_lowercase();
            for forbidden in ["zeroclaw", "codex", "claude", "vendor"] {
                assert!(
                    !lowered.contains(forbidden),
                    "provider-specific name {forbidden:?} leaked into the delivery spine schema"
                );
            }
        }
    }

    #[test]
    fn v36_validator_refuses_drifted_state_vocabulary() {
        for (table, removed_clause) in [
            (
                "delivery_intents",
                "delivery_state IN ('not_ready', 'ready', 'requester_queued', 'delivered', 'blocked', 'retrying', 'dismissed')",
            ),
            (
                "delivery_intents",
                "CHECK (delivery_state IN ('blocked', 'retrying') OR blocker_class IS NULL)",
            ),
            (
                "delivery_events",
                "kind IN ('intent_created', 'result_ready', 'result_superseded', 'claimed', 'delivered', 'blocked', 'retry_scheduled', 'dismissed', 'claim_expired', 'transition_debt')",
            ),
            ("delivery_events", "UNIQUE (delivery_id, event_id)"),
        ] {
            let conn = Connection::open_in_memory().unwrap();
            migrate_v36_delivery_spine(&conn).unwrap();
            conn.execute_batch("PRAGMA writable_schema = ON;").unwrap();
            let changed = conn
                .execute(
                    "UPDATE sqlite_schema
                     SET sql = replace(sql, ?1, '')
                     WHERE type = 'table' AND name = ?2",
                    rusqlite::params![removed_clause, table],
                )
                .unwrap();
            assert_eq!(changed, 1, "must mutate the {table} fixture");
            conn.execute_batch("PRAGMA writable_schema = OFF;").unwrap();

            let error = crate::db::schema::validate_delivery_spine_schema(&conn)
                .expect_err("drifted delivery constraint must fail closed");
            assert!(error.to_string().contains(table), "{table}: {error}");
        }
    }

    #[test]
    fn v36_validator_refuses_claim_lease_columns_without_their_pairing_checks() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v36_delivery_spine(&conn).unwrap();
        conn.execute_batch("PRAGMA writable_schema = ON;").unwrap();
        let changed = conn
            .execute(
                "UPDATE sqlite_schema
                 SET sql = replace(sql, ?1, '')
                 WHERE type = 'table' AND name = 'delivery_intents'",
                rusqlite::params![
                    "CHECK (delivery_state != 'requester_queued' OR (active_claim_key IS NOT NULL AND claimed_by IS NOT NULL AND claim_expires_at IS NOT NULL))"
                ],
            )
            .unwrap();
        assert_eq!(changed, 1);
        conn.execute_batch("PRAGMA writable_schema = OFF;").unwrap();
        let error = crate::db::schema::validate_delivery_spine_schema(&conn)
            .expect_err("dropped claim-lease pairing check must fail closed");
        assert!(error.to_string().contains("delivery_intents"), "{error}");
    }
}
