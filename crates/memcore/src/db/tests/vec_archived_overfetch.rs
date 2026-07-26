//! tachi#1245 (part B): `search_vec` must not let archived rows -- or the
//! unconditional anchor exclusion -- starve the live candidate budget.
//! sqlite-vec's vec0 picks its nearest-`k` window BEFORE the
//! `archived`/`superseded`/`path`/`as_of`/anchor predicates run (they're
//! ordinary post-JOIN filters) -- on a heavily-filtered table, rows that
//! will be filtered out still consume a slot in that window, so a naive
//! `k = top_k` query can return far fewer than `top_k` live rows.
//!
//! These tests seed a table where the nearest-by-distance rows are
//! dominated by rows that get filtered out post-JOIN, with live rows
//! sitting past the naive top_k cutoff. They FAIL against the pre-fix
//! `search_vec` (which queries vec0 at exactly `k = top_k` with no
//! widening) and PASS against the widened-refetch fix.
//!
//! Distance model: `memories_vec` is created as
//! `vec0(id TEXT PRIMARY KEY, embedding float[1024])` with no
//! `distance_metric=` option (`crates/memcore/src/db/sqlite_vec.rs`), and
//! sqlite-vec's own default (`sqlite-vec.c`:
//! `enum Vec0DistanceMetrics distanceMetric = VEC0_DISTANCE_METRIC_L2;`,
//! set before any `distance_metric=` token is parsed) is **L2**, not
//! cosine, despite `search_vec`'s doc comment calling it "cosine distance"
//! (pre-existing inaccuracy, out of scope here). With a uniform query
//! vector `q` and uniform doc vectors `q + delta`, L2 distance reduces to
//! `sqrt(DIM) * |delta|` (`DIM` = 1024, so `sqrt(DIM) = 32`) -- a
//! deterministic, strictly-monotonic function of `delta` with no
//! degenerate zero-query-vector collapse.

use super::*;

const DIM: usize = 1024;
/// L2 distance between two uniform-DIM vectors differing by `delta` in
/// every coordinate is `sqrt(DIM) * |delta|`.
const SQRT_DIM: f32 = 32.0; // sqrt(1024)

/// Deterministic non-zero query vector: every coordinate = 0.5. A zero
/// query vector is degenerate under this metric conversion -- `sim = 1 -
/// dist/2` clamps to 0 once `dist >= 2`, and with a zero query the L2
/// distance to any non-trivial doc vector blows past 2 immediately,
/// collapsing every similarity to the same clamped 0 (this is exactly what
/// sank the first draft of this test -- Oz measured near=mid=far=0).
const QUERY_VALUE: f32 = 0.5;

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

fn uniform_vec(value: f32) -> Vec<f32> {
    vec![value; DIM]
}

/// L2 distance from the fixed query vector to a uniform doc vector at
/// `value`, under this test's metric model.
fn expected_distance(value: f32) -> f32 {
    SQRT_DIM * (value - QUERY_VALUE).abs()
}

fn insert_vec_row(conn: &mut Connection, id: &str, value: f32, archived: bool) {
    let mut entry = make_entry(id, &format!("vec archived overfetch probe {id}"));
    entry.vector = Some(uniform_vec(value));
    entry.archived = archived;
    upsert(conn, &entry, true).unwrap();
}

/// Naive top-k is dominated by archived rows: 6 archived rows sit closer to
/// the query (deltas 0.001..0.006, distances 0.032..0.192) than any of the
/// 3 live rows (deltas 0.02/0.025/0.03, distances 0.64/0.8/0.96 -- all still
/// comfortably under the `dist < 2` clamp boundary so similarity stays
/// distinguishable). A `k = top_k(=3)` vec0 query (the pre-fix behavior)
/// picks exactly the 3 nearest archived rows, which are then all filtered
/// out by `archived = 0` -- yielding zero live results. Parent (`dd1dc14a`)
/// is RED for this exact reason; this fix widens `k` until the live rows
/// surface.
#[test]
fn archived_rows_do_not_starve_live_candidates() {
    let mut conn = make_conn();
    require_vec_table(&conn);

    let query = uniform_vec(QUERY_VALUE);

    let archived_values = [0.501, 0.502, 0.503, 0.504, 0.505, 0.506];
    for (i, value) in archived_values.into_iter().enumerate() {
        insert_vec_row(&mut conn, &format!("archived-{i}"), value, true);
        assert!(
            expected_distance(value) < 2.0,
            "archived-{i} distance must stay under the sim-clamp boundary"
        );
    }

    let live = [
        ("live-near", 0.52_f32),
        ("live-mid", 0.525),
        ("live-far", 0.53),
    ];
    for (id, value) in live {
        insert_vec_row(&mut conn, id, value, false);
        assert!(
            expected_distance(value) < 2.0,
            "{id} distance must stay under the sim-clamp boundary"
        );
    }
    // Sanity on the test's own construction: every archived row must be
    // strictly nearer than every live row, or this isn't actually testing
    // starvation.
    let max_archived_dist = archived_values
        .iter()
        .cloned()
        .map(expected_distance)
        .fold(0.0_f32, f32::max);
    let min_live_dist = live
        .iter()
        .map(|(_, v)| expected_distance(*v))
        .fold(f32::MAX, f32::min);
    assert!(
        max_archived_dist < min_live_dist,
        "test construction bug: archived rows must dominate the naive top_k window \
         (max archived dist {max_archived_dist} >= min live dist {min_live_dist})"
    );

    let top_k = 3;
    let results = search_vec(&conn, &query, top_k, false, true, None, None, None).unwrap();

    assert_eq!(
        results.len(),
        top_k,
        "expected all {top_k} live rows despite 6 nearer archived rows, got {results:?}"
    );
    for (id, _) in live {
        assert!(
            results.contains_key(id),
            "expected live row {id} in results, got {results:?}"
        );
    }
    for i in 0..archived_values.len() {
        let archived_id = format!("archived-{i}");
        assert!(
            !results.contains_key(&archived_id),
            "archived row {archived_id} must not leak into include_archived=false results"
        );
    }

    // Budget contract: never more than top_k, even though internally more
    // than top_k rows were fetched from vec0 to survive the archived filter.
    assert!(results.len() <= top_k);

    // Distance ordering preserved: nearer live row -> strictly higher
    // similarity. Non-degenerate because every seeded distance here is < 2.
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

    let query = uniform_vec(QUERY_VALUE);

    // 5 archived rows nearer than the 2 live rows (same delta scheme as
    // above, kept comfortably under the dist<2 sim-clamp boundary).
    for (i, value) in [0.501, 0.502, 0.503, 0.504, 0.505].into_iter().enumerate() {
        insert_vec_row(&mut conn, &format!("archived-{i}"), value, true);
    }
    insert_vec_row(&mut conn, "live-a", 0.52, false);
    insert_vec_row(&mut conn, "live-b", 0.525, false);

    let top_k = 5; // more than the 2 live rows that exist
    let results = search_vec(&conn, &query, top_k, false, true, None, None, None).unwrap();

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

/// Regression guard: when nothing is actually filtered post-JOIN (no
/// archived/superseded/path/as_of rows exist to exclude, and no anchors),
/// behavior is unchanged -- the naive top_k window IS the final result.
#[test]
fn no_filter_active_returns_naive_top_k_unchanged() {
    let mut conn = make_conn();
    require_vec_table(&conn);

    let query = uniform_vec(QUERY_VALUE);

    for (i, value) in [0.501, 0.502, 0.503, 0.504, 0.505, 0.506]
        .into_iter()
        .enumerate()
    {
        insert_vec_row(&mut conn, &format!("archived-{i}"), value, true);
    }
    insert_vec_row(&mut conn, "live-near", 0.52, false);

    let top_k = 3;
    let results = search_vec(&conn, &query, top_k, true, true, None, None, None).unwrap();

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

/// tachi#1245 review defect 1: the anchor exclusion (`id NOT LIKE
/// 'anchor:%'`) is UNCONDITIONAL -- not gated by `include_archived` /
/// `include_superseded`. A `may_filter` guard keyed only on those caller
/// flags would wrongly conclude "nothing can be filtered" here and skip
/// widening even though the nearest rows are all anchors. Seed 6 anchor
/// rows nearer than 3 live rows, call with `include_archived = true,
/// include_superseded = true` (every flag maximally permissive) and assert
/// the live rows still surface.
#[test]
fn anchor_rows_do_not_starve_live_candidates_even_when_every_flag_is_permissive() {
    let mut conn = make_conn();
    require_vec_table(&conn);

    let query = uniform_vec(QUERY_VALUE);

    // 6 anchor rows nearer than the 3 live rows below. `ensure_anchor`
    // creates the `memories` row (ordinary `upsert` rejects `anchor:`-
    // prefixed ids by design, tachi#773 guard c); the vec embedding is
    // inserted directly the same way production upsert code populates
    // `memories_vec` for a non-anchor row.
    let anchor_values = [0.501, 0.502, 0.503, 0.504, 0.505, 0.506];
    for (i, value) in anchor_values.into_iter().enumerate() {
        let id = ensure_anchor(&conn, AnchorKind::Issue, &format!("1245b-probe-{i}")).unwrap();
        conn.execute(
            "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
            params![id, serialize_f32(&uniform_vec(value))],
        )
        .unwrap();
        assert!(id.starts_with("anchor:"));
    }

    let live = [
        ("live-near", 0.52_f32),
        ("live-mid", 0.525),
        ("live-far", 0.53),
    ];
    for (id, value) in live {
        insert_vec_row(&mut conn, id, value, false);
    }

    let top_k = 3;
    // Every flag maximally permissive: include_archived=true,
    // include_superseded=true, no path/as_of filter. Only the unconditional
    // anchor exclusion can drop a row here.
    let results = search_vec(&conn, &query, top_k, true, true, None, None, None).unwrap();

    assert_eq!(
        results.len(),
        top_k,
        "expected all {top_k} live rows despite 6 nearer anchor rows, got {results:?}"
    );
    for (id, _) in live {
        assert!(
            results.contains_key(id),
            "expected live row {id} in results, got {results:?}"
        );
    }
}
