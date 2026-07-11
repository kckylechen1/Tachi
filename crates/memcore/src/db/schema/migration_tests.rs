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

// --- #984 F1 round 3: compatibility transaction widened to cover
// init_schema_inner's legacy v6/v8/v9 work -----------------------------

/// Build a real (file-backed, not in-memory) legacy-shape DB carrying BOTH a
/// non-empty `persons` column (drives the standalone v6 fold + v8 drop inside
/// `init_schema_inner`, via `fold_and_drop_legacy_persons_column`) and a
/// non-empty `location` column (drives the standalone v9 relocate + drop),
/// so the fault-injection test below has real, observable legacy work to
/// roll back — not a no-op.
fn open_legacy_db_with_persons_and_location() -> (tempfile::NamedTempFile, std::path::PathBuf) {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_path_buf();
    let conn = Connection::open(&path).expect("open");
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
               VALUES ('row1', '/notes/x', '', 'hello', 0.5, '2026-04-30T00:00:00Z',
                       'fact', '', '[]', ?1, '[]', ?2, 'manual', 'general', 0,
                       '2026-04-30T00:00:00Z', '2026-04-30T00:00:00Z', 0, NULL, 1,
                       '{}', NULL, NULL)"#,
        params![r#"["Kyle"]"#, "/scratch/legacy-location"],
    )
    .unwrap();
    drop(conn);
    (tmp, path)
}

/// Fault-injection proof (#984 F1 round 3, the bug codex found in round 2):
/// arm the `init_schema_with_label_mut` test hook to fail right after
/// `init_schema_inner` (DDL + the standalone v6/v8/v9 legacy-column work)
/// completes but before `run_data_migrations_in_tx` and the version stamp
/// run. Assert the legacy schema/data — `persons` column, `location` column,
/// and the row's un-relocated `location` value — are ALL still there
/// (rolled back), the sentinel migrations never ran, and `user_version` is
/// unchanged. Then a real (unarmed) run must apply and stamp cleanly.
#[test]
fn legacy_column_work_rolls_back_with_stamp_on_injected_failure() {
    let (tmp, path) = open_legacy_db_with_persons_and_location();
    let path_str = tmp.path().to_str().expect("utf8 tmp path").to_string();

    super::test_hooks::arm_fail_after_legacy_work();
    // Match rather than expect_err: MemoryStore (the Ok variant) is not
    // Debug, and we don't want to derive Debug on a struct holding live
    // connections (see the round-2 fixup for the same pattern elsewhere in
    // this test suite).
    let err = match crate::MemoryStore::open_with_label(&path_str, "global") {
        Ok(_) => panic!("armed injection must fail init_schema_with_label_mut"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("injected failure"),
        "unexpected error: {err}"
    );

    // Re-open a raw connection to inspect post-failure state directly
    // (MemoryStore::open failed, so there's no live handle to reuse).
    let inspect = Connection::open(&path).expect("reopen for inspection");

    // 1. Legacy `persons` column must still exist — v6/v8's standalone drop
    //    must have rolled back, not just the sentinel-gated migrations.
    let has_persons: bool = inspect
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='persons' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(
        has_persons,
        "persons column must still exist after rollback — v6/v8 legacy work leaked outside the tx"
    );
    let persons: String = inspect
        .query_row("SELECT persons FROM memories WHERE id='row1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        persons, r#"["Kyle"]"#,
        "persons data must be unfolded (original value) after rollback"
    );

    // 2. Legacy `location` column must still exist and be un-relocated — v9's
    //    standalone relocate+drop must have rolled back too.
    let has_location: bool = inspect
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='location' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(
        has_location,
        "location column must still exist after rollback — v9 legacy work leaked outside the tx"
    );
    let (path_col, location): (String, String) = inspect
        .query_row(
            "SELECT path, location FROM memories WHERE id='row1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        path_col, "/notes/x",
        "path must be un-relocated after rollback"
    );
    assert_eq!(
        location, "/scratch/legacy-location",
        "location value must be intact after rollback"
    );

    // 3. Sentinel migrations must never have run (they're chronologically
    //    after the injection point, but assert explicitly for clarity).
    let sentinel_count: i64 = inspect
        .query_row(
            "SELECT COUNT(*) FROM hard_state WHERE namespace = 'migrations'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    assert_eq!(sentinel_count, 0, "no sentinel migration should have run");

    // 4. `user_version` must be unchanged (still the pre-migration default).
    assert_eq!(
        crate::db::migrations::read_schema_version(&inspect).unwrap(),
        0,
        "user_version must not advance when init_schema_inner's legacy work rolled back"
    );
    drop(inspect);

    // 5. A real (unarmed) run now proceeds cleanly: legacy work applied,
    //    migrations run, and the DB ends up stamped at the current version.
    let _store =
        crate::MemoryStore::open_with_label(&path_str, "global").expect("unarmed run must succeed");
    let verify = Connection::open(&path).expect("reopen to verify success run");
    let has_persons_after: bool = verify
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='persons' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(
        !has_persons_after,
        "persons column must be dropped after a real run"
    );
    assert_eq!(
        crate::db::migrations::read_schema_version(&verify).unwrap(),
        crate::db::migrations::EXPECTED_SCHEMA_VERSION,
        "user_version must be stamped after a real run"
    );
}
