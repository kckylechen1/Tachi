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

    crate::db::enable_simple_auto_extension().unwrap();
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

    crate::db::enable_simple_auto_extension().unwrap();
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

/// tachi#1446. `last_use_at` has to survive two hazards that a plain
/// `ensure_column` does not cover on its own, and both of them are silent:
///
/// 1. `migrate_enum_constraints` DROPs and re-CREATEs `memories` from a literal
///    column list in `rebuild_memories_with_check_constraints`, and it runs
///    *after* the `ensure_column` block in `init_schema_inner`. A column added
///    only by `ensure_column` would be created and then destroyed inside one
///    `init_schema` call, leaving `MEMORY_SELECT_COLUMNS` naming a column that
///    does not exist.
/// 2. That rebuild fires on **fresh** databases too, not just legacy ones —
///    `BASE_SCHEMA_SQL` creates `memories` without the CHECK constraints, so
///    the shape probe misses on first open.
///
/// Asserted on the legacy path (which starts without the column) and on the
/// fresh path (which starts with it), because the two reach the rebuild through
/// different branches of the guarded expression.
#[test]
fn last_use_at_survives_the_check_constraint_rebuild_on_both_paths() {
    let legacy = open_with_legacy_row("manual", "fact", "general", None, "/notes/x", "{}");
    assert!(
        !has_column(&legacy, "memories", "last_use_at").unwrap(),
        "fixture must start without the column, else this proves nothing"
    );
    // `let _` not `.unwrap()`: the auto-extension may already be enabled if a
    // sibling test in this process got there first, and this test has nothing
    // to say about that.
    let _ = crate::db::enable_simple_auto_extension();
    init_schema(&legacy).unwrap();
    assert!(
        has_column(&legacy, "memories", "last_use_at").unwrap(),
        "legacy DB must end up with last_use_at after init_schema"
    );
    let value: Option<String> = legacy
        .query_row(
            "SELECT last_use_at FROM memories WHERE id='row1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        value, None,
        "nothing in tachi#1446 commit 1 writes this column; a back-filled value would mean the \
         rebuild invented one"
    );

    let (fresh, _tmp) = sigil_1289_fresh_full_path();
    assert!(
        has_column(&fresh, "memories", "last_use_at").unwrap(),
        "fresh DB must carry last_use_at after the full init path"
    );

    let init_only = sigil_1289_init_schema_only();
    assert!(
        has_column(&init_only, "memories", "last_use_at").unwrap(),
        "the init_schema-only path must carry it too — this is the path a sentinel-migration-only \
         column would miss"
    );
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
    crate::db::enable_simple_auto_extension().unwrap();
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
    let (_tmp, path) = open_legacy_db_with_persons_and_location();

    super::test_hooks::arm_fail_after_legacy_work();
    let mut migration_conn = Connection::open(&path).expect("open migration fixture");
    let reference_guard = crate::db::register_reserved_reference_write_guard(&migration_conn)
        .expect("register reference guard");
    crate::db::install_reserved_reference_authorizer(&migration_conn, Some(&reference_guard))
        .expect("install authorizer");
    let migration_authorization =
        crate::db::authorize_schema_migration(&reference_guard).expect("authorize test migration");
    let err = super::init_schema_with_label_mut(
        &mut migration_conn,
        "global",
        &path,
        &crate::db::DbOpenContext::create_fresh(),
    )
    .expect_err("armed injection must fail init_schema_with_label_mut");
    drop(migration_authorization);
    drop(migration_conn);
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
    let mut migration_conn = Connection::open(&path).expect("reopen migration fixture");
    let reference_guard = crate::db::register_reserved_reference_write_guard(&migration_conn)
        .expect("register reference guard");
    crate::db::install_reserved_reference_authorizer(&migration_conn, Some(&reference_guard))
        .expect("install authorizer");
    let migration_authorization =
        crate::db::authorize_schema_migration(&reference_guard).expect("authorize test migration");
    super::init_schema_with_label_mut(
        &mut migration_conn,
        "global",
        &path,
        &crate::db::DbOpenContext::create_fresh(),
    )
    .expect("unarmed run must succeed");
    drop(migration_authorization);
    drop(migration_conn);
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

// ---------------------------------------------------------------------------
// #1289: schema-init ordering-escape regression + convergence oracle.
//
// Class of bug: `init_schema_inner` runs `execute_batch(BASE_SCHEMA_SQL)`
// (unconditional `CREATE TABLE/INDEX IF NOT EXISTS`) BEFORE the `ensure_column`
// + sentinel migrations that add "evolutionary" columns. On a fresh DB the
// `CREATE TABLE` builds every column so the indexes are fine — which is why CI
// (always fresh) stayed green. On a legacy DB the table already exists, so
// `CREATE TABLE IF NOT EXISTS` is a no-op and does NOT add the missing column;
// the immediately-following `CREATE INDEX` on that column then dies with
// `no such column`, rolling back the whole init transaction. The three escapes
// were `idx_access_hist_hash` (query_hash), `idx_exec_envs_claim` (v21
// claim_id), and `idx_session_claims_identity_active` (v21 mode predicate).
//
// The fixtures below use `user_version == 0` + `CreateFresh` deliberately: the
// ordering crash lives in `init_schema_inner`, which runs before and
// independent of the version-stamp gate, and because a hand-built fixture
// carries no migration sentinels the full v1..v21 sequence replays regardless
// of the stamp. That keeps the tests free of `.migration-bak`/marker side
// files while still exercising the real legacy-column-backfill path.

/// A pre-v21 legacy shape for the three tables #1289 touches: `access_history`
/// without `query_hash`, `exec_envs` without the v21 `agent_identity_id` /
/// `claim_id`, and `session_claims` without the v21 `mode` (and the rest of the
/// v21 spine columns). All other tables are left for `init_schema_inner` to
/// build fresh via `CREATE TABLE IF NOT EXISTS`.
const SIGIL_1289_PRE_V21_LEGACY_SQL: &str = r#"
    CREATE TABLE access_history (
        memory_id  TEXT NOT NULL,
        accessed_at TEXT NOT NULL
    );
    CREATE TABLE exec_envs (
        env_id         TEXT PRIMARY KEY,
        kind           TEXT NOT NULL DEFAULT 'worktree',
        path           TEXT NOT NULL,
        repo_root      TEXT NOT NULL DEFAULT '',
        branch         TEXT NOT NULL DEFAULT '',
        base_sha       TEXT NOT NULL DEFAULT '',
        dispatch_id    TEXT,
        env_class      TEXT NOT NULL DEFAULT 'edit-only',
        state          TEXT NOT NULL DEFAULT 'active',
        reclaim_reason TEXT,
        schema_version INTEGER NOT NULL DEFAULT 1,
        created_at     TEXT NOT NULL DEFAULT '',
        reclaimed_at   TEXT
    );
    CREATE TABLE session_claims (
        claim_id             TEXT PRIMARY KEY,
        session_client       TEXT,
        issue_ref            TEXT,
        flow_id              TEXT,
        dispatch_id          TEXT,
        branch               TEXT NOT NULL DEFAULT '',
        declared_file_scope  TEXT,
        state                TEXT NOT NULL DEFAULT 'active',
        release_reason       TEXT,
        created_at           TEXT NOT NULL DEFAULT '',
        heartbeat_at         TEXT NOT NULL DEFAULT '',
        released_at          TEXT
    );
"#;

fn sigil_1289_register_exts() {
    let _ = crate::db::enable_simple_auto_extension();
    crate::db::register_sqlite_vec();
}

fn sigil_1289_index_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1 LIMIT 1",
        params![name],
        |_| Ok(true),
    )
    .unwrap_or(false)
}

/// Build a file DB pre-seeded with `setup_sql` (a legacy table shape) on a bare
/// connection, then drive the FULL init path (`init_schema_with_label_mut` under
/// `CreateFresh`) over it with extensions registered. Panics if init fails —
/// which is exactly the #1289 crash before the fix.
fn sigil_1289_init_full_path_with_setup(setup_sql: &str) -> (Connection, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_path_buf();
    {
        let conn = Connection::open(&path).expect("open fixture");
        conn.execute_batch(setup_sql).expect("build legacy fixture");
    }
    sigil_1289_register_exts();
    let mut conn = Connection::open(&path).expect("reopen with extensions");
    let ctx = crate::db::DbOpenContext::create_fresh();
    crate::db::init_schema_with_label_mut(&mut conn, "global", &path, &ctx)
        .expect("init_schema_with_label_mut must succeed on a pre-v21 legacy DB (#1289)");
    (conn, tmp)
}

#[test]
fn pre_v21_legacy_db_boots_through_init_schema_inner() {
    // Before the fix this panics inside the helper: init_schema_inner's
    // BASE_SCHEMA index build hits `no such column` on the pre-v21 tables and
    // the whole init transaction rolls back. After the fix, init succeeds and
    // the three previously-crashing indexes are present.
    let (conn, _tmp) = sigil_1289_init_full_path_with_setup(SIGIL_1289_PRE_V21_LEGACY_SQL);
    for idx in [
        "idx_access_hist_hash",
        "idx_exec_envs_claim",
        "idx_session_claims_identity_active",
    ] {
        assert!(
            sigil_1289_index_exists(&conn, idx),
            "index {idx} must exist after booting a pre-v21 legacy DB (#1289)"
        );
    }
    assert_eq!(
        crate::db::migrations::read_schema_version(&conn).unwrap(),
        crate::db::migrations::EXPECTED_SCHEMA_VERSION,
        "a full-path boot of a legacy DB must end stamped at the current version"
    );
}

/// Legacy single-table shape for `exec_envs` at a v15..v20 era: it has
/// `env_class` (v15) but lacks the v21 `agent_identity_id` / `claim_id`.
const SIGIL_1289_LEGACY_EXEC_ENVS_SQL: &str = r#"
    CREATE TABLE exec_envs (
        env_id         TEXT PRIMARY KEY,
        kind           TEXT NOT NULL DEFAULT 'worktree',
        path           TEXT NOT NULL,
        repo_root      TEXT NOT NULL DEFAULT '',
        branch         TEXT NOT NULL DEFAULT '',
        base_sha       TEXT NOT NULL DEFAULT '',
        dispatch_id    TEXT,
        env_class      TEXT NOT NULL DEFAULT 'edit-only',
        state          TEXT NOT NULL DEFAULT 'active',
        reclaim_reason TEXT,
        schema_version INTEGER NOT NULL DEFAULT 1,
        created_at     TEXT NOT NULL DEFAULT '',
        reclaimed_at   TEXT
    );
"#;

/// Legacy single-table shape for `session_claims` before the v21 spine: no
/// `mode` (nor the rest of the v21 columns).
const SIGIL_1289_LEGACY_SESSION_CLAIMS_SQL: &str = r#"
    CREATE TABLE session_claims (
        claim_id             TEXT PRIMARY KEY,
        session_client       TEXT,
        issue_ref            TEXT,
        flow_id              TEXT,
        dispatch_id          TEXT,
        branch               TEXT NOT NULL DEFAULT '',
        declared_file_scope  TEXT,
        state                TEXT NOT NULL DEFAULT 'active',
        release_reason       TEXT,
        created_at           TEXT NOT NULL DEFAULT '',
        heartbeat_at         TEXT NOT NULL DEFAULT '',
        released_at          TEXT
    );
"#;

/// Full fresh path over an empty file: init_schema_inner + all v1..v21
/// migrations (unset sentinels), stamped at the current version.
fn sigil_1289_fresh_full_path() -> (Connection, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let path = tmp.path().to_path_buf();
    sigil_1289_register_exts();
    let mut conn = Connection::open(&path).expect("open fresh");
    let ctx = crate::db::DbOpenContext::create_fresh();
    crate::db::init_schema_with_label_mut(&mut conn, "global", &path, &ctx)
        .expect("fresh full-path init must succeed");
    (conn, tmp)
}

/// The migration-free `init_schema` path (owner ruling A: its product IS the
/// complete current schema).
fn sigil_1289_init_schema_only() -> Connection {
    sigil_1289_register_exts();
    let conn = Connection::open_in_memory().expect("in-memory");
    crate::db::init_schema(&conn).expect("init_schema must succeed");
    conn
}

/// Normalize a live DB into a comparable schema shape:
///   - table -> SET of column names (compared as a set: ALTER-appended columns
///     land in a different ordinal position than a fresh CREATE TABLE, so
///     ordinal is not a difference that matters here).
///   - named index -> normalized CREATE sql (lowercased, `IF NOT EXISTS`
///     stripped, whitespace collapsed) so the same index created via different
///     statements (BASE vs migration DROP+CREATE) compares equal.
///     FTS5 virtual/shadow tables and sqlite autoindexes are excluded: identical
///     across build paths and pure noise for the #1289 convergence question.
fn sigil_1289_normalized_schema(
    conn: &Connection,
) -> (
    std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    std::collections::BTreeMap<String, String>,
) {
    use std::collections::{BTreeMap, BTreeSet};

    let table_names: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type='table' AND name NOT LIKE 'sqlite_%' \
                   AND name NOT LIKE 'memories_fts%' \
                   AND name NOT LIKE 'memories_symbolic_fts%' \
                 ORDER BY name",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    let mut tables = BTreeMap::new();
    for t in table_names {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info(\"{t}\")"))
            .unwrap();
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<BTreeSet<_>, _>>()
            .unwrap();
        tables.insert(t, cols);
    }

    let mut indexes = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT name, sql FROM sqlite_master \
                 WHERE type='index' AND sql IS NOT NULL AND name NOT LIKE 'sqlite_%' \
                 ORDER BY name",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for (name, sql) in rows {
            let norm = sql
                .to_lowercase()
                .replace("if not exists ", "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            indexes.insert(name, norm);
        }
    }
    (tables, indexes)
}

#[test]
fn init_paths_converge_to_the_same_schema() {
    // (a) fresh full path, (b) init_schema-only, (c) pre-v21 legacy replay must
    // all land on the SAME normalized schema. This one net catches #1289 (path
    // c crashes without the fix) AND any evolutionary index that lives only in
    // a sentinel migration (path b would then be missing it — the class that
    // hid idx_hard_state_ns_updated).
    let (a_conn, _a_tmp) = sigil_1289_fresh_full_path();
    let b_conn = sigil_1289_init_schema_only();
    let (c_conn, _c_tmp) = sigil_1289_init_full_path_with_setup(SIGIL_1289_PRE_V21_LEGACY_SQL);

    let a = sigil_1289_normalized_schema(&a_conn);
    let b = sigil_1289_normalized_schema(&b_conn);
    let c = sigil_1289_normalized_schema(&c_conn);

    assert_eq!(
        a.0, b.0,
        "table column-sets differ: fresh-full (left) vs init_schema-only (right)"
    );
    assert_eq!(
        a.0, c.0,
        "table column-sets differ: fresh-full (left) vs legacy-replay (right)"
    );
    assert_eq!(
        a.1, b.1,
        "index definitions differ: fresh-full (left) vs init_schema-only (right)"
    );
    assert_eq!(
        a.1, c.1,
        "index definitions differ: fresh-full (left) vs legacy-replay (right)"
    );
}

#[test]
fn legacy_access_history_backfills_query_hash_index() {
    let (conn, _tmp) = sigil_1289_init_full_path_with_setup(
        "CREATE TABLE access_history (memory_id TEXT NOT NULL, accessed_at TEXT NOT NULL);",
    );
    assert!(
        has_column(&conn, "access_history", "query_hash").unwrap(),
        "query_hash must be ensured on a legacy access_history"
    );
    assert!(
        sigil_1289_index_exists(&conn, "idx_access_hist_hash"),
        "idx_access_hist_hash must exist after backfilling query_hash"
    );
}

#[test]
fn legacy_exec_envs_backfills_claim_index() {
    let (conn, _tmp) = sigil_1289_init_full_path_with_setup(SIGIL_1289_LEGACY_EXEC_ENVS_SQL);
    assert!(
        has_column(&conn, "exec_envs", "claim_id").unwrap(),
        "claim_id must be ensured on a legacy exec_envs"
    );
    assert!(
        has_column(&conn, "exec_envs", "agent_identity_id").unwrap(),
        "agent_identity_id must be ensured on a legacy exec_envs"
    );
    assert!(
        sigil_1289_index_exists(&conn, "idx_exec_envs_claim"),
        "idx_exec_envs_claim must exist after backfilling claim_id"
    );
}

#[test]
fn legacy_session_claims_backfills_identity_index() {
    let (conn, _tmp) = sigil_1289_init_full_path_with_setup(SIGIL_1289_LEGACY_SESSION_CLAIMS_SQL);
    assert!(
        has_column(&conn, "session_claims", "mode").unwrap(),
        "mode must be ensured on a legacy session_claims"
    );
    assert!(
        sigil_1289_index_exists(&conn, "idx_session_claims_identity_active"),
        "idx_session_claims_identity_active must exist after backfilling mode"
    );
}

/// Pre-v21 legacy `session_claims` (no `mode` column) carrying TWO active rows
/// with the SAME identity triple — the exact on-disk state a pre-#1001-round-2
/// kernel could leave, since its read-then-write upsert had no DB constraint to
/// prevent duplicate active rows. Before #1289's init-path dedup, booting this
/// DB crashes: `init_schema_inner`'s `MIGRATED_INDEXES_SQL` runs
/// `CREATE UNIQUE INDEX idx_session_claims_identity_active` over the two
/// un-deduped active rows and the whole init transaction rolls back.
const SIGIL_1289_LEGACY_SESSION_CLAIMS_DUP_SQL: &str = r#"
    CREATE TABLE session_claims (
        claim_id             TEXT PRIMARY KEY,
        session_client       TEXT,
        issue_ref            TEXT,
        flow_id              TEXT,
        dispatch_id          TEXT,
        branch               TEXT NOT NULL DEFAULT '',
        declared_file_scope  TEXT,
        state                TEXT NOT NULL DEFAULT 'active',
        release_reason       TEXT,
        created_at           TEXT NOT NULL DEFAULT '',
        heartbeat_at         TEXT NOT NULL DEFAULT '',
        released_at          TEXT
    );
    INSERT INTO session_claims
        (claim_id, session_client, issue_ref, flow_id, state, created_at, heartbeat_at)
    VALUES
        ('dup-old', 'claude-code', 'org/repo#7', 'flow-7', 'active',
         '2026-07-11T00:00:00Z', '2026-07-11T00:00:00Z'),
        ('dup-new', 'claude-code', 'org/repo#7', 'flow-7', 'active',
         '2026-07-11T00:10:00Z', '2026-07-11T00:10:00Z');
"#;

#[test]
fn legacy_session_claims_with_duplicate_active_rows_dedupes_and_boots() {
    // RED before the fix: init_schema_inner's MIGRATED_INDEXES_SQL builds the
    // partial UNIQUE index over the two un-deduped active rows and init dies
    // with a UNIQUE-constraint failure. GREEN after: the init-path dedup
    // (crate::db::migrations::dedupe_session_claims_identity_conflicts, called
    // right before the index build) collapses the older duplicate first, so
    // init succeeds, the index is built, and exactly one identity row stays
    // active.
    let (conn, _tmp) =
        sigil_1289_init_full_path_with_setup(SIGIL_1289_LEGACY_SESSION_CLAIMS_DUP_SQL);

    assert!(
        sigil_1289_index_exists(&conn, "idx_session_claims_identity_active"),
        "the partial UNIQUE index must be built after deduping legacy duplicates"
    );

    // Exactly one of the two same-identity rows survives active.
    let active_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_claims WHERE state = 'active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        active_count, 1,
        "duplicate active identity must be collapsed to one"
    );

    // The older heartbeat is the one released, with the migration's reason.
    let old_state: String = conn
        .query_row(
            "SELECT state FROM session_claims WHERE claim_id = 'dup-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        old_state, "released",
        "the older-heartbeat duplicate is released"
    );
    let old_reason: String = conn
        .query_row(
            "SELECT release_reason FROM session_claims WHERE claim_id = 'dup-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_reason, "superseded-by-unique-identity-migration");

    let new_state: String = conn
        .query_row(
            "SELECT state FROM session_claims WHERE claim_id = 'dup-new'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        new_state, "active",
        "the newest-heartbeat row survives as the single active claim"
    );

    // Re-running init over the now-deduped DB is a clean no-op (the index
    // already enforces uniqueness, so nothing is left to collapse and the
    // release_reason does not change).
    crate::db::init_schema(&conn).expect("second init over a deduped DB must succeed");
    let active_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_claims WHERE state = 'active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active_after, 1, "dedup is idempotent on a second init");
}

#[test]
fn init_schema_only_carries_all_evolutionary_indexes() {
    // owner ruling A: init_schema (no migrations) must produce the complete
    // current schema. The three relocated indexes plus idx_hard_state_ns_updated
    // (previously reachable only via the v13 migration) must all be present on
    // the migration-free path.
    let conn = sigil_1289_init_schema_only();
    for idx in [
        "idx_access_hist_hash",
        "idx_exec_envs_claim",
        "idx_session_claims_identity_active",
        "idx_hard_state_ns_updated",
    ] {
        assert!(
            sigil_1289_index_exists(&conn, idx),
            "init_schema (no migrations) must carry {idx} (#1289 ruling A)"
        );
    }
}

#[test]
fn legacy_recall_cache_rows_migrate_to_a_nonmatching_generation_fingerprint() {
    crate::db::enable_simple_auto_extension().expect("register simple tokenizer");
    crate::db::register_sqlite_vec();
    let conn = Connection::open_in_memory().expect("open legacy database");
    conn.execute_batch(
        r#"
        CREATE TABLE recall_cache (
            cache_id TEXT PRIMARY KEY,
            query TEXT NOT NULL DEFAULT '',
            rows_json TEXT NOT NULL DEFAULT '[]',
            result_count INTEGER NOT NULL DEFAULT 0,
            reranked INTEGER NOT NULL DEFAULT 0,
            hit_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        INSERT INTO recall_cache (
            cache_id, query, rows_json, result_count, created_at, updated_at
        ) VALUES (
            'legacy-cache', 'stale query', '[{"id":"stale"}]', 1,
            '2026-07-25T00:00:00Z', '2026-07-25T00:00:00Z'
        );
        "#,
    )
    .expect("create legacy recall cache");

    crate::db::init_schema(&conn).expect("migrate legacy recall cache");
    crate::db::init_schema(&conn).expect("migration must be idempotent");

    let fingerprint: String = conn
        .query_row(
            "SELECT generation_fingerprint FROM recall_cache WHERE cache_id = 'legacy-cache'",
            [],
            |row| row.get(0),
        )
        .expect("migrated generation fingerprint");
    assert_eq!(
        fingerprint, "",
        "legacy cache rows must never inherit the current generation as if fresh"
    );
}
