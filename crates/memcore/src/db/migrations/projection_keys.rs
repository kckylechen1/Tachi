//! v47: stable rowids for the lexical and symbolic FTS projections.

use rusqlite::Connection;

use crate::error::MemoryError;

pub(crate) fn install_v47_fts_projection_map(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch(crate::db::fts_projection::FTS_PROJECTION_MAP_DDL)?;
    Ok(())
}

pub(crate) fn migrate_v47_fts_projection_keys(conn: &Connection) -> Result<(), MemoryError> {
    install_v47_fts_projection_map(conn)?;
    let had_projection_rows: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memories_fts)
             OR EXISTS(SELECT 1 FROM memories_symbolic_fts)",
        [],
        |row| row.get(0),
    )?;
    let inserted = crate::db::fts_projection::rebuild_memories_fts_from_authority(conn)?;
    if had_projection_rows || inserted > 0 {
        crate::db::bump_search_generation(conn)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::catalogue::test_support::with_prefix;
    use crate::db::{DbOpenContext, IdlessUpsertResult, InsertMemoryResult, StoreProfile};
    use crate::{MemoryEntry, MemoryStore};
    use rusqlite::params;

    fn entry(id: &str, text: &str) -> MemoryEntry {
        let mut entry: MemoryEntry = serde_json::from_value(serde_json::json!({
            "id": id,
            "path": "/notes/v47",
            "summary": text,
            "text": text,
            "topic": "v47-topic",
            "keywords": ["legacy-keyword", "comma,keyword"],
            "entities": ["entity-one"],
            "timestamp": "2026-11-27T00:00:00Z"
        }))
        .unwrap();
        entry.fold_persons_into_entities();
        entry
    }

    fn assert_aligned(conn: &Connection, expected: usize) {
        for table in ["memories_fts", "memories_symbolic_fts"] {
            let aligned: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM memories_fts_projection_map p
                         JOIN {table} f ON f.rowid = p.projection_key AND f.id = p.memory_id"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(aligned as usize, expected, "{table}");
        }
        let mapped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts_projection_map",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapped as usize, expected);
    }

    fn projection_key(conn: &Connection, id: &str) -> i64 {
        conn.query_row(
            "SELECT projection_key FROM memories_fts_projection_map WHERE memory_id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn memory_rowid(conn: &Connection, id: &str) -> i64 {
        conn.query_row("SELECT rowid FROM memories WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn term_hits(conn: &Connection, table: &str, term: &str) -> i64 {
        if table == "memories_fts" {
            conn.query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH ?1",
                [format!("\"{term}\"")],
                |row| row.get(0),
            )
            .unwrap()
        } else {
            conn.query_row(
                "SELECT COUNT(*) FROM memories_symbolic_fts WHERE text LIKE ?1",
                [format!("%{term}%")],
                |row| row.get(0),
            )
            .unwrap()
        }
    }

    fn neighbor_projection(conn: &Connection) -> (i64, String, i64, String) {
        conn.query_row(
            "SELECT l.rowid, l.text, s.rowid, s.text
             FROM memories_fts l JOIN memories_symbolic_fts s ON s.id = l.id
             WHERE l.id = 'neighbor'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    }

    #[test]
    fn v47_rebuilds_historical_rowids_and_updates_only_the_mapped_memory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v46-misaligned.db");
        let path_str = path.to_str().unwrap();
        with_prefix(46, || {
            let mut store =
                MemoryStore::open_with_context(path_str, &DbOpenContext::create_fresh()).unwrap();
            store.upsert(&entry("target", "legacy-token")).unwrap();
            store
                .upsert(&entry("neighbor", "neighbor-sentinel"))
                .unwrap();
            assert_eq!(
                super::super::read_schema_version(store.connection()).unwrap(),
                46
            );
        });

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "UPDATE memories SET rowid = 100 WHERE id = 'target';
                 UPDATE memories SET rowid = 200 WHERE id = 'neighbor';
                 DELETE FROM memories_fts;
                 DELETE FROM memories_symbolic_fts;
                 INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
                   SELECT 2, id, path, summary, text, 'legacy-keyword', 'entity-one'
                   FROM memories WHERE id = 'target';
                 INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
                   SELECT 1, id, path, summary, text, 'neighbor-keyword', 'entity-one'
                   FROM memories WHERE id = 'neighbor';
                 INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
                   VALUES (99, 'orphan', '/orphan', '', 'orphan-token', '', '');
                 INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
                   SELECT 2, id, path, summary, text, keywords, entities, topic
                   FROM memories WHERE id = 'target';
                 INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
                   SELECT 1, id, path, summary, text, keywords, entities, topic
                   FROM memories WHERE id = 'neighbor';
                 INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
                   VALUES (99, 'orphan', '/orphan', '', 'orphan-token', '', '', '');",
            )
            .unwrap();
        }

        let mut store = MemoryStore::open_with_context(
            path_str,
            &DbOpenContext::open_existing_allow("test:v47-misaligned"),
        )
        .unwrap();
        assert_eq!(
            super::super::read_schema_version(store.connection()).unwrap(),
            47
        );
        assert_aligned(store.connection(), 2);
        assert_ne!(
            projection_key(store.connection(), "target"),
            memory_rowid(store.connection(), "target")
        );
        assert_ne!(
            projection_key(store.connection(), "neighbor"),
            memory_rowid(store.connection(), "neighbor")
        );
        for table in ["memories_fts", "memories_symbolic_fts"] {
            assert_eq!(term_hits(store.connection(), table, "orphan-token"), 0);
            assert_eq!(term_hits(store.connection(), table, "legacy-token"), 1);
        }
        let neighbor_before = neighbor_projection(store.connection());

        assert!(store
            .update_with_revision(
                "target",
                "fresh-token",
                "fresh-token",
                "manual",
                &serde_json::json!({}),
                None,
                1,
            )
            .unwrap());
        for table in ["memories_fts", "memories_symbolic_fts"] {
            assert_eq!(term_hits(store.connection(), table, "legacy-token"), 0);
            assert_eq!(term_hits(store.connection(), table, "fresh-token"), 1);
        }
        assert_eq!(neighbor_projection(store.connection()), neighbor_before);
        drop(store);

        Connection::open(&path)
            .unwrap()
            .execute_batch("VACUUM")
            .unwrap();
        let mut store = MemoryStore::open(path_str).unwrap();
        assert_eq!(store.rebuild_fts_full().unwrap(), 2);
        assert_aligned(store.connection(), 2);
        let neighbor_before = neighbor_projection(store.connection());
        assert!(store
            .update_with_revision(
                "target",
                "after-rebuild-token",
                "after-rebuild-token",
                "manual",
                &serde_json::json!({}),
                None,
                2,
            )
            .unwrap());
        for table in ["memories_fts", "memories_symbolic_fts"] {
            assert_eq!(term_hits(store.connection(), table, "fresh-token"), 0);
            assert_eq!(
                term_hits(store.connection(), table, "after-rebuild-token"),
                1
            );
        }
        assert_eq!(neighbor_projection(store.connection()), neighbor_before);
        drop(store);

        with_prefix(46, || {
            let error = match MemoryStore::open_with_context(
                path_str,
                &DbOpenContext::open_existing_deny(),
            ) {
                Ok(_) => panic!("v46 writer must refuse the v47 store"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("newer than supported 46"),
                "{error}"
            );
        });
    }

    #[test]
    fn v47_rolls_back_mapping_and_both_projections_on_injected_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v47-rollback.db");
        let path_str = path.to_str().unwrap();
        with_prefix(46, || {
            let mut store =
                MemoryStore::open_with_context(path_str, &DbOpenContext::create_fresh()).unwrap();
            store.upsert(&entry("rollback", "rollback-token")).unwrap();
        });
        let mut conn = Connection::open(&path).unwrap();
        crate::db::fts_projection::register(&conn).unwrap();
        conn.execute_batch(&format!(
            "{} CREATE TRIGGER fail_v47_mapping BEFORE INSERT ON memories_fts_projection_map
             BEGIN SELECT RAISE(FAIL, 'injected v47 failure'); END;",
            crate::db::fts_projection::FTS_PROJECTION_MAP_DDL
        ))
        .unwrap();
        let before = conn
            .prepare("SELECT rowid, id, text FROM memories_fts ORDER BY rowid")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        {
            let tx = conn.transaction().unwrap();
            let error = migrate_v47_fts_projection_keys(&tx).expect_err("injected failure");
            assert!(
                error.to_string().contains("injected v47 failure"),
                "{error}"
            );
            tx.rollback().unwrap();
        }
        assert_eq!(super::super::read_schema_version(&conn).unwrap(), 46);
        let after = conn
            .prepare("SELECT rowid, id, text FROM memories_fts ORDER BY rowid")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(after, before);
        let mapped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts_projection_map",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapped, 0);

        conn.execute("DROP TRIGGER fail_v47_mapping", []).unwrap();
        {
            let tx = conn.transaction().unwrap();
            migrate_v47_fts_projection_keys(&tx).unwrap();
            tx.commit().unwrap();
        }
        assert_aligned(&conn, 1);
        assert_eq!(term_hits(&conn, "memories_fts", "rollback-token"), 1);
        assert_eq!(
            term_hits(&conn, "memories_symbolic_fts", "rollback-token"),
            1
        );
    }

    #[test]
    fn fresh_v47_insert_duplicate_and_idless_noop_share_one_mapping() {
        let mut store = MemoryStore::open_in_memory().unwrap();
        let original = entry("fresh", "fresh-token");
        store.upsert(&original).unwrap();
        let key = projection_key(store.connection(), "fresh");
        assert_aligned(store.connection(), 1);

        assert_eq!(
            store.insert_if_absent(&original).unwrap(),
            InsertMemoryResult::Existing
        );
        assert_eq!(projection_key(store.connection(), "fresh"), key);
        assert_aligned(store.connection(), 1);

        let winner = entry("idless-winner", "idless-token");
        assert_eq!(
            store.upsert_idless(&winner, "idless-identity").unwrap(),
            IdlessUpsertResult::Saved
        );
        let loser = entry("idless-loser-must-not-project", "duplicate-token");
        assert_eq!(
            store.upsert_idless(&loser, "idless-identity").unwrap(),
            IdlessUpsertResult::Duplicate {
                id: "idless-winner".into()
            }
        );
        assert_aligned(store.connection(), 2);
        let orphan_mapping: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memories_fts_projection_map WHERE memory_id = ?1",
                params![loser.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan_mapping, 0);
    }

    #[test]
    fn portable_v46_requires_authority_and_reaches_the_same_v47_invariant() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("portable-v47.db");
        let path_str = path.to_str().unwrap();
        {
            let context = DbOpenContext::create_fresh().with_profile(StoreProfile::PortableKernel);
            let mut store = MemoryStore::open_with_context(path_str, &context).unwrap();
            store.upsert(&entry("portable", "portable-token")).unwrap();
        }
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "DROP TABLE memories_fts_projection_map;
                 DELETE FROM hard_state WHERE namespace = 'migrations' AND key = 'v47_fts_projection_keys';
                 PRAGMA user_version = 46;",
            )
            .unwrap();
        let denied = MemoryStore::open_with_context(
            path_str,
            &DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel),
        );
        assert!(matches!(
            denied,
            Err(MemoryError::SchemaMigrationOptInRequired {
                stored: 46,
                expected: 47,
                ..
            })
        ));
        let store = MemoryStore::open_with_context(
            path_str,
            &DbOpenContext::open_existing_allow("test:portable-v47")
                .with_profile(StoreProfile::PortableKernel),
        )
        .unwrap();
        assert_eq!(
            super::super::read_schema_version(store.connection()).unwrap(),
            47
        );
        assert_aligned(store.connection(), 1);
        assert_eq!(
            term_hits(store.connection(), "memories_fts", "portable-token"),
            1
        );
        assert_eq!(
            term_hits(
                store.connection(),
                "memories_symbolic_fts",
                "portable-token"
            ),
            1
        );
    }
}
