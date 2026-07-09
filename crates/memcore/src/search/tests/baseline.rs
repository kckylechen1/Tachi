use super::*;

#[test]
fn hybrid_search_promotes_exact_uuid_query() {
    let mut conn = setup();
    let exact_id = "11111111-1111-4111-8111-111111111111";
    insert(
        &mut conn,
        exact_id,
        "This row has unrelated prose and should still win by exact memory id.",
        &["exact-id"],
    );
    insert(
        &mut conn,
        "distractor",
        "11111111-1111-4111-8111-111111111111 appears only in text here.",
        &["distractor"],
    );

    let opts = SearchOptions {
        top_k: 2,
        candidates_per_channel: 0,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, exact_id, &opts).unwrap();

    assert_eq!(results[0].entry.id, exact_id);
    assert_eq!(results[0].score.symbolic, 1.0);
    assert!(results[0].score.final_score >= 10.0);
}

#[test]
fn hybrid_returns_relevant() {
    let mut conn = setup();
    insert(
        &mut conn,
        "a",
        "Rust is fast and memory safe",
        &["rust", "performance"],
    );
    insert(
        &mut conn,
        "b",
        "Python is great for scripting",
        &["python", "scripting"],
    );

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "rust performance", &opts).unwrap();
    assert!(!results.is_empty());
    // "a" should score higher for "rust performance" query
    assert_eq!(results[0].entry.id, "a");
}

#[test]
fn hybrid_uses_fts_when_vectors_are_available_but_query_vec_missing() {
    let mut conn = setup();
    insert(
        &mut conn,
        "a",
        "Voyage outage should still allow lexical fallback search",
        &["voyage", "fallback"],
    );
    insert(&mut conn, "b", "Unrelated operational note", &["ops"]);

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        vec_available: true,
        query_vec: None,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "voyage fallback", &opts).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].entry.id, "a");
    assert!(results[0].score.fts > 0.0);
}

#[test]
fn empty_query_returns_empty() {
    let mut conn = setup();
    insert(&mut conn, "x", "some text", &[]);
    let opts = SearchOptions {
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "", &opts).unwrap();
    // FTS5 with empty query should produce no FTS results; vec channel also empty
    assert!(results.is_empty());
}

#[test]
fn valid_at_compares_offset_timestamps_by_instant() {
    let mut entry = memory_entry("offset", "Offset timestamp memory", &[]);
    entry.valid_from = "2026-01-01T08:00:00+08:00".to_string();
    entry.valid_until = Some("2026-01-02T08:00:00+08:00".to_string());

    assert!(valid_at(&entry, Some("2026-01-01T00:00:00Z")));
    assert!(!valid_at(&entry, Some("2026-01-02T00:00:00Z")));
}
