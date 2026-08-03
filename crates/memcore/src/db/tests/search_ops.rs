use super::*;

#[test]
fn upsert_and_fts() {
    let mut conn = make_conn();
    let e = make_entry("abc", "Rust is a systems programming language");
    upsert(&mut conn, &e, false).unwrap();

    let results = search_fts(
        &conn,
        "systems programming",
        5,
        false,
        false,
        None,
        None,
        None,
        false,
    )
    .unwrap();
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

    let err = search_fts(&conn, "needle", 5, false, false, None, None, None, false)
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

    let results = search_fts(&conn, "mergedtag", 5, false, false, None, None, None, false).unwrap();
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
        None,
        false,
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
        None,
        false,
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
        None,
        false,
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
        None,
        false,
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
    let results = search_vec(&conn, &query, 3, false, false, None, None, None, false).unwrap();
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
        None,
        false,
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

    let results =
        search_symbolic_candidates(&conn, "___", 10, false, false, None, None, None, false)
            .unwrap();
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

    let results = search_fts(&conn, "TrendLock", 5, false, false, None, None, None, false).unwrap();
    assert!(results.contains_key("new"));
    assert!(!results.contains_key("old"));

    let with_superseded =
        search_fts(&conn, "TrendLock", 5, false, true, None, None, None, false).unwrap();
    assert!(with_superseded.contains_key("old"));
}

/// Seed a row that the Wiki clause classifies as internal via its
/// `metadata.wiki_log` flag. It cannot be written through `upsert` (the
/// operation-log identity is reserved for the trusted Wiki log seam), so the
/// flag is set afterwards on the fixture connection — the same shape
/// `search::tests::noise::hybrid_hides_operation_logs` uses.
fn seed_wiki_log_row(conn: &mut Connection, id: &str, path: &str, text: &str) {
    let mut entry = make_entry(id, text);
    entry.path = path.to_string();
    upsert(conn, &entry, false).unwrap();
    conn.execute(
        "UPDATE memories SET metadata = json_set(COALESCE(NULLIF(metadata, ''), '{}'), '$.wiki_log', 1) WHERE id = ?1",
        params![id],
    )
    .unwrap();
}

/// tachi#1569 (a): the Wiki internal-row exclusion now fires on *store
/// identity*, in SQL, with no `path_prefix` involved.
///
/// This asserts at the retrieval-leg level on purpose. The Rust classifier
/// (`is_namespace_search_noise`) drops these rows from `hybrid_search`'s
/// results either way, so a whole-search assertion could not tell a working
/// SQL gate from a broken one — it would pass with the clause deleted. These
/// legs return raw SQL candidates, so their output *is* the gate.
#[test]
fn retrieval_legs_gate_internal_rows_on_store_identity_not_path_prefix() {
    let mut conn = make_conn();
    let ordinary = make_entry("legs-ordinary", "StoreGateNeedle ordinary content");
    upsert(&mut conn, &ordinary, false).unwrap();
    let mut cache = make_entry("legs-cache", "StoreGateNeedle rendered recall rows");
    cache.path = "/recall-cache/legs".to_string();
    upsert(&mut conn, &cache, false).unwrap();
    seed_wiki_log_row(
        &mut conn,
        "legs-log",
        "/notes/legs-log",
        "StoreGateNeedle wiki operation log",
    );

    // Not the wiki store, no path prefix: every row is a candidate, exactly
    // as before this change.
    let ungated = search_fts(
        &conn,
        "StoreGateNeedle",
        10,
        false,
        false,
        None,
        None,
        None,
        false,
    )
    .unwrap();
    assert!(ungated.contains_key("legs-ordinary"));
    assert!(
        ungated.contains_key("legs-cache") && ungated.contains_key("legs-log"),
        "default-off must reproduce the pre-#1569 candidate set: {ungated:?}"
    );

    // Same query, same rows, wiki store: SQL drops the internal rows.
    let gated = search_fts(
        &conn,
        "StoreGateNeedle",
        10,
        false,
        false,
        None,
        None,
        None,
        true,
    )
    .unwrap();
    assert!(gated.contains_key("legs-ordinary"));
    assert!(
        !gated.contains_key("legs-cache") && !gated.contains_key("legs-log"),
        "an unscoped read of the wiki store must not even retrieve internal rows: {gated:?}"
    );

    // The symbolic leg answers the same way.
    let symbolic_ids = search_symbolic_candidates(
        &conn,
        "StoreGateNeedle",
        10,
        false,
        false,
        None,
        None,
        None,
        true,
    )
    .unwrap()
    .into_iter()
    .map(|entry| entry.id)
    .collect::<Vec<_>>();
    assert!(symbolic_ids.contains(&"legs-ordinary".to_string()));
    assert!(!symbolic_ids.contains(&"legs-cache".to_string()));
    assert!(!symbolic_ids.contains(&"legs-log".to_string()));
}

/// tachi#1569 (b), frozen decision: the store-keyed clause honours the
/// recall-cache opt-in that `is_namespace_search_noise` has always had, and
/// honours it *only* for the recall-cache class.
#[test]
fn store_keyed_gate_honours_the_recall_cache_opt_in() {
    let mut conn = make_conn();
    let mut cache = make_entry("optin-cache", "OptInNeedle rendered recall rows");
    cache.path = "/recall-cache/optin".to_string();
    upsert(&mut conn, &cache, false).unwrap();
    // Deliberately *also* under /recall-cache: with the opt-in active the
    // cache terms are dropped, so only the wiki-log term can keep this row
    // out. That is the case that distinguishes "drop the cache terms" from
    // the sloppier "OR in every cache row".
    seed_wiki_log_row(
        &mut conn,
        "optin-log",
        "/recall-cache/optin-log",
        "OptInNeedle wiki operation log",
    );

    let opted_in = search_fts(
        &conn,
        "OptInNeedle",
        10,
        false,
        false,
        Some("/recall-cache"),
        None,
        None,
        true,
    )
    .unwrap();
    assert!(
        opted_in.contains_key("optin-cache"),
        "naming /recall-cache is an explicit request for those rows, not a leak: {opted_in:?}"
    );
    assert!(
        !opted_in.contains_key("optin-log"),
        "the cache opt-in must not release the other internal classes: {opted_in:?}"
    );

    // Same store, a prefix that reaches the same row but opts into nothing.
    let not_opted_in = search_fts(
        &conn,
        "OptInNeedle",
        10,
        false,
        false,
        Some("/"),
        None,
        None,
        true,
    )
    .unwrap();
    assert!(
        !not_opted_in.contains_key("optin-cache"),
        "without the opt-in the cache row stays out: {not_opted_in:?}"
    );
}

/// tachi#1569 (cross-vendor review, CONCERN 6): the vector leg's own
/// discrimination. The FTS and symbolic legs are covered above, and the splice
/// unit tests only prove the clause is *built* — neither shows that
/// `search_vec`'s query text actually carries it. Removing `wiki_gate` from
/// `run_search_vec_query` turns the second half of this test red.
#[test]
fn the_vector_leg_is_gated_on_store_identity_too() {
    let mut conn = make_conn();
    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if has_vec == 0 {
        // Same skip the other `search_vec` tests take when sqlite-vec is not
        // loadable in this environment.
        return;
    }

    let mut ordinary = make_entry("vec-gate-ordinary", "vector ordinary content");
    ordinary.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &ordinary, true).unwrap();

    let mut cache = make_entry("vec-gate-cache", "vector rendered recall rows");
    cache.path = "/recall-cache/vec-gate".to_string();
    cache.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &cache, true).unwrap();

    let mut log = make_entry("vec-gate-log", "vector wiki operation log");
    log.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &log, true).unwrap();
    conn.execute(
        "UPDATE memories SET metadata = json_set(COALESCE(NULLIF(metadata, ''), '{}'), '$.wiki_log', 1) WHERE id = 'vec-gate-log'",
        [],
    )
    .unwrap();

    let query = vec![0.1_f32; 1024];
    let ungated = search_vec(&conn, &query, 10, false, false, None, None, None, false).unwrap();
    assert!(
        ungated.contains_key("vec-gate-ordinary")
            && ungated.contains_key("vec-gate-cache")
            && ungated.contains_key("vec-gate-log"),
        "default-off must reproduce the pre-#1569 KNN candidate set: {ungated:?}"
    );

    let gated = search_vec(&conn, &query, 10, false, false, None, None, None, true).unwrap();
    assert!(gated.contains_key("vec-gate-ordinary"));
    assert!(
        !gated.contains_key("vec-gate-cache") && !gated.contains_key("vec-gate-log"),
        "the vector leg must drop the wiki store's internal rows in SQL: {gated:?}"
    );

    // The opt-in reaches this leg as well.
    let opted_in = search_vec(
        &conn,
        &query,
        10,
        false,
        false,
        Some("/recall-cache"),
        None,
        None,
        true,
    )
    .unwrap();
    assert!(
        opted_in.contains_key("vec-gate-cache"),
        "an explicit /recall-cache prefix opts back in on the vector leg too: {opted_in:?}"
    );
}
