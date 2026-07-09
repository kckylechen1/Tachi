use super::*;

#[test]
fn upsert_and_fts() {
    let mut conn = make_conn();
    let e = make_entry("abc", "Rust is a systems programming language");
    upsert(&mut conn, &e, false).unwrap();

    let results = search_fts(&conn, "systems programming", 5, false, false, None, None).unwrap();
    assert!(results.contains_key("abc"), "expected 'abc' in FTS results");
}

#[test]
fn search_fts_returns_row_decode_errors() {
    let conn = make_conn();
    let blob_id = [1u8, 2, 3, 4];
    conn.execute(
        "INSERT INTO memories(id, timestamp) VALUES (?1, ?2)",
        params![&blob_id[..], Utc::now().to_rfc3339()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         VALUES (?1, '/test', 'needle', 'needle', 'needle', 'needle')",
        params![&blob_id[..]],
    )
    .unwrap();

    let err = search_fts(&conn, "needle", 5, false, false, None, None)
        .expect_err("row decode errors must propagate instead of being dropped");
    assert!(
        err.to_string().contains("Invalid column type")
            || err.to_string().contains("InvalidColumnType"),
        "unexpected error: {err}"
    );
}

#[test]
fn upsert_jaccard_dedup_returns_fts_row_decode_errors() {
    let mut conn = make_conn();
    let blob_id = [5u8, 6, 7, 8];
    conn.execute(
        "INSERT INTO memories(id, timestamp) VALUES (?1, ?2)",
        params![&blob_id[..], Utc::now().to_rfc3339()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         VALUES (?1, '/test', 'needle overlap', 'needle overlap', 'needle', 'needle')",
        params![&blob_id[..]],
    )
    .unwrap();

    let entry = make_entry("dedup-bad-row", "needle overlap");
    let err = upsert(&mut conn, &entry, false)
        .expect_err("dedup FTS row decode errors must abort the write");
    assert!(
        err.to_string().contains("Invalid column type")
            || err.to_string().contains("InvalidColumnType"),
        "unexpected error: {err}"
    );

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'dedup-bad-row'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "failed dedup query should roll back the upsert");
}

#[test]
fn jaccard_dedup_refreshes_candidate_fts() {
    let mut conn = make_conn();
    let text = "Rust memory systems need atomic full text search updates";
    let mut canonical = make_entry("canonical", text);
    canonical.keywords = vec!["oldtag".to_string()];
    upsert(&mut conn, &canonical, false).unwrap();

    let mut duplicate = make_entry("duplicate", text);
    duplicate.keywords = vec!["mergedtag".to_string()];
    upsert(&mut conn, &duplicate, false).unwrap();

    let superseded_by: Option<String> = conn
        .query_row(
            "SELECT superseded_by FROM memories WHERE id='duplicate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(superseded_by.as_deref(), Some("canonical"));

    let results = search_fts(&conn, "mergedtag", 5, false, false, None, None).unwrap();
    assert!(
        results.contains_key("canonical"),
        "merged keyword should be searchable through the canonical row"
    );
}

#[test]
fn search_fts_respects_as_of_validity_window() {
    let mut conn = make_conn();

    let mut expired = make_entry("temporal-old", "TemporalNeedle old memory");
    expired.valid_from = "2026-01-01T00:00:00Z".to_string();
    expired.valid_until = Some("2026-02-01T00:00:00Z".to_string());
    upsert(&mut conn, &expired, false).unwrap();

    let mut active = make_entry("temporal-new", "TemporalNeedle new memory");
    active.valid_from = "2026-02-01T00:00:00Z".to_string();
    upsert(&mut conn, &active, false).unwrap();

    let january = search_fts(
        &conn,
        "TemporalNeedle",
        5,
        false,
        false,
        None,
        Some("2026-01-15T00:00:00.000Z"),
    )
    .unwrap();
    assert!(january.contains_key("temporal-old"));
    assert!(!january.contains_key("temporal-new"));

    let march = search_fts(
        &conn,
        "TemporalNeedle",
        5,
        false,
        false,
        None,
        Some("2026-03-01T00:00:00.000Z"),
    )
    .unwrap();
    assert!(!march.contains_key("temporal-old"));
    assert!(march.contains_key("temporal-new"));
}

#[test]
fn search_vec_respects_as_of_validity_window() {
    let mut conn = make_conn();
    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if has_vec == 0 {
        return;
    }

    let mut old = make_entry("temporal-vec-old", "Temporal vector old memory");
    old.valid_from = "2026-01-01T00:00:00Z".to_string();
    old.valid_until = Some("2026-02-01T00:00:00Z".to_string());
    old.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &old, true).unwrap();

    let mut new = make_entry("temporal-vec-new", "Temporal vector new memory");
    new.valid_from = "2026-02-01T00:00:00Z".to_string();
    new.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &new, true).unwrap();

    let query = vec![0.1_f32; 1024];
    let january = search_vec(
        &conn,
        &query,
        5,
        false,
        false,
        None,
        Some("2026-01-15T00:00:00.000Z"),
    )
    .unwrap();
    assert!(january.contains_key("temporal-vec-old"));
    assert!(!january.contains_key("temporal-vec-new"));

    let march = search_vec(
        &conn,
        &query,
        5,
        false,
        false,
        None,
        Some("2026-03-01T00:00:00.000Z"),
    )
    .unwrap();
    assert!(!march.contains_key("temporal-vec-old"));
    assert!(march.contains_key("temporal-vec-new"));
}

#[test]
fn search_vec_knn_with_k_constraint() {
    let mut conn = make_conn();
    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if has_vec == 0 {
        return;
    }

    let mut e = make_entry("vec-1", "vector memory entry");
    e.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &e, true).unwrap();

    let query = vec![0.1_f32; 1024];
    let results = search_vec(&conn, &query, 3, false, false, None, None).unwrap();
    assert!(results.contains_key("vec-1"));
}

#[test]
fn search_fts_respects_path_prefix() {
    let mut conn = make_conn();
    let mut project_entry = make_entry("proj-1", "systems programming with Rust");
    project_entry.path = "/project/rust".into();
    upsert(&mut conn, &project_entry, false).unwrap();

    let mut docs_entry = make_entry("docs-1", "systems programming with Rust");
    docs_entry.path = "/docs/rust".into();
    upsert(&mut conn, &docs_entry, false).unwrap();

    let results = search_fts(
        &conn,
        "systems programming",
        5,
        false,
        false,
        Some("/project"),
        None,
    )
    .unwrap();
    assert!(results.contains_key("proj-1"));
    assert!(!results.contains_key("docs-1"));
}

#[test]
fn search_symbolic_candidates_treats_like_wildcards_as_literals() {
    let mut conn = make_conn();
    let plain = make_entry("plain-wildcard", "ordinary symbolic candidate text");
    upsert(&mut conn, &plain, false).unwrap();

    let literal = make_entry("literal-wildcard", "literal ___ marker text");
    upsert(&mut conn, &literal, false).unwrap();

    let results = search_symbolic_candidates(&conn, "___", 10, false, false, None, None).unwrap();
    let ids = results
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();

    assert_eq!(
        ids,
        vec!["literal-wildcard"],
        "underscore wildcards must not broaden symbolic LIKE matches"
    );
}

#[test]
fn raw_search_channels_exclude_superseded_by_default() {
    let mut conn = make_conn();
    let old = make_entry("old", "TrendLock old rule");
    let new = make_entry("new", "TrendLock new rule");
    upsert(&mut conn, &old, false).unwrap();
    upsert(&mut conn, &new, false).unwrap();
    supersede_memory(&conn, "old", "new").unwrap();

    let results = search_fts(&conn, "TrendLock", 5, false, false, None, None).unwrap();
    assert!(results.contains_key("new"));
    assert!(!results.contains_key("old"));

    let with_superseded = search_fts(&conn, "TrendLock", 5, false, true, None, None).unwrap();
    assert!(with_superseded.contains_key("old"));
}
