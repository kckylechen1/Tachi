use super::*;

// ── Tier lifecycle tests ───────────────────────────────────────────────────────

#[test]
fn record_access_increments_recall_count_for_fts_hits() {
    let mut conn = make_conn();
    let e = make_entry("tier-rc-1", "tier recall count test memory entry");
    upsert(&mut conn, &e, false).unwrap();

    // First call: id is in fts_hits → recall_count should be 1
    record_access(
        &conn,
        &["tier-rc-1".to_string()],
        &["tier-rc-1".to_string()],
        Some("query-a"),
    )
    .unwrap();

    let (rc, qd): (i64, i64) = conn
        .query_row(
            "SELECT recall_count, query_diversity FROM memories WHERE id = ?1",
            rusqlite::params!["tier-rc-1"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rc, 1, "recall_count should be 1 after one FTS hit");
    assert_eq!(
        qd, 1,
        "query_diversity should be 1 after one distinct query"
    );

    // Second call with different query and no FTS hit → recall_count stays 1, diversity goes to 2
    record_access(&conn, &["tier-rc-1".to_string()], &[], Some("query-b")).unwrap();

    let (rc2, qd2): (i64, i64) = conn
        .query_row(
            "SELECT recall_count, query_diversity FROM memories WHERE id = ?1",
            rusqlite::params!["tier-rc-1"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        rc2, 1,
        "recall_count should still be 1 (no FTS hit in second call)"
    );
    assert_eq!(
        qd2, 2,
        "query_diversity should be 2 after two distinct queries"
    );
}

#[test]
fn record_access_promotion_gate_raw_to_consolidated() {
    let mut conn = make_conn();
    let e = make_entry(
        "tier-promo-1",
        "promotion gate test memory entry for tier lifecycle",
    );
    upsert(&mut conn, &e, false).unwrap();

    // Need: recall_count >= 3 AND query_diversity >= 3
    // Call 3 times with distinct queries and id in fts_hits each time
    let id = "tier-promo-1".to_string();
    let legacy_config = crate::RecallConfig {
        use_provenance_recency: false,
        ..crate::RecallConfig::default()
    };
    record_access_with_updates(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("q-alpha"),
        &legacy_config,
        None,
    )
    .unwrap();
    record_access_with_updates(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("q-beta"),
        &legacy_config,
        None,
    )
    .unwrap();
    record_access_with_updates(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("q-gamma"),
        &legacy_config,
        None,
    )
    .unwrap();

    let tier: String = conn
        .query_row(
            "SELECT tier FROM memories WHERE id = ?1",
            rusqlite::params!["tier-promo-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        tier, "consolidated",
        "tier should be promoted to consolidated after 3 FTS recalls with 3 distinct queries"
    );
}

#[test]
fn record_access_no_promotion_without_diversity() {
    let mut conn = make_conn();
    let e = make_entry(
        "tier-no-promo",
        "no promotion without diversity memory entry",
    );
    upsert(&mut conn, &e, false).unwrap();

    let id = "tier-no-promo".to_string();
    // Same query hash every time → diversity stays 1
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("same-query"),
    )
    .unwrap();
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("same-query"),
    )
    .unwrap();
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("same-query"),
    )
    .unwrap();

    let (tier, rc, qd): (String, i64, i64) = conn
        .query_row(
            "SELECT tier, recall_count, query_diversity FROM memories WHERE id = ?1",
            rusqlite::params!["tier-no-promo"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        tier, "raw",
        "tier should NOT be promoted with low query diversity"
    );
    assert_eq!(rc, 3, "recall_count should be 3");
    assert_eq!(qd, 1, "query_diversity should be 1 (all same query hash)");
}

#[test]
fn tier_based_decay_pattern_decays_slower_than_raw() {
    use crate::scorer::decay_score;

    // Create two identical entries except for tier and timestamp (1 day ago)
    let yesterday = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    let raw_entry = MemoryEntry {
        id: "decay-raw".into(),
        tier: "raw".into(),
        last_access: Some(yesterday.clone()),
        access_count: 0,
        importance: 0.7,
        timestamp: yesterday.clone(),
        ..make_entry("decay-raw", "decay test entry")
    };
    let pattern_entry = MemoryEntry {
        id: "decay-pattern".into(),
        tier: "pattern".into(),
        last_access: Some(yesterday.clone()),
        access_count: 0,
        importance: 0.7,
        timestamp: yesterday.clone(),
        ..make_entry("decay-pattern", "decay test entry")
    };
    let consolidated_entry = MemoryEntry {
        id: "decay-cons".into(),
        tier: "consolidated".into(),
        last_access: Some(yesterday.clone()),
        access_count: 0,
        importance: 0.7,
        timestamp: yesterday.clone(),
        ..make_entry("decay-cons", "decay test entry")
    };

    let raw_score = decay_score(&raw_entry);
    let consolidated_score = decay_score(&consolidated_entry);
    let pattern_score = decay_score(&pattern_entry);

    // Pattern tier half-life is 30000 days → barely any decay after 1 day
    // Raw tier half-life is ~30 days → more decay after 1 day
    assert!(
        pattern_score > consolidated_score,
        "pattern ({pattern_score:.4}) should decay slower than consolidated ({consolidated_score:.4})"
    );
    assert!(
        consolidated_score > raw_score,
        "consolidated ({consolidated_score:.4}) should decay slower than raw ({raw_score:.4})"
    );
}
