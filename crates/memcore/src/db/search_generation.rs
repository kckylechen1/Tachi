//! Database-authoritative search-cache freshness generation.
//!
//! The recall cache is shared across processes, so cache correctness cannot
//! depend on a process-local invalidation counter. SQLite commits these trigger
//! updates with the memory mutation itself: rolled-back writes do not publish a
//! generation, and every committed change to `memories` advances it.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

const GENERATION_MAX: i64 = i64::MAX;

const INSERT_TRIGGER: &str = "memory_search_generation_after_insert";
const UPDATE_TRIGGER: &str = "memory_search_generation_after_update";
const DELETE_TRIGGER: &str = "memory_search_generation_after_delete";
const EDGE_INSERT_TRIGGER: &str = "memory_edge_search_generation_after_insert";
const EDGE_UPDATE_TRIGGER: &str = "memory_edge_search_generation_after_update";
const EDGE_DELETE_TRIGGER: &str = "memory_edge_search_generation_after_delete";
const ACCESS_INSERT_TRIGGER: &str = "memory_access_search_generation_after_insert";
const ACCESS_UPDATE_TRIGGER: &str = "memory_access_search_generation_after_update";
const ACCESS_DELETE_TRIGGER: &str = "memory_access_search_generation_after_delete";
const PREVIOUS_SEARCH_AFFECTING_UPDATE_COLUMNS: &str = "path, summary, text, importance, timestamp, valid_from, valid_until, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, revision, metadata, superseded_by, idless_identity, retention_policy, domain, tier";

const GENERATION_SCHEMA_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS memory_search_generation (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        generation INTEGER NOT NULL CHECK (generation >= 0)
    );
    INSERT OR IGNORE INTO memory_search_generation (id, generation) VALUES (1, 0);

    CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_insert
    AFTER INSERT ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_update
    AFTER UPDATE ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_delete
    AFTER DELETE ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_insert
    AFTER INSERT ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_update
    AFTER UPDATE ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_delete
    AFTER DELETE ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_insert
    AFTER INSERT ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_update
    AFTER UPDATE ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_delete
    AFTER DELETE ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;
"#;

/// Create the generation table and its mutation triggers, then reject any
/// pre-existing drift instead of silently treating a stale cache as clean.
///
/// This runs after legacy table rebuilds because SQLite drops triggers attached
/// to a rebuilt table. `CREATE ... IF NOT EXISTS` makes normal reopen/migration
/// idempotent; a malformed existing trigger is a loud open failure.
pub(crate) fn ensure_search_generation_schema(conn: &Connection) -> Result<(), MemoryError> {
    migrate_previous_memory_update_trigger(conn)?;
    conn.execute_batch(GENERATION_SCHEMA_SQL)?;
    validate_search_generation_schema(conn).map(|_| ())
}

/// The private schema-migration authorizer permits DDL only for this fixed
/// trigger inventory. Keeping the name-to-table mapping here makes the DDL
/// gate and post-open validation share the search-generation contract.
pub(crate) fn is_expected_search_generation_trigger_target(name: &str, table: &str) -> bool {
    matches!(
        (name, table),
        (INSERT_TRIGGER, "memories")
            | (UPDATE_TRIGGER, "memories")
            | (DELETE_TRIGGER, "memories")
            | (EDGE_INSERT_TRIGGER, "memory_edges")
            | (EDGE_UPDATE_TRIGGER, "memory_edges")
            | (EDGE_DELETE_TRIGGER, "memory_edges")
            | (ACCESS_INSERT_TRIGGER, "access_history")
            | (ACCESS_UPDATE_TRIGGER, "access_history")
            | (ACCESS_DELETE_TRIGGER, "access_history")
    )
}

pub(crate) fn is_canonical_search_generation_trigger(
    name: &str,
    table: &str,
    sql: Option<&str>,
) -> bool {
    is_expected_search_generation_trigger_target(name, table)
        && sql.is_some_and(|sql| normalizes_to_expected_trigger(name, sql))
}

fn migrate_previous_memory_update_trigger(conn: &Connection) -> Result<(), MemoryError> {
    let existing = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            [UPDATE_TRIGGER],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(existing) = existing else {
        return Ok(());
    };
    let previous = trigger_sql(
        UPDATE_TRIGGER,
        &format!("UPDATE OF {PREVIOUS_SEARCH_AFFECTING_UPDATE_COLUMNS}"),
        "memories",
    );
    if normalize_sql(&existing) == normalize_sql(&previous) {
        conn.execute("DROP TRIGGER memory_search_generation_after_update", [])?;
    }
    Ok(())
}

/// Return the persisted generation only when the complete trigger contract is
/// present. Read-only/degraded callers use this as a cache gate: any error must
/// bypass the cache rather than risk a stale hit.
pub fn search_generation(conn: &Connection) -> Result<i64, MemoryError> {
    validate_search_generation_schema(conn)
}

/// Advance the authoritative generation for a search projection that SQLite
/// cannot trigger directly (FTS/vec virtual tables). Callers must invoke this
/// on the same connection/transaction as the projection mutation; rollback
/// then rolls back both changes together.
pub fn bump_search_generation(conn: &Connection) -> Result<i64, MemoryError> {
    let generation = validate_search_generation_schema(conn)?;
    if generation == GENERATION_MAX {
        return Err(MemoryError::InvalidArg(
            "memory search generation exhausted".to_string(),
        ));
    }
    let changed = conn.execute(
        "UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1 AND generation < ?1",
        [GENERATION_MAX],
    )?;
    if changed != 1 {
        return Err(MemoryError::InvalidArg(
            "memory search generation row missing or exhausted".to_string(),
        ));
    }
    Ok(generation + 1)
}

fn validate_search_generation_schema(conn: &Connection) -> Result<i64, MemoryError> {
    let snapshot = conn
        .query_row(
            "SELECT generation,
                    (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?2),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?3),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?4),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?5),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?6),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?7),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?8),
                    (SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?9)
             FROM memory_search_generation
             WHERE id = 1
             LIMIT 1",
            params![
                INSERT_TRIGGER,
                UPDATE_TRIGGER,
                DELETE_TRIGGER,
                EDGE_INSERT_TRIGGER,
                EDGE_UPDATE_TRIGGER,
                EDGE_DELETE_TRIGGER,
                ACCESS_INSERT_TRIGGER,
                ACCESS_UPDATE_TRIGGER,
                ACCESS_DELETE_TRIGGER,
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    [
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ],
                ))
            },
        )
        .optional()?;
    let Some((generation, count, trigger_sql)) = snapshot else {
        return Err(MemoryError::InvalidArg(
            "memory search generation row id=1 is missing".to_string(),
        ));
    };
    if count != 1 {
        return Err(MemoryError::InvalidArg(format!(
            "memory search generation must contain exactly one id=1 row, found {count}"
        )));
    }

    if !(0..=GENERATION_MAX).contains(&generation) {
        return Err(MemoryError::InvalidArg(format!(
            "memory search generation is out of range: {generation}"
        )));
    }

    for (trigger, sql) in [
        INSERT_TRIGGER,
        UPDATE_TRIGGER,
        DELETE_TRIGGER,
        EDGE_INSERT_TRIGGER,
        EDGE_UPDATE_TRIGGER,
        EDGE_DELETE_TRIGGER,
        ACCESS_INSERT_TRIGGER,
        ACCESS_UPDATE_TRIGGER,
        ACCESS_DELETE_TRIGGER,
    ]
    .into_iter()
    .zip(trigger_sql)
    {
        let sql = sql.ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "memory search generation trigger '{trigger}' is missing; recall cache is unsafe"
            ))
        })?;
        if !normalizes_to_expected_trigger(trigger, &sql) {
            return Err(MemoryError::InvalidArg(format!(
                "memory search generation trigger '{trigger}' drifted; recall cache is unsafe"
            )));
        }
    }

    Ok(generation)
}

fn normalizes_to_expected_trigger(name: &str, actual: &str) -> bool {
    let expected = match name {
        INSERT_TRIGGER => trigger_sql(INSERT_TRIGGER, "INSERT", "memories"),
        UPDATE_TRIGGER => trigger_sql(UPDATE_TRIGGER, "UPDATE", "memories"),
        DELETE_TRIGGER => trigger_sql(DELETE_TRIGGER, "DELETE", "memories"),
        EDGE_INSERT_TRIGGER => trigger_sql(EDGE_INSERT_TRIGGER, "INSERT", "memory_edges"),
        EDGE_UPDATE_TRIGGER => trigger_sql(EDGE_UPDATE_TRIGGER, "UPDATE", "memory_edges"),
        EDGE_DELETE_TRIGGER => trigger_sql(EDGE_DELETE_TRIGGER, "DELETE", "memory_edges"),
        ACCESS_INSERT_TRIGGER => trigger_sql(ACCESS_INSERT_TRIGGER, "INSERT", "access_history"),
        ACCESS_UPDATE_TRIGGER => trigger_sql(ACCESS_UPDATE_TRIGGER, "UPDATE", "access_history"),
        ACCESS_DELETE_TRIGGER => trigger_sql(ACCESS_DELETE_TRIGGER, "DELETE", "access_history"),
        _ => return false,
    };
    normalize_sql(actual) == normalize_sql(&expected)
}

fn trigger_sql(name: &str, operation: &str, table: &str) -> String {
    format!(
        "CREATE TRIGGER {name} AFTER {operation} ON {table} BEGIN \
         SELECT CASE WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1 \
         THEN RAISE(ABORT, 'memory search generation row missing') \
         WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807 \
         THEN RAISE(ABORT, 'memory search generation exhausted') END; \
         UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1; END"
    )
}

fn normalize_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{MemoryEntry, MemoryStore};

    fn entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: format!("/scratch/generation/{id}"),
            summary: text.to_string(),
            text: text.to_string(),
            importance: 0.7,
            timestamp: "2026-07-25T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "manual".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn committed_memory_mutations_advance_generation_but_rollback_does_not_publish_one() {
        let mut store = MemoryStore::open_in_memory().expect("open in-memory store");
        assert_eq!(store.search_generation().expect("initial generation"), 0);

        store
            .upsert(&entry("upsert", "raw memcore upsert"))
            .expect("upsert");
        assert_eq!(store.search_generation().expect("after upsert"), 1);

        store
            .connection()
            .execute(
                "UPDATE memories SET access_count = access_count + 1 WHERE id = 'upsert'",
                [],
            )
            .expect("telemetry update");
        assert_eq!(
            store.search_generation().expect("after telemetry"),
            2,
            "access_count changes ranking and must advance cache generation"
        );

        let archive_authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize archive fixture mutation");
        store
            .connection()
            .execute("UPDATE memories SET archived = 1 WHERE id = 'upsert'", [])
            .expect("archive update");
        drop(archive_authorization);
        assert_eq!(store.search_generation().expect("after archive"), 3);

        let rollback_authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize rollback fixture memory write");
        let tx = store
            .connection_mut()
            .transaction()
            .expect("begin rollback probe");
        tx.execute(
            "INSERT INTO memories (id, path, text, timestamp) VALUES ('rolled-back', '/scratch/generation/rollback', 'rolled back', '2026-07-25T00:00:00Z')",
            [],
        )
        .expect("insert inside transaction");
        tx.rollback().expect("rollback generation probe");
        drop(rollback_authorization);
        assert_eq!(
            store.search_generation().expect("after rollback"),
            3,
            "a rolled-back memory mutation must not publish a cache generation"
        );

        store
            .connection()
            .execute("DELETE FROM memories WHERE id = 'upsert'", [])
            .expect("delete");
        assert_eq!(store.search_generation().expect("after delete"), 4);
    }

    #[test]
    fn missing_or_drifted_trigger_refuses_generation_read() {
        let store = MemoryStore::open_in_memory().expect("open in-memory store");
        let migration_authorization =
            crate::db::authorize_schema_migration(&store.reserved_reference_write)
                .expect("authorize missing-trigger fixture");
        store
            .connection()
            .execute_batch("DROP TRIGGER memory_search_generation_after_update")
            .expect("drop trigger");
        drop(migration_authorization);

        let error = store
            .search_generation()
            .expect_err("missing trigger must fail closed");
        assert!(
            error
                .to_string()
                .contains("trigger 'memory_search_generation_after_update' is missing"),
            "unexpected generation safety error: {error}"
        );
    }

    #[test]
    fn exhausted_generation_aborts_the_memory_write_without_wrapping() {
        let store = MemoryStore::open_in_memory().expect("open in-memory store");
        store
            .connection()
            .execute(
                "UPDATE memory_search_generation SET generation = ?1 WHERE id = 1",
                [i64::MAX],
            )
            .expect("set exhaustion fixture");

        let write_authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize exhaustion fixture memory write");
        let error = store
            .connection()
            .execute(
                "INSERT INTO memories (id, path, text, timestamp) VALUES ('overflow', '/scratch/generation/overflow', 'must not wrap', '2026-07-25T00:00:00Z')",
                [],
            )
            .expect_err("overflow must abort the write rather than wrap generation");
        drop(write_authorization);
        assert!(
            error
                .to_string()
                .contains("memory search generation exhausted"),
            "unexpected generation overflow error: {error}"
        );
        assert_eq!(
            store
                .search_generation()
                .expect("generation remains readable"),
            i64::MAX,
            "the failed write must not wrap or publish a new generation"
        );
    }
}
