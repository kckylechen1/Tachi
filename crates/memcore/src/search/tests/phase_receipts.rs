//! tachi#1097 PERF-T3 S1 — phase-attribution receipt discrimination tests.
//!
//! Every test here flips one observable branch of `hybrid_search` and
//! asserts the receipt records THAT branch as executed (or absent). Per the
//! issue's acceptance: phase totals/counts must cover the executed branches
//! — empty candidates, vector unavailable, FTS fallback, graph disabled/
//! enabled, `record_access=false/true`, MMR on/off. Each test is RED on the
//! pre-instrumentation code (the function `hybrid_search_with_receipt` and
//! the `SearchPhaseReceipt` family simply do not exist there) and GREEN
//! after.
//!
//! Workload re-use (#1097 D7): the entries/queries here reuse the same
//! shape the existing fixtures (`baseline.rs`, `expansion.rs`, `graph.rs`)
//! already proved out — no parallel CJK/graph corpus is minted here. CJK
//! recall already has `golden_corpus::Slice::Cjk`; graph expansion already
//! has `tests/graph.rs:60`; vector KNN has no prior fixture and is added
//! here (per #1097 D7-③ "新建的有:vector 通道...").

use super::*;

// ---------------------------------------------------------------------------
// Default-off / sampling switch (#1097 D4)
// ---------------------------------------------------------------------------

#[test]
fn receipt_is_not_sampled_by_default_and_returns_the_placeholder() {
    let mut conn = setup();
    insert(&mut conn, "x", "rust performance memory safety", &["rust"]);

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        ..Default::default()
    };
    let (_, receipt) = hybrid_search_with_receipt(&conn, "rust performance", &opts).unwrap();

    // Default-off posture (#1097 D4): when the caller did not opt in, the
    // receipt is a placeholder. Every phase is None, every layer tag is
    // NotSampled, total_elapsed is zero. Mechanism on this path: no
    // `Instant::now` reads, no DB work, no Vec allocations — but the
    // placeholder struct and the result tuple ARE still constructed, so this
    // is **not** a zero-overhead claim. The false path's real cost is an S2
    // measurement acceptance item (#1097).
    assert!(!receipt.sampled, "collect_phase_receipt defaults to false");
    assert_eq!(receipt.total_elapsed, std::time::Duration::ZERO);
    assert!(receipt.candidates.is_none());
    assert!(receipt.fetch.is_none());
    assert!(receipt.rank.is_none());
    assert!(receipt.graph_expansion.is_none());
    assert!(receipt.access_recording.is_none());
    assert_eq!(receipt.pool_wait, LayerAvailability::NotSampled);
    assert_eq!(receipt.sqlite_retry, LayerAvailability::NotSampled);
}

// ---------------------------------------------------------------------------
// Discrimination ① — empty-candidate early return (search.rs:160-162)
// ---------------------------------------------------------------------------

#[test]
fn receipt_marks_fetch_rank_graph_access_absent_on_empty_candidates() {
    let mut conn = setup();
    insert(&mut conn, "x", "rust performance memory safety", &["rust"]);

    // A query with zero overlap: vec channel off, FTS conjunctive zero,
    // symbolic empty, no exact-id. All three candidate channels return
    // nothing and the merged candidate set is empty.
    let opts = SearchOptions {
        top_k: 3,
        record_access: true,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (results, receipt) = hybrid_search_with_receipt(&conn, "zzz qqq xxx", &opts).unwrap();

    assert!(
        results.is_empty(),
        "query with no overlap returns no results"
    );
    assert!(receipt.sampled);
    // Candidate collection ALWAYS runs — its receipt is populated even when
    // it found nothing (the honest "ran and matched zero" form).
    let candidates = receipt
        .candidates
        .as_ref()
        .expect("candidates phase always runs when sampled");
    assert_eq!(candidates.merged_candidate_count, 0);
    // The empty-candidate early return at search.rs:160-162 skips fetch /
    // rank / graph_expansion / access_recording. The receipt marks them
    // absent (`None`) — NOT zero elapsed, per the contract's "executed phases
    // appear, unexecuted ones are marked absent rather than zero".
    assert!(
        receipt.fetch.is_none(),
        "fetch must be absent on empty-candidate early return"
    );
    assert!(
        receipt.rank.is_none(),
        "rank must be absent on empty-candidate early return"
    );
    assert!(
        receipt.graph_expansion.is_none(),
        "graph_expansion must be absent on empty-candidate early return"
    );
    assert!(
        receipt.access_recording.is_none(),
        "access_recording must be absent on empty-candidate early return"
    );
}

// ---------------------------------------------------------------------------
// Discrimination ② — vector channel unavailable (candidates.rs:30-31 else)
// ---------------------------------------------------------------------------

#[test]
fn receipt_records_vector_channel_as_none_when_unavailable() {
    let mut conn = setup();
    insert(&mut conn, "v", "voyage fallback lexical probe", &["voyage"]);

    // vec_available=false forces the `else { HashMap::new() }` branch at
    // candidates.rs:44-45. The receipt's `vec` field must be `None` (honest
    // "channel did not run") rather than `Some({0 elapsed, 0 count})`.
    let opts = SearchOptions {
        top_k: 3,
        vec_available: false,
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt) = hybrid_search_with_receipt(&conn, "voyage fallback", &opts).unwrap();

    let candidates = receipt.candidates.expect("candidates phase ran");
    assert!(
        candidates.vec.is_none(),
        "vec receipt must be None when vec_available=false (channel did not run)"
    );
    // The flag itself is still recorded so a reader can distinguish
    // "channel off" from "channel on but no query vec supplied".
    assert!(!candidates.vec_available);
}

#[test]
fn receipt_records_vector_channel_as_none_when_available_but_no_query_vec() {
    let mut conn = setup();
    insert(&mut conn, "v", "voyage fallback lexical probe", &["voyage"]);

    // vec_available=true but query_vec=None — same `else { HashMap::new() }`
    // branch via the inner `if let Some(qv)` at candidates.rs:31.
    let opts = SearchOptions {
        top_k: 3,
        vec_available: true,
        query_vec: None,
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt) = hybrid_search_with_receipt(&conn, "voyage fallback", &opts).unwrap();

    let candidates = receipt.candidates.expect("candidates phase ran");
    assert!(
        candidates.vec.is_none(),
        "vec receipt must be None when query_vec=None even if vec_available=true"
    );
    assert!(candidates.vec_available);
}

// ---------------------------------------------------------------------------
// New (#1097 D7-③): vector channel executed. #1097 r1 codex review ④-A:
// the first round ended with `let _ = vec_receipt.candidate_count;`, which
// asserted nothing — a channel that silently returned a fake zero-count
// receipt would still pass. This version seeds a real stored embedding so
// KNN actually matches, then asserts `candidate_count >= 1`: the test now
// goes red if anyone reverts the vec-receipt construction to report a
// placeholder/zero count, or wires the receipt to the wrong channel.
// ---------------------------------------------------------------------------

/// Seed an entry WITH a stored embedding vector. The shared `insert` /
/// `insert_entry` helpers pass `vec_available=false` to `upsert`, so the
/// vector never reaches `memories_vec` — KNN then legitimately returns
/// zero matches and a real `candidate_count >= 1` assertion requires this
/// helper. (#1097 r1 codex review ④-A.)
fn insert_with_vector(
    conn: &mut Connection,
    id: &str,
    text: &str,
    keywords: &[&str],
    vector: Vec<f32>,
) {
    let mut entry = memory_entry(id, text, keywords);
    entry.vector = Some(vector);
    upsert(conn, &entry, true).unwrap();
}

#[test]
fn receipt_records_vector_channel_as_some_when_query_vec_supplied() {
    let mut conn = setup();
    // Seed a row whose stored embedding is identical to the query vector
    // (cosine similarity = 1.0 → guaranteed top-K match). The vector MUST
    // be 1024-wide: that is the `embedding float[1024]` column
    // `try_load_sqlite_vec` creates (db/sqlite_vec.rs:37), and sqlite-vec
    // *validates* the query vector's width — a mismatch is a hard
    // `SqliteFailure`, not a quiet zero-row result. (The dimension is a
    // magic number on both sides today; no shared constant exists.)
    let dim = 1024;
    insert_with_vector(
        &mut conn,
        "v",
        "voyage fallback lexical probe",
        &["voyage"],
        vec![0.01; dim],
    );

    let opts = SearchOptions {
        top_k: 3,
        vec_available: true,
        query_vec: Some(vec![0.01; dim]),
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt) = hybrid_search_with_receipt(&conn, "voyage fallback", &opts).unwrap();

    let candidates = receipt.candidates.expect("candidates phase ran");
    let vec_receipt = candidates
        .vec
        .as_ref()
        .expect("vec receipt must be Some when vec_available=true AND query_vec=Some");
    // #1097 r1 codex review ④-A: a REAL assertion. The seeded embedding is
    // identical to the query, so KNN must surface at least one candidate —
    // reverting the vec-receipt to a placeholder zero count, or wiring it
    // to the wrong channel, turns this red.
    assert!(
        vec_receipt.candidate_count >= 1,
        "vec channel must report the KNN match (seeded identical embedding), got count={}",
        vec_receipt.candidate_count
    );
    // #1097 r3 codex review ④ gap-1: the vec sub-timer
    // (candidates.rs:42 `vec_start` → :61 `s.elapsed()`) must be > ZERO when
    // the channel actually ran a KNN query. `vec` is `None` in the
    // all-phases timer test (no query_vec supplied there), so this is the
    // home for the vec-timer discrimination assertion: revert the vec
    // `Instant::now`/`elapsed` to `Duration::ZERO` and this goes red.
    assert!(
        vec_receipt.elapsed > std::time::Duration::ZERO,
        "vec.elapsed must be > ZERO (vec_start timer wired — KNN actually executed)"
    );
}

// ---------------------------------------------------------------------------
// Discrimination ③ — FTS OR-fallback (expansion.rs:266 merged.is_empty())
// ---------------------------------------------------------------------------

#[test]
fn receipt_records_fts_or_fallback_group_when_conjunctive_fts_zeros() {
    let mut conn = setup();
    // Same shape as `hybrid_recovers_candidate_starved_partial_coverage_target_through_or_fallback`
    // (tests/expansion.rs:163): a target missing one query term plus a fresh
    // distractor batch. The conjunctive FTS query "quartz beacon absent"
    // zeros on every row; the OR-fallback re-runs as a relaxed OR and
    // surfaces the partial-coverage target.
    let mut target = memory_entry(
        "starved-target",
        "quartz beacon coverage repair note",
        &["quartz", "beacon"],
    );
    target.timestamp = "2026-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, target);
    for idx in 0..12 {
        let mut distractor = memory_entry(
            &format!("fresh-symbolic-{idx:02}"),
            "quartz scratch distractor",
            &["quartz"],
        );
        distractor.timestamp = format!("2026-01-02T00:00:{idx:02}Z");
        insert_entry(&mut conn, distractor);
    }

    let opts = SearchOptions {
        top_k: 1,
        candidates_per_channel: 1,
        weights: HybridWeights {
            semantic: 0.0,
            fts: 1.0,
            symbolic: 0.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        mmr_threshold: None,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt) = hybrid_search_with_receipt(&conn, "quartz beacon absent", &opts).unwrap();

    let candidates = receipt.candidates.expect("candidates phase ran");
    // At least one non-fallback group (the original conjunctive query, idx 0)
    // plus exactly one fallback group (is_fallback=true).
    let fallbacks: Vec<&FtsExpansionGroupReceipt> = candidates
        .fts_groups
        .iter()
        .filter(|g| g.is_fallback)
        .collect();
    assert_eq!(
        fallbacks.len(),
        1,
        "exactly one OR-fallback group must be recorded when conjunctive FTS zeroed, got {:?}",
        candidates.fts_groups
    );
    assert!(
        candidates.fts_groups.iter().any(|g| !g.is_fallback),
        "non-fallback (conjunctive) group must also be recorded"
    );
    // Discriminator for the fallback's own timer (expansion.rs:292). Group
    // *presence* alone stays green even if `fallback_start` is replaced by a
    // hardcoded `Duration::ZERO` — the group would still be pushed. This
    // asserts the timer actually wraps the `search_fts_raw_match` call.
    //
    // On the `> ZERO` form (see the module note on
    // `receipt_sampled_timers_actually_ran_and_subphases_fit_under_total`):
    // this is a practical assertion, not an API guarantee. `Instant` is
    // documented only as nondecreasing, so `elapsed()` returning zero is
    // permitted. The assertion holds because the fallback brackets a real
    // SQLite query — microseconds of work — while `Instant`'s resolution on
    // the platforms this suite runs on is nanoseconds. A zero here indicates a
    // timer that never started, not a run too fast to measure.
    assert!(
        fallbacks[0].elapsed > std::time::Duration::ZERO,
        "OR-fallback group must carry its own live timer, got {:?}",
        fallbacks[0].elapsed
    );
    // #1097 r4 codex review ①: the `hit_count` fields
    // (expansion.rs:271/:284 and :303/:322) had no discriminator — replacing
    // either with a literal `0` stayed green. Here the two groups are
    // distinguishable *by count*, which pins each field to its own source:
    // the fallback only runs BECAUSE every conjunctive group merged to
    // nothing, so the non-fallback groups must report exactly zero hits, and
    // the fallback must report the rows it recovered. Wiring `hit_count` to
    // the wrong group's hits, or stubbing the fallback's to `0`, turns this
    // red.
    assert!(
        fallbacks[0].hit_count >= 1,
        "the OR-fallback ran a relaxed query that recovered the \
         partial-coverage target, so its hit_count must be non-zero, got {:?}",
        candidates.fts_groups
    );
    for group in candidates.fts_groups.iter().filter(|g| !g.is_fallback) {
        assert_eq!(
            group.hit_count, 0,
            "the fallback fires only when every conjunctive group merged to \
             nothing (expansion.rs:288 `merged.is_empty()`), so non-fallback \
             group idx={} must report 0 hits, got {:?}",
            group.idx, candidates.fts_groups
        );
    }
}

#[test]
fn receipt_records_no_fts_fallback_group_when_conjunctive_fts_hits() {
    let mut conn = setup();
    insert(&mut conn, "a", "rust performance memory safety", &["rust"]);
    insert(&mut conn, "b", "rust async runtime tokio", &["rust"]);

    // An ordinary query whose conjunctive FTS hits cleanly — no fallback.
    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt) = hybrid_search_with_receipt(&conn, "rust performance", &opts).unwrap();

    let candidates = receipt.candidates.expect("candidates phase ran");
    assert!(
        candidates.fts_groups.iter().all(|g| !g.is_fallback),
        "no fallback group should be recorded when conjunctive FTS matched, got {:?}",
        candidates.fts_groups
    );
    assert!(
        !candidates.fts_groups.is_empty(),
        "the conjunctive group itself should be recorded"
    );
    // #1097 r4 codex review ①: discriminator for the non-fallback
    // `hit_count` (expansion.rs:271 `group_hits.len()` → :284). Stubbing it to
    // a literal `0` stayed green before this assertion existed. idx 0 is the
    // original conjunctive query; "rust performance" matches the seeded row
    // "rust performance memory safety", and the fact that NO fallback group
    // was recorded above independently proves the merged set was non-empty —
    // so the original group must report at least one hit.
    let original = candidates
        .fts_groups
        .iter()
        .find(|g| g.idx == 0)
        .expect("the original conjunctive query is group idx 0");
    assert!(
        original.hit_count >= 1,
        "the conjunctive group matched (no fallback was triggered), so its \
         hit_count must be non-zero, got {:?}",
        candidates.fts_groups
    );
}

// ---------------------------------------------------------------------------
// Discrimination ⑤ — record_access on/off (search.rs:189)
// ---------------------------------------------------------------------------

#[test]
fn receipt_populates_access_recording_only_when_record_access_true() {
    let mut conn = setup();
    let mut entry = memory_entry(
        "access-probe",
        "AccessProbe unique searchable memory text",
        &["access-probe"],
    );
    entry.access_count = 3;
    insert_entry(&mut conn, entry);

    // record_access=true: the entire `if opts.record_access { ... }` block
    // runs and `access_recording` is populated. `updated_row_count` is
    // `record_access_with_updates(...).len()` — the existing return value
    // (access.rs:73), so no extra DB query and no new counter on the access
    // boundary. (That is the mechanism; the cost of the `.len()` itself is
    // not claimed or measured.)
    let opts_on = SearchOptions {
        top_k: 1,
        candidates_per_channel: 0,
        record_access: true,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt_on) = hybrid_search_with_receipt(&conn, "AccessProbe", &opts_on).unwrap();
    let access_on = receipt_on
        .access_recording
        .as_ref()
        .expect("access_recording must be Some when record_access=true");
    assert_eq!(
        access_on.updated_row_count, 1,
        "the single seeded row should be the one bumped row"
    );

    // record_access=false: the block is skipped entirely and the receipt's
    // access_recording is None — NOT zero elapsed, per the contract's
    // "executed phases appear, unexecuted ones are marked absent".
    let opts_off = SearchOptions {
        top_k: 1,
        candidates_per_channel: 0,
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt_off) = hybrid_search_with_receipt(&conn, "AccessProbe", &opts_off).unwrap();
    assert!(
        receipt_off.access_recording.is_none(),
        "access_recording must be None when record_access=false"
    );
}

// ---------------------------------------------------------------------------
// Discrimination ⑥ — MMR on/off (opts.mmr_threshold Some/None)
// ---------------------------------------------------------------------------

#[test]
fn receipt_records_mmr_enabled_flag_under_both_states() {
    let mut conn = setup();
    insert(&mut conn, "a", "rust performance memory safety", &["rust"]);
    insert(&mut conn, "b", "rust async runtime tokio", &["rust"]);
    insert(&mut conn, "c", "rust ownership borrow checker", &["rust"]);

    // mmr_threshold = Some(0.85) (the default) — the MMR diversity post-filter
    // runs (`apply_mmr_diversity` at ranking.rs:143-147).
    let opts_on = SearchOptions {
        top_k: 3,
        mmr_threshold: Some(0.85),
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt_on) = hybrid_search_with_receipt(&conn, "rust performance", &opts_on).unwrap();
    let rank_on = receipt_on.rank.as_ref().expect("rank phase ran");
    assert!(
        rank_on.mmr_enabled,
        "mmr_enabled must be true when opts.mmr_threshold=Some"
    );

    // mmr_threshold = None — the ranker returns plain score-sorted order
    // (the `else` branch at ranking.rs:145-147). Per #1097 D3 MMR is NOT
    // separately timed; the on/off flag is the discrimination signal a
    // benchmark uses to compare the same query both ways.
    let opts_off = SearchOptions {
        top_k: 3,
        mmr_threshold: None,
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt_off) =
        hybrid_search_with_receipt(&conn, "rust performance", &opts_off).unwrap();
    let rank_off = receipt_off.rank.as_ref().expect("rank phase ran");
    assert!(
        !rank_off.mmr_enabled,
        "mmr_enabled must be false when opts.mmr_threshold=None"
    );
    // #1097 D3: the two DB I/Os inside `rank_candidate_entries` carry
    // individual timers under both states.
    assert!(
        rank_off.get_superseded_ids.candidate_count > 0,
        "get_superseded_ids candidate count is read off the already-fetched ids vec (no extra DB query)"
    );
    let access_off = rank_off
        .get_access_times
        .as_ref()
        .expect("get_access_times must be Some on a normal (non-filtered-to-zero) rank path");
    assert!(
        access_off.candidate_count > 0,
        "get_access_times candidate count is read off the already-filtered ids vec (no extra DB query)"
    );
}

// ---------------------------------------------------------------------------
// Sanity — a normal successful run populates every always-running phase
// and marks pool_wait / sqlite_retry per the honesty clause (#1097 D1, D8).
// ---------------------------------------------------------------------------

#[test]
fn receipt_marks_pool_wait_unavailable_and_sqlite_retry_not_applicable() {
    let mut conn = setup();
    insert(&mut conn, "a", "rust performance memory safety", &["rust"]);

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (_, receipt) = hybrid_search_with_receipt(&conn, "rust performance", &opts).unwrap();

    // #1097 D1 / D8: hybrid_search takes a bare &Connection, so the
    // ReadPoolCheckoutReceipt (memory-server-runtime::lib.rs:67) is owned by
    // a higher layer; the production read path actually uses `with_store`
    // which discards it. We mark it Unavailable rather than fake a zero.
    assert_eq!(
        receipt.pool_wait,
        LayerAvailability::Unavailable,
        "pool checkout wait must be Unavailable (not measured from this layer)"
    );
    // #1097 D1: retry_memory_locked is wired only into write paths
    // (crud/derived/enrichment/open); recall is read-only and never enters
    // the retry loop. NotApplicable rather than a literal zero.
    assert_eq!(
        receipt.sqlite_retry,
        LayerAvailability::NotApplicable,
        "SQLite BUSY/LOCKED retry is not on the recall path; report NotApplicable, not zero"
    );
}

#[test]
fn receipt_populates_all_running_phases_on_a_normal_successful_search() {
    let mut conn = setup();
    insert(&mut conn, "a", "rust performance memory safety", &["rust"]);
    insert(&mut conn, "b", "rust async runtime tokio", &["rust"]);

    let opts = SearchOptions {
        top_k: 3,
        record_access: true,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (results, receipt) = hybrid_search_with_receipt(&conn, "rust performance", &opts).unwrap();

    assert!(!results.is_empty());
    // Every always-running phase is populated...
    assert!(receipt.candidates.is_some());
    assert!(receipt.fetch.is_some());
    assert!(receipt.rank.is_some());
    // graph_expansion is always invoked; the inner `enabled` flag carries
    // the disabled-vs-empty distinction.
    let graph = receipt
        .graph_expansion
        .as_ref()
        .expect("graph_expansion function is always invoked when sampled");
    assert!(
        !graph.enabled,
        "graph_expand_hops defaults to 0 → enabled=false"
    );
    assert_eq!(graph.expanded_count, 0);
    // ...and access_recording is populated iff record_access=true.
    let access = receipt
        .access_recording
        .as_ref()
        .expect("access_recording must be Some when record_access=true");
    assert!(access.updated_row_count > 0);
}

// ---------------------------------------------------------------------------
// #1097 r1 codex review ② — rank phase must stay present when its candidate
// filter zeros out (the DB I/O `get_superseded_ids` already executed).
// ---------------------------------------------------------------------------

#[test]
fn receipt_keeps_rank_phase_when_candidate_filter_zeros_out() {
    let mut conn = setup();
    insert(&mut conn, "a", "rust performance memory safety", &["rust"]);
    insert(&mut conn, "b", "rust async runtime tokio", &["rust"]);

    // The `domain` filter is applied ONLY inside `rank_candidate_entries`
    // (ranking.rs:75-80), NOT during candidate collection — so the FTS /
    // symbolic channels still surface both rows, fetch_by_ids loads them,
    // and THEN ranking's domain filter zeros `entries_ref` out (every
    // seeded row has `domain: None`, which never matches `Some("nope")`).
    // This is exactly the "#1097 r1 ②" shape: rank ran real DB work
    // (`get_superseded_ids`) but produced zero survivors.
    let opts = SearchOptions {
        top_k: 3,
        domain: Some("nonexistent-domain".to_string()),
        record_access: false,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (results, receipt) = hybrid_search_with_receipt(&conn, "rust performance", &opts).unwrap();

    assert!(
        results.is_empty(),
        "domain filter excludes every seeded row"
    );
    assert!(receipt.sampled, "caller opted into sampling");
    // fetch DID execute (candidates were non-empty) — honest "ran":
    assert!(
        receipt.fetch.is_some(),
        "fetch must be present — candidate collection surfaced rows"
    );
    // #1097 r1 ②: the rank phase executed `get_superseded_ids` and was
    // timed BEFORE the domain filter ran. The receipt must carry that
    // work — `rank = None` here would erase an already-executed DB I/O
    // and make "rank is slow" reports lie ("rank never ran").
    let rank = receipt.rank.as_ref().expect(
        "rank must be Some even when its candidate filter zeros out — get_superseded_ids ran",
    );
    assert!(
        rank.get_superseded_ids.candidate_count > 0,
        "get_superseded_ids ran on the fetched candidate set (count is read off the already-fetched ids vec, no extra DB query)"
    );
    assert_eq!(
        rank.ranked_result_count, 0,
        "domain filter zeroed out every candidate"
    );
    // #1097 r1 ② honest "did not run": `get_access_times` runs AFTER the
    // filter (ranking.rs:96), so when the filter zeros out it never
    // executes. The receipt marks it `None` — NOT a fake zero count —
    // so a reader can tell "ran and matched zero" from "never reached".
    assert!(
        rank.get_access_times.is_none(),
        "get_access_times must be None when the candidate filter zeroed out before it could run"
    );
}

// ---------------------------------------------------------------------------
// #1097 r1 codex review ④-B — no prior test asserted that sampled elapsed
// fields are actually wired (a timer that was never started, or wired to
// the wrong phase, would leave every elapsed at ZERO / out-of-order and
// these tests would still be green). This test falsifies both: total must
// be non-zero on a real search, and the whole is at least as large as each
// measured sub-phase (guards "timer pointed at the wrong phase").
//
// #1097 r3 codex review ④ (gap-1) — Discriminator 3 below: every present
// (Some) sub-phase timer must be strictly greater than `Duration::ZERO`.
// Reverting ANY present timer point to ZERO turns the matching assertion red.
// `>= ZERO` (a tautology) is deliberately NOT used.
//
// #1097 r4 codex review ② — what the `> ZERO` form does and does not claim.
// It is NOT an API guarantee that an executed phase has `elapsed > 0`:
// `Instant` is documented only as *nondecreasing*, and `elapsed()` returning
// `Duration::ZERO` is permitted. (Earlier rounds asserted the guarantee as
// fact; that was wrong, and the assertions are kept on different grounds.)
// These are *practical* assertions, resting on two facts about the platforms
// this suite runs on (macOS/Linux, where `Instant` reads a nanosecond-
// resolution monotonic clock):
//
//   * Most timers below bracket a real SQLite query — microseconds of work,
//     three-plus orders of magnitude above the clock's resolution.
//   * The one exception is `graph_expansion` when `graph_expand_hops == 0`,
//     which brackets only an early return. That is nanoseconds, not
//     microseconds — but the span still contains the *second* `Instant::now()`
//     read, and two successive reads of a nanosecond-resolution clock do not
//     return the same value. The margin is far thinner than the others'; it is
//     called out rather than papered over.
//
// So a zero indicates a timer that never started (or one whose
// `Instant::now`/`elapsed()` was reverted to a literal `Duration::ZERO`),
// not a run too fast to measure. That inference is what makes these useful
// discriminators. It is not a promise the standard library makes, and it is
// not claimed as one.
// ---------------------------------------------------------------------------

#[test]
fn receipt_sampled_timers_actually_ran_and_subphases_fit_under_total() {
    let mut conn = setup();
    insert(&mut conn, "a", "rust performance memory safety", &["rust"]);
    insert(&mut conn, "b", "rust async runtime tokio", &["rust"]);
    insert(&mut conn, "c", "rust ownership borrow checker", &["rust"]);

    let opts = SearchOptions {
        top_k: 3,
        record_access: true,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (results, receipt) = hybrid_search_with_receipt(&conn, "rust performance", &opts).unwrap();
    assert!(!results.is_empty());

    // Discriminator 1: a sampled receipt on a real search must have a
    // non-zero total. If `total_start = sample.then(Instant::now)` were
    // ever broken (sample flag ignored, timer never started, or
    // `finish_receipt` returning the `not_sampled()` placeholder), this
    // would be exactly `Duration::ZERO`. See the module note above for why
    // `> ZERO` is a sound practical assertion here rather than an API
    // guarantee about `Instant`.
    assert!(
        receipt.total_elapsed > std::time::Duration::ZERO,
        "sampled total_elapsed must be non-zero on a real search (timer wired / started)"
    );

    // Discriminator 2: sub-phase containment. `candidates`, `fetch`, and
    // `access_recording` all run inside the `total_start..finish_receipt`
    // window, so each must be <= total. If a timer were pointed at the
    // WRONG phase (e.g. `candidates.total_elapsed` accidentally measured
    // the whole call), that ordering would invert and one of these would
    // exceed `total_elapsed`. Loose inequality (not equality) on purpose
    // — the receipt does not promise the sum, only the containment, so
    // this stays a non-flaky structural check.
    let candidates = receipt
        .candidates
        .as_ref()
        .expect("candidates phase always runs when sampled");
    assert!(
        candidates.total_elapsed <= receipt.total_elapsed,
        "candidates.total_elapsed must fit under total_elapsed (sub-phase wiring)"
    );
    assert!(
        candidates.symbolic.elapsed <= candidates.total_elapsed,
        "symbolic.elapsed must fit under candidates.total_elapsed (sub-sub-phase wiring)"
    );
    let fetch = receipt
        .fetch
        .as_ref()
        .expect("fetch runs when candidates non-empty");
    assert!(
        fetch.elapsed <= receipt.total_elapsed,
        "fetch.elapsed must fit under total_elapsed"
    );
    let access = receipt
        .access_recording
        .as_ref()
        .expect("access_recording runs when record_access=true");
    assert!(
        access.elapsed <= receipt.total_elapsed,
        "access_recording.elapsed must fit under total_elapsed"
    );

    // Discriminator 3 (#1097 r3 ④ gap-1): every present (Some) sub-phase
    // timer must be strictly > ZERO — on the practical grounds set out in the
    // module note above, not as an API guarantee. Each assertion below pins
    // ONE concrete `Instant::now()` / `elapsed()` call site in the production
    // code — reverting that single call site to a literal `Duration::ZERO`
    // makes exactly the matching assertion fail. `>= ZERO` would be a
    // tautology (the r3 codex finding) and is deliberately not used; absolute
    // upper bounds are deliberately not used (flaky). The scenario runs a
    // normal successful search with `record_access=true`, so every
    // always-running phase is present here; the `vec` sub-timer is covered
    // by `receipt_records_vector_channel_as_some_when_query_vec_supplied`
    // (vec is `None` here because no query_vec is supplied).

    // candidates.total_elapsed — candidates.rs:121 (`phase_start.map(|s| s.elapsed())`).
    assert!(
        candidates.total_elapsed > std::time::Duration::ZERO,
        "candidates.total_elapsed must be > ZERO (candidates phase_start timer wired)"
    );
    // Each executed FTS group's elapsed — expansion.rs:260 / :292
    // (per-group `group_start`/`fallback_start`). The query "rust
    // performance" matches both seeded rows conjunctively, so at least one
    // non-fallback group ran and was timed; a group whose timer was
    // reverted to ZERO makes this red.
    assert!(
        !candidates.fts_groups.is_empty(),
        "the conjunctive FTS group must be recorded for a matching query"
    );
    for (i, group) in candidates.fts_groups.iter().enumerate() {
        assert!(
            group.elapsed > std::time::Duration::ZERO,
            "fts_groups[{i}] (idx={}, fallback={}) elapsed must be > ZERO \
             (per-group Instant wired)",
            group.idx,
            group.is_fallback
        );
    }
    // symbolic.elapsed — candidates.rs:92/103 (`symbolic_start`/`symbolic_elapsed`).
    assert!(
        candidates.symbolic.elapsed > std::time::Duration::ZERO,
        "symbolic.elapsed must be > ZERO (symbolic_start timer wired)"
    );
    // fetch.elapsed — search.rs fetch_start (`sample.then(Instant::now)`).
    assert!(
        fetch.elapsed > std::time::Duration::ZERO,
        "fetch.elapsed must be > ZERO (fetch_start timer wired)"
    );
    // rank phase — ranking.rs:37 phase_start, :53 superseded_start, :113
    // access_start. The normal path leaves get_access_times `Some` (the
    // candidate filter does not zero out here), so both rank DB I/O timers
    // are present and individually asserted.
    let rank = receipt
        .rank
        .as_ref()
        .expect("rank runs when candidates non-empty");
    assert!(
        rank.total_elapsed > std::time::Duration::ZERO,
        "rank.total_elapsed must be > ZERO (rank phase_start timer wired)"
    );
    assert!(
        rank.get_superseded_ids.elapsed > std::time::Duration::ZERO,
        "rank.get_superseded_ids.elapsed must be > ZERO (superseded_start timer wired)"
    );
    let rank_access = rank
        .get_access_times
        .as_ref()
        .expect("get_access_times is Some on the normal (non-zeroed-filter) rank path");
    assert!(
        rank_access.elapsed > std::time::Duration::ZERO,
        "rank.get_access_times.elapsed must be > ZERO (rank access_start timer wired)"
    );
    // graph_expansion.elapsed — graph_expansion.rs:26 phase_start. With
    // `graph_expand_hops == 0` (enabled=false) the function is invoked and
    // takes the early-return branch, so this span brackets nanoseconds, not
    // the microseconds the DB-backed timers above do — it is the thin-margin
    // case flagged in the module note. It still holds because the span
    // contains the second `Instant::now()` read; reverting that
    // `Instant::now`/`elapsed` to ZERO reds this.
    let graph = receipt
        .graph_expansion
        .as_ref()
        .expect("graph_expansion function is always invoked when sampled");
    assert!(
        graph.elapsed > std::time::Duration::ZERO,
        "graph_expansion.elapsed must be > ZERO even when disabled (phase_start timer wired)"
    );
    // access_recording.elapsed — search.rs access_start
    // (`sample.then(Instant::now)` inside the `if opts.record_access` block).
    assert!(
        access.elapsed > std::time::Duration::ZERO,
        "access_recording.elapsed must be > ZERO (access_start timer wired)"
    );
}
