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
fn receipt_is_not_sampled_by_default_and_reports_zero_overhead() {
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
    // NotSampled, total_elapsed is zero. This is the "no Instant::now reads,
    // no Vec allocations" zero-overhead production state.
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
// New (#1097 D7-③): vector channel executed. No prior fixture exercises
// real KNN; this one only proves the receipt captures the channel running,
// not that KNN matched anything (would need a populated `vector` column).
// ---------------------------------------------------------------------------

#[test]
fn receipt_records_vector_channel_as_some_when_query_vec_supplied() {
    let mut conn = setup();
    insert(&mut conn, "v", "voyage fallback lexical probe", &["voyage"]);

    // vec_available=true + a dummy query_vec. sqlite-vec may or may not be
    // loaded in the test env, but `search_vec` is still invoked and timed;
    // even an empty KNN result populates the receipt's `vec` field.
    let opts = SearchOptions {
        top_k: 3,
        vec_available: true,
        query_vec: Some(vec![0.01, 0.02, 0.03]),
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
    // We do NOT assert candidate_count > 0 here — the seeded entry has no
    // `vector` column, so KNN may legitimately return zero. The point is
    // that the channel RAN and was timed, not that it matched.
    let _ = vec_receipt.candidate_count;
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
    // runs and `access_recording` is populated. The free `updated_row_count`
    // is `record_access_with_updates(...).len()` — the existing return value
    // (access.rs:73), NOT a new counter on the access boundary.
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
        "get_superseded_ids candidate count is a free read of the existing fetched-ids vec"
    );
    assert!(
        rank_off.get_access_times.candidate_count > 0,
        "get_access_times candidate count is a free read of the existing filtered-ids vec"
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
