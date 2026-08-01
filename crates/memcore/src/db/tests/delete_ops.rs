use super::*;
use crate::db::delete_if_expected_state;
use crate::types::ExpectedMemoryState;

#[test]
fn delete_existing() {
    let mut conn = make_conn();
    let e = make_entry("del-1", "to be deleted");
    upsert(&mut conn, &e, false).unwrap();

    let deleted = delete(&mut conn, "del-1", false).unwrap();
    assert!(deleted, "should return true for existing entry");

    // Verify it's gone from main table
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'del-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);

    // Verify it's gone from FTS
    let fts_results = search_fts(&conn, "deleted", 5, false, false, None, None, None).unwrap();
    assert!(!fts_results.contains_key("del-1"));
}

#[test]
fn delete_if_expected_state_refuses_drift_and_deletes_exact_occupant() {
    let mut conn = make_conn();
    let entry = make_entry("del-guarded", "guarded target");
    upsert(&mut conn, &entry, false).unwrap();
    let stale = ExpectedMemoryState::from_entry(&entry, None);

    conn.execute(
        "UPDATE memories SET valid_until = ?1 WHERE id = ?2",
        params!["2026-12-31T23:59:59Z", entry.id],
    )
    .unwrap();
    assert!(!delete_if_expected_state(&mut conn, &entry.id, &stale, false).unwrap());

    let current = fetch_by_ids(&conn, std::slice::from_ref(&entry.id), true)
        .unwrap()
        .remove(&entry.id)
        .unwrap();
    let exact = ExpectedMemoryState::from_entry(&current, None);
    assert!(delete_if_expected_state(&mut conn, &entry.id, &exact, false).unwrap());
    assert!(fetch_by_ids(&conn, std::slice::from_ref(&entry.id), true)
        .unwrap()
        .is_empty());
}

#[test]
fn delete_returns_vector_cleanup_errors_and_rolls_back() {
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

    let e = make_entry("del-vec-error", "to be deleted after vector cleanup");
    upsert(&mut conn, &e, false).unwrap();
    conn.execute("DROP TABLE memories_vec", []).unwrap();

    let err = delete(&mut conn, "del-vec-error", true)
        .expect_err("vector cleanup errors must be returned");
    assert!(
        err.to_string().contains("memories_vec") || err.to_string().contains("no such table"),
        "unexpected error: {err}"
    );

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'del-vec-error'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "failed vector cleanup should roll back delete");
}

#[test]
fn delete_nonexistent() {
    let mut conn = make_conn();
    let deleted = delete(&mut conn, "nonexistent-id", false).unwrap();
    assert!(!deleted, "should return false for non-existent entry");
}

#[test]
fn delete_cascades_access_history_and_known_state() {
    let mut conn = make_conn();
    let e = make_entry("del-cascade", "delete target");
    upsert(&mut conn, &e, false).unwrap();

    record_access(&conn, &["del-cascade".to_string()], &[], None).unwrap();
    update_agent_known_state(
        &conn,
        "agent-delete-test",
        &[("del-cascade".to_string(), 1)],
    )
    .unwrap();

    let ah_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["del-cascade"],
            |row| row.get(0),
        )
        .unwrap();
    let aks_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_known_state WHERE memory_id = ?1",
            params!["del-cascade"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ah_before > 0);
    assert!(aks_before > 0);

    delete(&mut conn, "del-cascade", false).unwrap();

    let ah_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["del-cascade"],
            |row| row.get(0),
        )
        .unwrap();
    let aks_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_known_state WHERE memory_id = ?1",
            params!["del-cascade"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ah_after, 0, "access_history should be cleaned up on delete");
    assert_eq!(
        aks_after, 0,
        "agent_known_state should be cleaned up on delete"
    );
}
