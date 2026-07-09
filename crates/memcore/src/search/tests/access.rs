use super::*;

#[test]
fn hybrid_search_propagates_access_history_errors() {
    let mut conn = setup();
    insert(
        &mut conn,
        "access-history-error",
        "AccessHistoryError should not silently degrade search scoring",
        &["accesshistoryerror"],
    );
    conn.execute("DROP TABLE access_history", []).unwrap();

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        ..Default::default()
    };
    let err = hybrid_search(&conn, "AccessHistoryError", &opts)
        .expect_err("access history query errors should propagate");
    let msg = err.to_string();
    assert!(
        msg.contains("access_history") || msg.contains("no such table"),
        "expected access_history error, got: {msg}"
    );
}

#[test]
fn hybrid_search_returns_post_record_access_fields() {
    let mut conn = setup();
    let mut entry = memory_entry(
        "access-return",
        "AccessReturnProbe unique searchable memory",
        &["access-return"],
    );
    entry.access_count = 7;
    insert_entry(&mut conn, entry);

    let opts = SearchOptions {
        top_k: 1,
        candidates_per_channel: 0,
        record_access: true,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "AccessReturnProbe", &opts).unwrap();

    assert_eq!(results[0].entry.id, "access-return");
    assert_eq!(results[0].entry.access_count, 8);
    assert!(results[0].entry.last_access.is_some());
}

#[test]
fn record_access_deduplicates_repeated_ids_before_incrementing() {
    let mut conn = setup();
    insert(
        &mut conn,
        "duplicate-access",
        "DuplicateAccessProbe unique searchable memory",
        &["duplicate-access"],
    );

    let ids = vec![
        "duplicate-access".to_string(),
        "duplicate-access".to_string(),
    ];
    let updates =
        record_access_with_updates(&conn, &ids, &ids, Some("DuplicateAccessProbe")).unwrap();

    assert_eq!(updates["duplicate-access"].access_count, 1);
    let (access_count, recall_count): (i64, i64) = conn
        .query_row(
            "SELECT access_count, recall_count FROM memories WHERE id = ?1",
            rusqlite::params!["duplicate-access"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(access_count, 1);
    assert_eq!(recall_count, 1);

    let history_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            rusqlite::params!["duplicate-access"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(history_count, 1);
}
