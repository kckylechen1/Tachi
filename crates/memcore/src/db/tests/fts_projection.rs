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
