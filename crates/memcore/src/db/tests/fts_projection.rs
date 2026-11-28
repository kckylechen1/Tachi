use super::make_entry;
use crate::MemoryStore;
use rusqlite::{params, Connection};
use std::path::PathBuf;

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    store: MemoryStore,
    terms: (String, String),
    symbolic: (String, String),
}

fn projection(conn: &Connection, table: &str) -> (String, String) {
    conn.query_row(
        &format!("SELECT keywords, entities FROM {table} WHERE id = 'projection'"),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("projection row")
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("scratch store");
    let path = dir.path().join("projection.db");
    let mut store = MemoryStore::open(path.to_str().unwrap()).expect("fresh store");
    let mut entry = make_entry("projection", "neutral body");
    entry.keywords = [
        "alpha",
        "beta",
        "comma,tail",
        "quote\"word",
        "back\\slash",
        "KeywordHead\nKeywordTail",
        "[brackets]",
        "",
        " edge ",
    ]
    .map(str::to_owned)
    .to_vec();
    entry.entities = ["EntityHead\nEntityTail", "entity\"quote", "entity\\slash"]
        .map(str::to_owned)
        .to_vec();
    let terms = (entry.keywords.join(" "), entry.entities.join(" "));
    let symbolic = (
        serde_json::to_string(&entry.keywords).unwrap(),
        serde_json::to_string(&entry.entities).unwrap(),
    );
    store.upsert(&entry).expect("CRUD projection");
    let fixture = Fixture {
        _dir: dir,
        path,
        store,
        terms,
        symbolic,
    };
    assert_projection(&fixture);
    fixture
}

fn assert_projection(fixture: &Fixture) {
    let conn = fixture.store.connection();
    // Column-restricted MATCH proves these terms come from the arrays, not
    // the neutral body. JSON's escaped newline used to index nKeywordTail.
    for query in [
        "keywords:alpha",
        "keywords:KeywordTail",
        "entities:EntityTail",
    ] {
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH ?1 AND id = ?2",
                params![query, "projection"],
                |row| row.get(0),
            )
            .expect("real MATCH");
        assert_eq!(hits, 1, "decoded term must remain searchable: {query}");
    }
    assert_eq!(projection(conn, "memories_fts"), fixture.terms);
    assert_eq!(
        projection(conn, "memories_symbolic_fts"),
        fixture.symbolic,
        "symbolic trigram projection deliberately retains raw JSON"
    );
}

#[test]
fn fts_projection_matches_crud_after_missing_backfill() {
    let mut f = fixture();
    f.store
        .connection()
        .execute("DELETE FROM memories_fts", [])
        .unwrap();
    assert_eq!(f.store.backfill_fts_missing().expect("missing backfill"), 1);
    assert_projection(&f);
    assert_eq!(
        f.store.backfill_fts_missing().expect("idempotent backfill"),
        0
    );
    assert_projection(&f);
    drop(f.store);
    f.store = MemoryStore::open_existing_read_write(f.path.to_str().unwrap())
        .expect("maintenance handle without schema init");
    f.store
        .connection()
        .execute("DELETE FROM memories_fts", [])
        .unwrap();
    assert_eq!(
        f.store
            .backfill_fts_missing()
            .expect("maintenance backfill"),
        1
    );
    assert_projection(&f);
}

#[test]
fn fts_projection_matches_crud_after_writable_reopen() {
    let mut f = fixture();
    f.store
        .connection()
        .execute("DELETE FROM memories_fts", [])
        .unwrap();
    drop(f.store);
    f.store = MemoryStore::open(f.path.to_str().unwrap()).expect("open-time backfill");
    assert_projection(&f);
}

#[test]
fn fts_projection_matches_crud_after_full_rebuild() {
    let mut f = fixture();
    assert_eq!(f.store.rebuild_fts_full().expect("full rebuild"), 1);
    assert_projection(&f);
}

#[test]
fn fts_projection_matches_crud_after_revision_update() {
    let mut f = fixture();
    assert!(f
        .store
        .update_with_revision(
            "projection",
            "neutral revised body",
            "neutral summary",
            "manual",
            &serde_json::json!({}),
            None,
            1
        )
        .expect("revision update"));
    assert_projection(&f);
}

#[test]
fn fts_projection_matches_crud_after_enrichment_update() {
    let mut f = fixture();
    assert!(f
        .store
        .update_enrichment_fields(
            "projection",
            Some("neutral enriched summary"),
            None,
            None,
            None,
            1
        )
        .expect("enrichment update"));
    assert_projection(&f);
}

#[test]
fn repair_compatibility_helpers_converge_both_projections_and_mapping() {
    let f = fixture();
    {
        let _guard = crate::db::acquire_fts_sync_lock();
        let conn = f.store.connection();
        conn.execute("DELETE FROM memories_fts WHERE id = 'projection'", [])
            .unwrap();
        conn.execute(
            "INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
             SELECT 999, id, path, summary, text, keywords, entities
             FROM memories WHERE id = 'projection'",
            [],
        )
        .unwrap();
    }

    crate::db::sync_memories_symbolic_fts(f.store.connection(), "projection")
        .expect("repair restore compatibility sync");
    assert_projection(&f);
    let key: i64 = f
        .store
        .connection()
        .query_row(
            "SELECT projection_key FROM memories_fts_projection_map WHERE memory_id = 'projection'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for table in ["memories_fts", "memories_symbolic_fts"] {
        let aligned: i64 = f
            .store
            .connection()
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE rowid = ?1 AND id = 'projection'"),
                [key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(aligned, 1, "{table}");
    }

    // This is the external purge order: memories, lexical FTS, then the
    // historical public symbolic-delete helper.
    f.store
        .connection()
        .execute("DELETE FROM memories WHERE id = 'projection'", [])
        .unwrap();
    f.store
        .connection()
        .execute("DELETE FROM memories_fts WHERE id = 'projection'", [])
        .unwrap();
    crate::db::delete_memories_symbolic_fts(f.store.connection(), "projection")
        .expect("repair purge compatibility delete");
    for table in [
        "memories_fts",
        "memories_symbolic_fts",
        "memories_fts_projection_map",
    ] {
        let remaining: i64 = f
            .store
            .connection()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 0, "{table}");
    }
}

#[test]
fn conflicting_projection_occupant_fails_without_deleting_adjacent_sentinel() {
    let mut f = fixture();
    f.store
        .upsert(&make_entry("neighbor", "neighbor body"))
        .expect("adjacent live memory");
    {
        let _guard = crate::db::acquire_fts_sync_lock();
        let conn = f.store.connection();
        conn.execute(
            "UPDATE memories_fts_projection_map SET projection_key = 999 WHERE memory_id = 'projection'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
             VALUES (999, 'adjacent-sentinel', '/sentinel', '', 'do not delete', '', '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
             VALUES (999, 'adjacent-sentinel', '/sentinel', '', 'do not delete', '', '', '')",
            [],
        )
        .unwrap();
    }

    let error = f
        .store
        .update_with_revision(
            "projection",
            "must roll back",
            "must roll back",
            "manual",
            &serde_json::json!({}),
            None,
            1,
        )
        .expect_err("conflicting rowid occupant must fail closed");
    assert!(
        error.to_string().contains("names a different row"),
        "{error}"
    );
    let error = crate::db::delete_memories_symbolic_fts(f.store.connection(), "projection")
        .expect_err("autocommit conflicting delete must roll back");
    assert!(
        error.to_string().contains("names a different row"),
        "{error}"
    );
    let mapped: i64 = f
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM memories_fts_projection_map WHERE memory_id = 'projection'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        mapped, 1,
        "failed delete must preserve the original mapping"
    );
    for table in ["memories_fts", "memories_symbolic_fts"] {
        let sentinel: i64 = f
            .store
            .connection()
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE rowid = 999 AND id = 'adjacent-sentinel'"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sentinel, 1, "{table}");
    }
    assert_eq!(
        f.store.get("projection").unwrap().unwrap().text,
        "neutral body"
    );
}

#[test]
fn fts_projection_decodes_empty_arrays_and_preserves_legacy_fallback() {
    let conn = super::make_conn();
    for (raw, expected) in [
        (Some("[]"), Some("")),
        (Some("[\" left \",\"\",\"right\"]"), Some(" left   right")),
        (Some("[\"mixed\",42]"), Some("mixed ,42")),
        (Some("not [json]"), Some("not  json")),
        (Some("\tlegacy\t"), Some("\tlegacy\t")),
        (None, None),
    ] {
        let actual: Option<String> = conn
            .query_row("SELECT memcore_fts_terms(?1)", [raw], |row| row.get(0))
            .expect("projection should tolerate legacy values");
        assert_eq!(actual.as_deref(), expected, "raw={raw:?}");
    }
}

#[test]
fn first_live_insert_replaces_only_its_unmapped_historical_id() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
             VALUES ('projection', '/stale', 'stale', 'stale body', '', '')",
            [],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO memories_symbolic_fts(id, path, summary, text, keywords, entities, topic)
             VALUES ('projection', '/stale', 'stale', 'stale body', '', '', '')",
            [],
        )
        .unwrap();
    store
        .upsert(&make_entry("projection", "fresh body"))
        .unwrap();
    let text: String = store
        .connection()
        .query_row(
            "SELECT text FROM memories_fts WHERE id = 'projection'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(text, "fresh body");
    let rows: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM memories_fts WHERE id = 'projection'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn incremental_backfill_preserves_orphans_and_counts_only_inserted_rows() {
    let mut f = fixture();
    {
        let conn = f.store.connection();
        conn.execute(
            "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
             VALUES ('independent-orphan', '/orphan', '', 'orphan payload', '', '')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE memories_fts_projection_map SET projection_key = projection_key + 1000 WHERE memory_id = 'projection'",
            [],
        )
        .unwrap();
    }
    let inserted = f.store.backfill_fts_missing().expect("incremental repair");
    assert_eq!(inserted, 1, "only the drifted live row is reinserted");
    assert_projection(&f);
    let orphan: i64 = f
        .store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM memories_fts WHERE id = 'independent-orphan'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphan, 1, "incremental backfill must preserve orphans");
}

#[test]
fn incremental_backfill_preserves_orphan_at_drifted_key() {
    let mut f = fixture();
    {
        let conn = f.store.connection();
        conn.execute(
            "UPDATE memories_fts_projection_map SET projection_key = 999 WHERE memory_id = 'projection'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories_fts(rowid, id, path, summary, text, keywords, entities)
             VALUES (999, 'orphan-at-key', '/orphan', '', 'keep orphan', '', '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memories_symbolic_fts(rowid, id, path, summary, text, keywords, entities, topic)
             VALUES (999, 'orphan-at-key', '/orphan', '', 'keep orphan', '', '', '')",
            [],
        )
        .unwrap();
    }
    let inserted = f.store.backfill_fts_missing().expect("incremental repair");
    assert!(
        inserted <= 1,
        "no full rebuild may be disguised as backfill"
    );
    assert_projection(&f);
    for table in ["memories_fts", "memories_symbolic_fts"] {
        let orphan: i64 = f
            .store
            .connection()
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE rowid = 999 AND id = 'orphan-at-key'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan, 1, "{table}: orphan at drifted key must remain");
    }
}

#[test]
fn projection_deletes_use_rowid_point_lookup_plans() {
    let mut f = fixture();
    for index in 0..256 {
        f.store
            .upsert(&make_entry(
                &format!("point-lookup-{index}"),
                "point lookup",
            ))
            .unwrap();
    }
    for sql in [
        crate::db::fts_projection::DELETE_LEXICAL_AT_KEY_SQL,
        crate::db::fts_projection::DELETE_SYMBOLIC_AT_KEY_SQL,
    ] {
        let details = f
            .store
            .connection()
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map([1_i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("VIRTUAL TABLE INDEX 0:=")),
            "FTS5 must receive a rowid equality constraint: {details:?}"
        );

        let steps = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed = steps.clone();
        f.store
            .conn
            .progress_handler(
                1,
                Some(move || {
                    observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    false
                }),
            )
            .unwrap();
        f.store
            .connection()
            .execute(sql, [2_000_000_i64])
            .expect("point delete on an absent key");
        f.store
            .conn
            .progress_handler(0, None::<fn() -> bool>)
            .unwrap();
        assert!(
            steps.load(std::sync::atomic::Ordering::Relaxed) < 256,
            "rowid lookup must not scan the populated projection: {steps:?}"
        );
    }
}
