//! #1974: `ensure_fts_backfilled` runs on every writable open, so it must be
//! linear in the number of memories and must keep its insert-missing /
//! delete-stale drift-repair semantics (including NULL ids on both sides).

use super::*;
use rusqlite::{params, Connection};
use std::time::{Duration, Instant};

/// The pre-#1974 statement shape. Kept only so the plan test can prove that
/// its `CORRELATED` probe actually discriminates on this SQLite build.
const LEGACY_CORRELATED_BACKFILL_SQL: &str = r#"INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
   SELECT m.id, m.path, m.summary, m.text, m.keywords, m.entities
   FROM memories m
   WHERE NOT EXISTS (SELECT 1 FROM memories_fts f WHERE f.id = m.id)"#;

fn fresh_conn() -> Connection {
    crate::db::enable_simple_auto_extension().expect("register simple tokenizer");
    crate::db::register_sqlite_vec();
    let conn = Connection::open_in_memory().expect("open in-memory");
    init_schema(&conn).expect("init schema");
    conn
}

fn insert_memory(conn: &Connection, id: Option<&str>, text: &str) {
    conn.execute(
        r#"INSERT INTO memories
               (id, path, summary, text, importance, timestamp, category, topic,
                keywords, entities, source, scope, archived,
                created_at, updated_at, access_count, revision, metadata)
           VALUES
               (?1, '/notes/fts', '', ?2, 0.5, '2026-09-26T00:00:00Z',
                'fact', 'topic', '["kw"]', '["ent"]', 'manual', 'general', 0,
                '2026-09-26T00:00:00Z', '2026-09-26T00:00:00Z', 0, 1, '{}')"#,
        params![id, text],
    )
    .expect("insert memory");
}

/// Bring both FTS projections fully in sync with `memories` (non-NULL ids).
fn project_all(conn: &Connection) {
    conn.execute_batch(
        r#"
        INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
        SELECT id, path, summary, text, keywords, entities
        FROM memories WHERE id IS NOT NULL;
        INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
        SELECT id, path, summary, text, keywords, entities, topic
        FROM memories WHERE id IS NOT NULL;
        "#,
    )
    .expect("project memories into FTS tables");
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).expect(sql)
}

fn fts_ids(conn: &Connection, table: &str) -> Vec<Option<String>> {
    let mut stmt = conn
        .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
        .expect("prepare id listing");
    let rows = stmt
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .expect("list ids");
    rows.collect::<Result<Vec<_>, _>>().expect("collect ids")
}

fn generation(conn: &Connection) -> i64 {
    crate::db::search_generation::search_generation(conn).expect("read search generation")
}

fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare EXPLAIN QUERY PLAN");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(3))
        .expect("run EXPLAIN QUERY PLAN");
    rows.collect::<Result<Vec<_>, _>>().expect("collect plan")
}

#[test]
fn backfill_on_in_sync_10k_store_is_linear() {
    const N: i64 = 10_000;
    // Generous for an unoptimized debug build under parallel test load; the
    // pre-#1974 correlated probe needs minutes at this size.
    const BOUND: Duration = Duration::from_secs(2);

    let conn = fresh_conn();
    conn.execute(
        r#"WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ?1)
           INSERT INTO memories
               (id, path, summary, text, importance, timestamp, category, topic,
                keywords, entities, source, scope, archived,
                created_at, updated_at, access_count, revision, metadata)
           SELECT
               printf('mem-%06d', n), '/notes/bulk', 'summary ' || n,
               'body text number ' || n || ' with some searchable words',
               0.5, '2026-09-26T00:00:00Z', 'fact', 'topic', '["kw"]', '["ent"]',
               'manual', 'general', 0,
               '2026-09-26T00:00:00Z', '2026-09-26T00:00:00Z', 0, 1, '{}'
           FROM seq"#,
        [N],
    )
    .expect("seed memories");
    project_all(&conn);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories"), N);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories_fts"), N);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM memories_symbolic_fts"),
        N
    );
    let generation_before = generation(&conn);

    let started = Instant::now();
    ensure_fts_backfilled(&conn).expect("backfill in-sync store");
    let elapsed = started.elapsed();
    eprintln!("ensure_fts_backfilled on in-sync {N}-row store: {elapsed:?}");

    assert!(
        elapsed < BOUND,
        "ensure_fts_backfilled took {elapsed:?} on an in-sync {N}-row store (bound {BOUND:?})"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories_fts"), N);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM memories_symbolic_fts"),
        N
    );
    assert_eq!(
        generation(&conn),
        generation_before,
        "an in-sync store must not bump the search generation"
    );
}

#[test]
fn backfill_statements_plan_without_correlated_subquery() {
    let conn = fresh_conn();

    let legacy = query_plan(&conn, LEGACY_CORRELATED_BACKFILL_SQL);
    assert!(
        legacy.iter().any(|line| line.contains("CORRELATED")),
        "probe must discriminate: legacy NOT EXISTS plan was {legacy:?}"
    );

    for sql in [
        FTS_BACKFILL_MISSING_SQL,
        SYMBOLIC_FTS_BACKFILL_MISSING_SQL,
        FTS_DELETE_ORPHANS_SQL,
        SYMBOLIC_FTS_DELETE_ORPHANS_SQL,
    ] {
        let plan = query_plan(&conn, sql);
        assert!(!plan.is_empty(), "empty plan for {sql}");
        assert!(
            plan.iter().all(|line| !line.contains("CORRELATED")),
            "backfill must not probe the FTS table per memory row; plan: {plan:?}"
        );
    }
}

#[test]
fn backfill_repairs_missing_and_stale_projection_rows() {
    let conn = fresh_conn();
    for (id, text) in [("a", "alpha"), ("b", "bravo"), ("c", "charlie")] {
        insert_memory(&conn, Some(id), text);
    }
    project_all(&conn);

    // Drift: one missing FTS row, one missing symbolic row, one stale row in each.
    conn.execute_batch(
        r#"
        DELETE FROM memories_fts WHERE id = 'b';
        DELETE FROM memories_symbolic_fts WHERE id = 'c';
        INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
            VALUES ('ghost', '/g', '', 'ghost', '', '');
        INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
            VALUES ('ghost', '/g', '', 'ghost', '', '', '');
        "#,
    )
    .unwrap();
    let generation_before = generation(&conn);

    ensure_fts_backfilled(&conn).expect("repair drift");

    let all = vec![Some("a".into()), Some("b".into()), Some("c".into())];
    assert_eq!(fts_ids(&conn, "memories_fts"), all);
    assert_eq!(fts_ids(&conn, "memories_symbolic_fts"), all);
    let (fts_text, fts_keywords): (String, String) = conn
        .query_row(
            "SELECT text, keywords FROM memories_fts WHERE id = 'b'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(fts_text, "bravo");
    assert_eq!(
        fts_keywords, "kw",
        "memories_fts keeps the JSON-stripped keywords"
    );
    let (sym_text, sym_keywords, sym_topic): (String, String, String) = conn
        .query_row(
            "SELECT text, keywords, topic FROM memories_symbolic_fts WHERE id = 'c'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(sym_text, "charlie");
    assert_eq!(sym_keywords, r#"["kw"]"#, "symbolic keeps raw column bytes");
    assert_eq!(sym_topic, "topic");
    assert_eq!(generation(&conn), generation_before + 1);

    // Converged: a second pass changes nothing and does not bump.
    ensure_fts_backfilled(&conn).expect("second pass");
    assert_eq!(fts_ids(&conn, "memories_fts"), all);
    assert_eq!(fts_ids(&conn, "memories_symbolic_fts"), all);
    assert_eq!(generation(&conn), generation_before + 1);
}

/// A NULL `memories.id` (legal: `TEXT PRIMARY KEY` without NOT NULL) is never
/// projected. The pre-#1974 `NOT EXISTS (... f.id = m.id)` never matched
/// NULL, so it appended another NULL-id FTS row and bumped the generation on
/// every open. The `m.id IS NOT NULL` guard also covers the empty-projection
/// case, where `NULL NOT IN (<empty>)` is TRUE.
#[test]
fn backfill_never_projects_null_memory_ids() {
    let conn = fresh_conn();
    insert_memory(&conn, None, "null id row");
    insert_memory(&conn, Some("real"), "real row");
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM memories WHERE id IS NULL"),
        1,
        "fixture must hold a NULL-id memory"
    );
    // Projections start empty: exercises `NULL NOT IN (<empty set>)`.
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM memories_fts"), 0);
    let generation_before = generation(&conn);

    ensure_fts_backfilled(&conn).expect("first pass");

    assert_eq!(fts_ids(&conn, "memories_fts"), vec![Some("real".into())]);
    assert_eq!(
        fts_ids(&conn, "memories_symbolic_fts"),
        vec![Some("real".into())]
    );
    assert_eq!(generation(&conn), generation_before + 1);

    // Repeated opens stay converged: no NULL-id rows accrue, no bump.
    for _ in 0..3 {
        ensure_fts_backfilled(&conn).expect("repeat pass");
    }
    assert_eq!(fts_ids(&conn, "memories_fts"), vec![Some("real".into())]);
    assert_eq!(
        fts_ids(&conn, "memories_symbolic_fts"),
        vec![Some("real".into())]
    );
    assert_eq!(generation(&conn), generation_before + 1);
}

/// A NULL-id row inside an FTS table must not suppress repair of real rows:
/// without `WHERE id IS NOT NULL` in the subquery, `m.id NOT IN (..., NULL)`
/// is NULL for every memory and nothing would ever be reinserted.
#[test]
fn null_id_projection_row_does_not_suppress_repair() {
    let conn = fresh_conn();
    insert_memory(&conn, Some("a"), "alpha");
    insert_memory(&conn, Some("b"), "bravo");
    project_all(&conn);
    conn.execute_batch(
        r#"
        DELETE FROM memories_fts WHERE id = 'b';
        DELETE FROM memories_symbolic_fts WHERE id = 'b';
        INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
            VALUES (NULL, '/n', '', 'null fts row', '', '');
        INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
            VALUES (NULL, '/n', '', 'null fts row', '', '', '');
        "#,
    )
    .unwrap();
    let generation_before = generation(&conn);

    ensure_fts_backfilled(&conn).expect("repair despite NULL-id FTS row");

    for table in ["memories_fts", "memories_symbolic_fts"] {
        let ids: Vec<String> = fts_ids(&conn, table).into_iter().flatten().collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()], "{table}");
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM {table} WHERE id IS NOT NULL")
            ),
            2,
            "{table}: real rows repaired exactly once"
        );
    }
    assert_eq!(generation(&conn), generation_before + 1);
}

/// Seed projection rows with the given ids (`None` = NULL) into `table`,
/// each pointing at no memory.
fn seed_projection_rows(conn: &Connection, table: &str, ids: &[Option<&str>]) {
    let sql = if table == "memories_symbolic_fts" {
        "INSERT INTO memories_symbolic_fts (id, path, summary, text, keywords, entities, topic)
             VALUES (?1, '/orphan', '', 'orphan', '', '', '')"
    } else {
        "INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
             VALUES (?1, '/orphan', '', 'orphan', '', '')"
    };
    for id in ids {
        conn.execute(sql, params![id])
            .expect("seed orphan projection row");
    }
}

const KEEPER_SENTINEL: &str = "keeper projection row, never rewritten";

/// Mark the live `keep` projection row so the test can tell "kept" from
/// "deleted, then re-projected by the insert-missing pass": a re-projection
/// writes `memories.text`, not this sentinel.
fn mark_keeper(conn: &Connection, table: &str) {
    let changed = conn
        .execute(
            &format!("UPDATE {table} SET text = ?1 WHERE id = 'keep'"),
            params![KEEPER_SENTINEL],
        )
        .expect("mark keeper projection row");
    assert_eq!(changed, 1, "{table}: exactly one keeper row");
}

fn assert_keeper_kept(conn: &Connection, table: &str) {
    let text: String = conn
        .query_row(
            &format!("SELECT text FROM {table} WHERE id = 'keep'"),
            [],
            |r| r.get(0),
        )
        .expect("keeper projection row present");
    assert_eq!(
        text, KEEPER_SENTINEL,
        "{table}: the live row must be kept, not deleted and re-projected"
    );
}

/// tachi#1993: the orphan pass must still prune `table` while a NULL-id
/// memory exists, must drop NULL-id projection rows, and must keep the live
/// row. Without `WHERE id IS NOT NULL` in the subquery, `id NOT IN (..., NULL)`
/// is NULL for every FTS row, so neither the orphan nor the NULL-id row goes.
fn assert_orphans_pruned_despite_null_memory_id(table: &str, other: &str) {
    let conn = fresh_conn();
    insert_memory(&conn, None, "null id memory");
    insert_memory(&conn, Some("keep"), "live row");
    project_all(&conn);
    mark_keeper(&conn, table);
    mark_keeper(&conn, other);
    seed_projection_rows(&conn, table, &[Some("ghost"), None]);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM memories WHERE id IS NULL"),
        1,
        "fixture must hold a NULL-id memory"
    );
    assert_eq!(
        fts_ids(&conn, table),
        vec![None, Some("ghost".into()), Some("keep".into())],
        "{table}: fixture holds a NULL-id row, an orphan, and the keeper"
    );
    let generation_before = generation(&conn);

    ensure_fts_backfilled(&conn).expect("prune orphans despite a NULL memories.id");

    assert_eq!(
        fts_ids(&conn, table),
        vec![Some("keep".into())],
        "{table}: orphan and NULL-id rows pruned, the live row kept"
    );
    assert_eq!(
        fts_ids(&conn, other),
        vec![Some("keep".into())],
        "{other}: the unpoisoned projection is left as it was"
    );
    assert_keeper_kept(&conn, table);
    assert_keeper_kept(&conn, other);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM memories"),
        2,
        "the orphan pass never deletes memories, NULL-id ones included"
    );
    assert_eq!(generation(&conn), generation_before + 1);

    // Converged: the NULL-id memory is not re-projected, so the next open
    // neither re-deletes a NULL-id row nor bumps the generation.
    ensure_fts_backfilled(&conn).expect("second pass");
    assert_eq!(fts_ids(&conn, table), vec![Some("keep".into())]);
    assert_eq!(fts_ids(&conn, other), vec![Some("keep".into())]);
    assert_eq!(generation(&conn), generation_before + 1);
}

#[test]
fn orphan_fts_rows_pruned_despite_null_memory_id() {
    assert_orphans_pruned_despite_null_memory_id("memories_fts", "memories_symbolic_fts");
}

#[test]
fn orphan_symbolic_fts_rows_pruned_despite_null_memory_id() {
    assert_orphans_pruned_despite_null_memory_id("memories_symbolic_fts", "memories_fts");
}

/// tachi#1993: a NULL-id projection row is an orphan even when no memory has
/// a NULL id. Every search leg joins `m.id = <fts>.id`, which never matches
/// NULL, so such a row is unreachable and only skews BM25 corpus statistics.
/// The subquery guard alone does not remove it (`NULL NOT IN (<non-empty>)`
/// is NULL); that takes the explicit `id IS NULL` arm.
#[test]
fn null_id_projection_rows_are_pruned() {
    let conn = fresh_conn();
    insert_memory(&conn, Some("keep"), "live row");
    project_all(&conn);
    for table in ["memories_fts", "memories_symbolic_fts"] {
        mark_keeper(&conn, table);
        seed_projection_rows(&conn, table, &[None]);
        assert_eq!(
            fts_ids(&conn, table),
            vec![None, Some("keep".into())],
            "{table}: fixture holds a NULL-id row and the keeper"
        );
    }
    let generation_before = generation(&conn);

    ensure_fts_backfilled(&conn).expect("prune NULL-id projection rows");

    for table in ["memories_fts", "memories_symbolic_fts"] {
        assert_eq!(
            fts_ids(&conn, table),
            vec![Some("keep".into())],
            "{table}: NULL-id row pruned, the live row kept"
        );
        assert_keeper_kept(&conn, table);
    }
    assert_eq!(generation(&conn), generation_before + 1);
}
