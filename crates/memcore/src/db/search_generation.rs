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
const SEARCH_AFFECTING_UPDATE_COLUMNS: &str = "path, summary, text, importance, timestamp, valid_from, valid_until, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, revision, metadata, superseded_by, idless_identity, retention_policy, domain, tier";

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
    AFTER UPDATE OF path, summary, text, importance, timestamp, valid_from, valid_until, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, revision, metadata, superseded_by, idless_identity, retention_policy, domain, tier ON memories
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
"#;

/// Create the generation table and its mutation triggers, then reject any
/// pre-existing drift instead of silently treating a stale cache as clean.
///
/// This runs after legacy table rebuilds because SQLite drops triggers attached
/// to a rebuilt table. `CREATE ... IF NOT EXISTS` makes normal reopen/migration
/// idempotent; a malformed existing trigger is a loud open failure.
pub(crate) fn ensure_search_generation_schema(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch(GENERATION_SCHEMA_SQL)?;
    validate_search_generation_schema(conn).map(|_| ())
}

/// Return the persisted generation only when the complete trigger contract is
/// present. Read-only/degraded callers use this as a cache gate: any error must
/// bypass the cache rather than risk a stale hit.
pub fn search_generation(conn: &Connection) -> Result<i64, MemoryError> {
    validate_search_generation_schema(conn)
}

fn validate_search_generation_schema(conn: &Connection) -> Result<i64, MemoryError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_search_generation WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(MemoryError::InvalidArg(format!(
            "memory search generation must contain exactly one id=1 row, found {count}"
        )));
    }

    let generation: i64 = conn.query_row(
        "SELECT generation FROM memory_search_generation WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if !(0..=GENERATION_MAX).contains(&generation) {
        return Err(MemoryError::InvalidArg(format!(
            "memory search generation is out of range: {generation}"
        )));
    }

    for trigger in [INSERT_TRIGGER, UPDATE_TRIGGER, DELETE_TRIGGER] {
        let sql = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                params![trigger],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
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
        INSERT_TRIGGER => trigger_sql(INSERT_TRIGGER, "INSERT"),
        UPDATE_TRIGGER => trigger_sql(
            UPDATE_TRIGGER,
            &format!("UPDATE OF {SEARCH_AFFECTING_UPDATE_COLUMNS}"),
        ),
        DELETE_TRIGGER => trigger_sql(DELETE_TRIGGER, "DELETE"),
        _ => return false,
    };
    normalize_sql(actual) == normalize_sql(&expected)
}

fn trigger_sql(name: &str, operation: &str) -> String {
    format!(
        "CREATE TRIGGER {name} AFTER {operation} ON memories BEGIN \
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
            last_access: None,
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
            1,
            "search telemetry alone must not invalidate an otherwise unchanged result cache"
        );

        store
            .connection()
            .execute("UPDATE memories SET archived = 1 WHERE id = 'upsert'", [])
            .expect("archive update");
        assert_eq!(store.search_generation().expect("after archive"), 2);

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
        assert_eq!(
            store.search_generation().expect("after rollback"),
            2,
            "a rolled-back memory mutation must not publish a cache generation"
        );

        store
            .connection()
            .execute("DELETE FROM memories WHERE id = 'upsert'", [])
            .expect("delete");
        assert_eq!(store.search_generation().expect("after delete"), 3);
    }

    #[test]
    fn missing_or_drifted_trigger_refuses_generation_read() {
        let store = MemoryStore::open_in_memory().expect("open in-memory store");
        store
            .connection()
            .execute_batch("DROP TRIGGER memory_search_generation_after_update")
            .expect("drop trigger");

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

        let error = store
            .connection()
            .execute(
                "INSERT INTO memories (id, path, text, timestamp) VALUES ('overflow', '/scratch/generation/overflow', 'must not wrap', '2026-07-25T00:00:00Z')",
                [],
            )
            .expect_err("overflow must abort the write rather than wrap generation");
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
