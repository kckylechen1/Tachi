//! tachi#1245 (part B): `search_vec` must not let archived rows starve the
//! live candidate budget. sqlite-vec's vec0 picks its nearest-`k` window
//! BEFORE the `archived`/`superseded`/`path`/`as_of` predicates run (they're
//! ordinary post-JOIN filters) -- on an archived-heavy table, rows that will
//! be filtered out still consume a slot in that window, so a naive
//! `k = top_k` query can return far fewer than `top_k` live rows.
//!
//! These tests seed a table where the nearest-by-distance rows are
//! dominated by archived rows and the live rows sit past the naive top_k
//! cutoff. They FAIL against the pre-fix `search_vec` (which queries vec0
//! at exactly `k = top_k` with no widening, so the archived-dominated
//! top_k window collapses to zero live rows after filtering) and PASS
//! against the widened-refetch fix.

use super::*;

const DIM: usize = 1024;

fn require_vec_table(conn: &Connection) {
    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert!(
        has_vec > 0,
        "sqlite-vec required for this test (memories_vec missing after setup())"
    );
}

/// Uniform vector at `value` in every dimension. With a zero query vector,
/// L2 distance from the query is `sqrt(DIM) * |value|`, so distance ordering
/// across rows is exactly the ordering of these `value`s -- deterministic
/// and easy to reason about without depending on sqlite-vec's exact metric.
fn uniform_vec(value: f32) -> Vec<f32> {
    vec![value; DIM]
}

fn insert_vec_row(conn: &mut Connection, id: &str, value: f32, archived: bool) {
    let mut entry = make_entry(id, &format!("vec archived overfetch probe {id}"));
    entry.vector = Some(uniform_vec(value));
    entry.archived = archived;
    upsert(conn, &entry, true).unwrap();
}

/// Naive top-k is dominated by archived rows: 6 archived rows sit closer to
/// the query than any of the 3 live rows. A `k = top_k(=3)` vec0 query (the
/// pre-fix behavior) picks exactly those 3 nearest archived rows, which are
/// then all filtered out by `archived = 0` -- yielding zero live results.
#[test]
fn archived_rows_do_not_starve_live_candidates() {
    let mut conn = make_conn();
    require_vec_table(&conn);

    let query = uniform_vec(0.0);

    // 6 archived rows: distances (from origin) 0.01 .. 0.06 -- all nearer
    // than every live row below.
    for (i, value) in [0.01, 0.02, 0.03, 0.04, 0.05, 0.06].into_iter().enumerate() {
        insert_vec_row(&mut conn, &format!("archived-{i}"), value, true);
    }

    // 3 live rows: distances 0.5, 0.6, 0.7 -- farther than every archived
    // row, so a naive k=3 vec0 fetch never reaches them.
    insert_vec_row(&mut conn, "live-near", 0.5, false);
    insert_vec_row(&mut conn, "live-mid", 0.6, false);
    insert_vec_row(&mut conn, "live-far", 0.7, false);

    let top_k = 3;
    let results = search_vec(&conn, &query, top_k, false, true, None, None).unwrap();

    assert_eq!(
        results.len(),
        top_k,
        "expected all {top_k} live rows despite 6 nearer archived rows, got {results:?}"
    );
    for id in ["live-near", "live-mid", "live-far"] {
        assert!(
            results.contains_key(id),
            "expected live row {id} in results, got {results:?}"
        );
    }
    for i in 0..6 {
        let archived_id = format!("archived-{i}");
        assert!(
            !results.contains_key(&archived_id),
            "archived row {archived_id} must not leak into include_archived=false results"
        );
    }

    // Budget contract: never more than top_k, even though internally more
    // than top_k rows were fetched from vec0 to survive the archived filter.
    assert!(results.len() <= top_k);

    // Distance ordering preserved: nearer live row -> higher similarity.
    let sim_near = results["live-near"];
    let sim_mid = results["live-mid"];
    let sim_far = results["live-far"];
    assert!(
        sim_near > sim_mid && sim_mid > sim_far,
        "expected similarity to decrease with distance: near={sim_near} mid={sim_mid} far={sim_far}"
    );
}

/// Degenerate case: fewer live rows exist than `top_k`. The widen loop must
/// terminate (bounded attempts) and return exactly the live rows that
/// exist -- no error, no infinite loop, no archived leakage.
#[test]
fn fewer_live_rows_than_top_k_returns_all_of_them_without_looping_forever() {
    let mut conn = make_conn();
    require_vec_table(&conn);

    let query = uniform_vec(0.0);

    // 5 archived rows nearer than the 2 live rows.
    for (i, value) in [0.01, 0.02, 0.03, 0.04, 0.05].into_iter().enumerate() {
        insert_vec_row(&mut conn, &format!("archived-{i}"), value, true);
    }
    insert_vec_row(&mut conn, "live-a", 0.5, false);
    insert_vec_row(&mut conn, "live-b", 0.6, false);

    let top_k = 5; // more than the 2 live rows that exist
    let results = search_vec(&conn, &query, top_k, false, true, None, None).unwrap();

    assert_eq!(
        results.len(),
        2,
        "expected exactly the 2 existing live rows (fewer than top_k), got {results:?}"
    );
    assert!(results.contains_key("live-a"));
    assert!(results.contains_key("live-b"));
    for i in 0..5 {
        assert!(!results.contains_key(&format!("archived-{i}")));
    }
}

/// No-op fast path: when nothing can be filtered out post-JOIN
/// (include_archived + include_superseded + no path/as_of filter), behavior
/// must be unchanged -- the naive top_k window IS the final result.
#[test]
fn no_filter_active_returns_naive_top_k_unchanged() {
    let mut conn = make_conn();
    require_vec_table(&conn);

    let query = uniform_vec(0.0);

    for (i, value) in [0.01, 0.02, 0.03, 0.04, 0.05, 0.06].into_iter().enumerate() {
        insert_vec_row(&mut conn, &format!("archived-{i}"), value, true);
    }
    insert_vec_row(&mut conn, "live-near", 0.5, false);

    let top_k = 3;
    let results = search_vec(&conn, &query, top_k, true, true, None, None).unwrap();

    assert_eq!(
        results.len(),
        top_k,
        "include_archived=true, include_superseded=true: naive top_k must be returned unchanged"
    );
    // The 3 nearest overall are archived-0, archived-1, archived-2.
    for i in 0..3 {
        assert!(
            results.contains_key(&format!("archived-{i}")),
            "expected nearest archived row archived-{i} when nothing is filtered, got {results:?}"
        );
    }
    assert!(
        !results.contains_key("live-near"),
        "live-near is farther than the 3 nearest archived rows; unfiltered top_k must not include it"
    );
}
