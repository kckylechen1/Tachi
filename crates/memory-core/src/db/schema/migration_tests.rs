use super::*;
use rusqlite::params;

#[test]
fn schema_identifier_helpers_reject_dynamic_sql_identifiers() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE memories (id TEXT PRIMARY KEY)", [])
        .expect("create minimal table");

    let table_err = has_column(&conn, "memories; DROP TABLE memories", "id")
        .expect_err("dynamic table identifier should be rejected");
    assert!(table_err.to_string().contains("invalid SQL identifier"));

    let column_err = ensure_column(&conn, "memories", "bad; DROP", "TEXT")
        .expect_err("dynamic column identifier should be rejected");
    assert!(column_err.to_string().contains("invalid SQL identifier"));
}

fn open_with_legacy_row(
    source: &str,
    category: &str,
    scope: &str,
    retention: Option<&str>,
    path: &str,
    metadata: &str,
) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    // Build legacy-shape table without CHECK constraints, mirroring the
    // pre-migration schema.
    conn.execute_batch(
        r#"
            CREATE TABLE memories (
                id           TEXT PRIMARY KEY,
                path         TEXT NOT NULL DEFAULT '/',
                summary      TEXT NOT NULL DEFAULT '',
                text         TEXT NOT NULL DEFAULT '',
                importance   REAL NOT NULL DEFAULT 0.7,
                timestamp    TEXT NOT NULL,
                category     TEXT NOT NULL DEFAULT 'fact',
                topic        TEXT NOT NULL DEFAULT '',
                keywords     TEXT NOT NULL DEFAULT '[]',
                persons      TEXT NOT NULL DEFAULT '[]',
                entities     TEXT NOT NULL DEFAULT '[]',
                location     TEXT NOT NULL DEFAULT '',
                source       TEXT NOT NULL DEFAULT 'manual',
                scope        TEXT NOT NULL DEFAULT 'general',
                archived     INTEGER NOT NULL DEFAULT 0,
                created_at   TEXT NOT NULL DEFAULT '',
                updated_at   TEXT NOT NULL DEFAULT '',
                access_count INTEGER NOT NULL DEFAULT 0,
                last_access  TEXT,
                revision     INTEGER NOT NULL DEFAULT 1,
                metadata     TEXT NOT NULL DEFAULT '{}',
                retention_policy TEXT,
                domain       TEXT
            );
            CREATE VIRTUAL TABLE memories_fts USING fts5(
                id UNINDEXED, path, summary, text, keywords, entities,
                tokenize = 'unicode61'
            );
            "#,
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO memories
                (id, path, summary, text, importance, timestamp, category, topic,
                 keywords, persons, entities, location, source, scope, archived,
                 created_at, updated_at, access_count, last_access, revision,
                 metadata, retention_policy, domain)
               VALUES (?1, ?2, '', 'hello', 0.5, '2026-04-30T00:00:00Z', ?3, '',
                       '[]','[]','[]','', ?4, ?5, 0,
                       '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, NULL, 1,
                       ?6, ?7, NULL)"#,
        params!["row1", path, category, source, scope, metadata, retention],
    )
    .unwrap();
    conn
}

fn open_with_hypertachi_shape() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
            CREATE TABLE memories (
                id           TEXT PRIMARY KEY,
                path         TEXT NOT NULL DEFAULT '/',
                summary      TEXT NOT NULL DEFAULT '',
                text         TEXT NOT NULL DEFAULT '',
                importance   REAL NOT NULL DEFAULT 0.7,
                timestamp    TEXT NOT NULL,
                valid_from   TEXT NOT NULL DEFAULT '',
                valid_until  TEXT,
                category     TEXT NOT NULL DEFAULT 'fact',
                topic        TEXT NOT NULL DEFAULT '',
                keywords     TEXT NOT NULL DEFAULT '[]',
                indexed_tags TEXT NOT NULL DEFAULT '[]',
                entities     TEXT NOT NULL DEFAULT '[]',
                domain_key   TEXT NOT NULL DEFAULT '',
                source       TEXT NOT NULL DEFAULT 'manual',
                scope        TEXT NOT NULL DEFAULT 'general',
                archived     INTEGER NOT NULL DEFAULT 0,
                created_at   TEXT NOT NULL DEFAULT '',
                updated_at   TEXT NOT NULL DEFAULT '',
                access_count INTEGER NOT NULL DEFAULT 0,
                last_access  TEXT,
                revision     INTEGER NOT NULL DEFAULT 1,
                metadata     TEXT NOT NULL DEFAULT '{}',
                retention_policy TEXT,
                domain       TEXT,
                superseded_by  TEXT
            );
            CREATE VIRTUAL TABLE memories_fts USING fts5(
                id UNINDEXED, path, summary, text, keywords, entities,
                tokenize = 'unicode61'
            );
            "#,
    )
    .unwrap();
    conn.execute(
        r#"INSERT INTO memories
                (id, path, text, importance, timestamp, indexed_tags, domain_key)
               VALUES ('hypertachi-1', '/wiki/test', 'hypertachi bridge row', 0.7,
                       '2026-04-30T00:00:00Z', '["alice"]', 'finance')"#,
        [],
    )
    .unwrap();
    conn
}

#[test]
fn bridge_repairs_mistaken_domain_in_location() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
            CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL DEFAULT '/',
                summary TEXT NOT NULL DEFAULT '',
                text TEXT NOT NULL DEFAULT '',
                importance REAL NOT NULL DEFAULT 0.7,
                timestamp TEXT NOT NULL,
                valid_from TEXT NOT NULL DEFAULT '',
                valid_until TEXT,
                category TEXT NOT NULL DEFAULT 'fact',
                topic TEXT NOT NULL DEFAULT '',
                keywords TEXT NOT NULL DEFAULT '[]',
                persons TEXT NOT NULL DEFAULT '[]',
                entities TEXT NOT NULL DEFAULT '[]',
                location TEXT NOT NULL DEFAULT 'finance',
                source TEXT NOT NULL DEFAULT 'manual',
                scope TEXT NOT NULL DEFAULT 'general',
                archived INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT '',
                access_count INTEGER NOT NULL DEFAULT 0,
                last_access TEXT,
                revision INTEGER NOT NULL DEFAULT 1,
                metadata TEXT NOT NULL DEFAULT '{}',
                retention_policy TEXT,
                domain TEXT,
                superseded_by TEXT,
                CHECK (source IN ('manual','extraction','migration','auto','foundry_distill',
                    'foundry_recall_rerank_cache','handoff','kanban','wiki','ghost','ingest_event')
                    OR source LIKE 'external:%'),
                CHECK (category IN ('fact','decision','experience','preference','entity',
                    'other','kanban','handoff','ghost','wiki','guide','eval')),
                CHECK (scope IN ('user','project','general'))
            );
            CREATE VIRTUAL TABLE memories_fts USING fts5(
                id UNINDEXED, path, summary, text, keywords, entities,
                tokenize = 'unicode61'
            );
            "#,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories (id, path, text, importance, timestamp)
             VALUES ('repair-1', '/trading', 'row', 0.7, '2026-04-30T00:00:00Z')",
        [],
    )
    .unwrap();
    init_schema(&conn).expect("bridge should move mistaken domain out of location");
    let domain: Option<String> = conn
        .query_row("SELECT domain FROM memories WHERE id='repair-1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(domain.as_deref(), Some("finance"));
    let has_location_column: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='location' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(!has_location_column);
}

#[test]
fn bridge_hypertachi_columns_before_enum_migration() {
    let conn = open_with_hypertachi_shape();
    init_schema(&conn).expect("init_schema should bridge hypertachi columns");
    let has_persons_column: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='persons' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(!has_persons_column);
    let (keywords, domain): (String, Option<String>) = conn
        .query_row(
            "SELECT keywords, domain FROM memories WHERE id='hypertachi-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(keywords, r#"["alice"]"#);
    assert_eq!(domain.as_deref(), Some("finance"));
    let has_location_column: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='location' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(!has_location_column);
}

#[test]
fn migration_normalizes_legacy_garbage() {
    let conn = open_with_legacy_row(
        "test",
        "WeirdCat",
        "self",
        Some("garbage"),
        "/notes/foo",
        "{}",
    );
    // First run: performs full migration.
    init_schema(&conn).unwrap();
    let (source, category, scope, retention): (String, String, String, Option<String>) = conn
        .query_row(
            "SELECT source, category, scope, retention_policy FROM memories WHERE id='row1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(source, "manual");
    assert_eq!(category, "other");
    assert_eq!(scope, "general");
    assert_eq!(retention, None);
}

#[test]
fn migration_collapses_ghost_publisher_into_metadata() {
    let conn = open_with_legacy_row(
        "ghost:agent_x",
        "fact",
        "general",
        None,
        "/ghost/messages",
        "{}",
    );
    init_schema(&conn).unwrap();
    let (source, metadata): (String, String) = conn
        .query_row(
            "SELECT source, metadata FROM memories WHERE id='row1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(source, "ghost");
    let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(m["ghost"]["publisher"], "agent_x");
}

#[test]
fn migration_backfills_handoff_retention_to_pinned() {
    let conn = open_with_legacy_row("manual", "fact", "general", None, "/handoff/foo", "{}");
    init_schema(&conn).unwrap();
    let r: Option<String> = conn
        .query_row(
            "SELECT retention_policy FROM memories WHERE id='row1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(r.as_deref(), Some("pinned"));
}

#[test]
fn migration_rejects_invalid_scope_after_migration() {
    let conn = open_with_legacy_row("manual", "fact", "general", None, "/notes/x", "{}");
    init_schema(&conn).unwrap();
    // Now CHECK constraint should reject 'self'.
    let err = conn
        .execute(
            r#"INSERT INTO memories
                   (id, path, summary, text, importance, timestamp, category, topic,
                    keywords, entities, source, scope, archived,
                    created_at, updated_at, access_count, last_access, revision,
                    metadata, retention_policy, domain)
                   VALUES ('row2', '/notes/y', '', '', 0.5, '2026-04-30T00:00:00Z',
                           'fact', '', '[]','[]', 'manual', 'self', 0,
                           '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, NULL, 1,
                           '{}', NULL, NULL)"#,
            [],
        )
        .unwrap_err();
    assert!(
        err.to_string().to_ascii_lowercase().contains("check")
            || err.to_string().to_ascii_lowercase().contains("constraint"),
        "expected CHECK violation, got: {err}"
    );
}

#[test]
fn migration_updates_stale_enum_checks() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            r#"
            CREATE TABLE memories (
                id           TEXT PRIMARY KEY,
                path         TEXT NOT NULL DEFAULT '/',
                summary      TEXT NOT NULL DEFAULT '',
                text         TEXT NOT NULL DEFAULT '',
                importance   REAL NOT NULL DEFAULT 0.7,
                timestamp    TEXT NOT NULL,
                category     TEXT NOT NULL DEFAULT 'fact',
                topic        TEXT NOT NULL DEFAULT '',
                keywords     TEXT NOT NULL DEFAULT '[]',
                persons      TEXT NOT NULL DEFAULT '[]',
                entities     TEXT NOT NULL DEFAULT '[]',
                location     TEXT NOT NULL DEFAULT '',
                source       TEXT NOT NULL DEFAULT 'manual',
                scope        TEXT NOT NULL DEFAULT 'general',
                archived     INTEGER NOT NULL DEFAULT 0,
                created_at   TEXT NOT NULL DEFAULT '',
                updated_at   TEXT NOT NULL DEFAULT '',
                access_count INTEGER NOT NULL DEFAULT 0,
                last_access  TEXT,
                revision     INTEGER NOT NULL DEFAULT 1,
                metadata     TEXT NOT NULL DEFAULT '{}',
                retention_policy TEXT,
                domain       TEXT,
                CHECK (category IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki','guide','eval')),
                CHECK (scope IN ('user','project','general')),
                CHECK (
                    source IN ('manual','extraction','migration','auto','foundry_distill','handoff','kanban','wiki','ghost','ingest_event')
                    OR source LIKE 'external:%'
                )
            );
            CREATE VIRTUAL TABLE memories_fts USING fts5(
                id UNINDEXED, path, summary, text, keywords, entities,
                tokenize = 'unicode61'
            );
            INSERT INTO memories
                (id, path, summary, text, importance, timestamp, category, topic,
                 keywords, persons, entities, location, source, scope, archived,
                 created_at, updated_at, access_count, last_access, revision,
                 metadata, retention_policy, domain)
               VALUES ('row1', '/notes/x', '', 'hello', 0.5, '2026-04-30T00:00:00Z',
                       'fact', '', '[]','[]','[]','', 'manual', 'general', 0,
                       '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, NULL, 1,
                       '{}', NULL, NULL);
            "#,
        )
        .unwrap();

    libsimple::enable_auto_extension().unwrap();
    init_schema(&conn).unwrap();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(sql.contains("'guide'"));
    assert!(sql.contains("'foundry_recall_rerank_cache'"));
    conn.execute(
        r#"INSERT INTO memories
                   (id, path, summary, text, importance, timestamp, category, topic,
                    keywords, entities, source, scope, archived,
                    created_at, updated_at, access_count, last_access, revision,
                    metadata, retention_policy, domain)
                   VALUES ('row2', '/guide/fix/main/x', '', '', 0.5, '2026-04-30T00:00:00Z',
                           'guide', '', '[]','[]', 'foundry_recall_rerank_cache', 'project', 0,
                           '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, NULL, 1,
                           '{}', NULL, NULL)"#,
        [],
    )
    .unwrap();
}

#[test]
fn migration_adds_lifecycle_columns_when_rebuilding_legacy_table() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            r#"
            CREATE TABLE memories (
                id           TEXT PRIMARY KEY,
                path         TEXT NOT NULL DEFAULT '/',
                summary      TEXT NOT NULL DEFAULT '',
                text         TEXT NOT NULL DEFAULT '',
                importance   REAL NOT NULL DEFAULT 0.7,
                timestamp    TEXT NOT NULL,
                category     TEXT NOT NULL DEFAULT 'fact',
                topic        TEXT NOT NULL DEFAULT '',
                keywords     TEXT NOT NULL DEFAULT '[]',
                persons      TEXT NOT NULL DEFAULT '[]',
                entities     TEXT NOT NULL DEFAULT '[]',
                location     TEXT NOT NULL DEFAULT '',
                source       TEXT NOT NULL DEFAULT 'manual',
                scope        TEXT NOT NULL DEFAULT 'general',
                archived     INTEGER NOT NULL DEFAULT 0,
                created_at   TEXT NOT NULL DEFAULT '',
                updated_at   TEXT NOT NULL DEFAULT '',
                access_count INTEGER NOT NULL DEFAULT 0,
                last_access  TEXT,
                revision     INTEGER NOT NULL DEFAULT 1,
                metadata     TEXT NOT NULL DEFAULT '{}',
                retention_policy TEXT,
                domain       TEXT,
                CHECK (category IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki','guide','eval')),
                CHECK (scope IN ('user','project','general')),
                CHECK (
                    source IN ('manual','extraction','migration','auto','foundry_distill','handoff','kanban','wiki','ghost','ingest_event')
                    OR source LIKE 'external:%'
                )
            );
            CREATE VIRTUAL TABLE memories_fts USING fts5(
                id UNINDEXED, path, summary, text, keywords, entities,
                tokenize = 'unicode61'
            );
            INSERT INTO memories
                (id, path, summary, text, importance, timestamp, category, topic,
                 keywords, persons, entities, location, source, scope, archived,
                 created_at, updated_at, access_count, last_access, revision,
                 metadata, retention_policy, domain)
               VALUES ('row1', '/notes/x', '', 'hello', 0.5, '2026-04-30T00:00:00Z',
                       'fact', '', '[]','[]','[]','', 'manual', 'general', 0,
                       '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, NULL, 1,
                       '{}', NULL, NULL);
            "#,
        )
        .unwrap();

    libsimple::enable_auto_extension().unwrap();
    init_schema(&conn).unwrap();
    let (recall_count, query_diversity, tier): (i64, i64, String) = conn
        .query_row(
            "SELECT recall_count, query_diversity, tier FROM memories WHERE id='row1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(recall_count, 0);
    assert_eq!(query_diversity, 0);
    assert_eq!(tier, "raw");
}

#[test]
fn migration_is_idempotent() {
    let conn = open_with_legacy_row("manual", "fact", "general", None, "/notes/x", "{}");
    init_schema(&conn).unwrap();
    // Snapshot the table SQL.
    let sql1: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Run again — should be no-op.
    init_schema(&conn).unwrap();
    let sql2: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sql1, sql2);
    // Row still present.
    let cnt: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cnt, 1);
}

#[test]
fn migration_preserves_fts_rows() {
    let conn = open_with_legacy_row("manual", "fact", "general", None, "/notes/x", "{}");
    // Pre-populate FTS to confirm it survives.
    conn.execute(
        "INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
             VALUES ('row1', '/notes/x', '', 'hello', '', '')",
        [],
    )
    .unwrap();
    init_schema(&conn).unwrap();
    let cnt: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories_fts", [], |r| r.get(0))
        .unwrap();
    assert!(cnt >= 1);
}

#[test]
fn migration_repairs_partial_fts_drift() {
    libsimple::enable_auto_extension().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    conn.execute_batch(
        r#"
            INSERT INTO memories
                (id, path, summary, text, importance, timestamp, category, topic,
                 keywords, entities, source, scope, archived,
                 created_at, updated_at, access_count, revision, metadata)
            VALUES
                ('row1', '/notes/a', '', 'alpha', 0.5, '2026-04-30T00:00:00Z',
                 'fact', '', '[]', '[]', 'manual', 'general', 0,
                 '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, 1, '{}'),
                ('row2', '/notes/b', '', 'bravo', 0.5, '2026-04-30T00:00:00Z',
                 'fact', '', '[]', '[]', 'manual', 'general', 0,
                 '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, 1, '{}');
            INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
            VALUES
                ('row1', '/notes/a', '', 'alpha', '', ''),
                ('ghost', '/notes/ghost', '', 'ghost', '', '');
            "#,
    )
    .unwrap();

    init_schema(&conn).unwrap();

    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories_fts", [], |r| r.get(0))
        .unwrap();
    let orphan_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts f LEFT JOIN memories m ON m.id=f.id WHERE m.id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
    let missing_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories m LEFT JOIN memories_fts f ON f.id=m.id WHERE f.id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();

    assert_eq!(fts_count, 2);
    assert_eq!(orphan_count, 0);
    assert_eq!(missing_count, 0);
}
