//! Corpus-level rank-effect evidence for tachi#1446's valid remainder.
//!
//! This driver reuses the exact `golden_corpus` and `ops_audit_corpus` seeds,
//! query lists, surfaces, and search options. It composes two existing test-only
//! observation paths instead of rebuilding scoring:
//!
//! * tachi#1447's sampled impression replay supplies the decay=0 pre-boost
//!   counterfactual;
//! * tachi#1344's rank attribution snapshots supply each production boost
//!   step's before/after ordering.
//!
//! Counts are exact fixture observations, not pass thresholds. Every named
//! lever is emitted even when its observed effect is zero. A separate gate runs
//! each exact corpus query twice with `record_access=true` and the unmodified
//! default recall config; exposure alone must leave the returned order exact.

use super::golden_corpus;
use super::ops_audit_corpus;
use super::*;
use std::collections::BTreeMap;

const BOOST_LEVERS: [&str; 7] = [
    "precision",
    "quality",
    "access_feedback",
    "tier",
    "entity_recency",
    "decision",
    "lexical_overlap",
];
const ALL_LEVERS: [&str; 8] = [
    "decay",
    "precision",
    "quality",
    "access_feedback",
    "tier",
    "entity_recency",
    "decision",
    "lexical_overlap",
];

/// The attribution twin reruns scoring after the production search, and both
/// calls read the live clock for decay. At the corpus's strongest decay slope
/// (30-day half-life, 0.20/20 decay injection), even five minutes of skew is
/// below one part per million; a relative bound also scales through the exact-id
/// and other multiplicative boosts. This is observer-clock allowance, not a
/// rank-effect threshold.
const ATTRIBUTION_CLOCK_SKEW_RELATIVE_BOUND: f64 = 1e-6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LeverEffect {
    rank_changed_candidates: usize,
    pairwise_inversions: usize,
}

fn empty_effects() -> BTreeMap<&'static str, LeverEffect> {
    ALL_LEVERS
        .into_iter()
        .map(|lever| (lever, LeverEffect::default()))
        .collect()
}

fn impression_group_id(conn: &Connection) -> String {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM recall_impression_groups", [], |row| {
            row.get(0)
        })
        .expect("count recall impression groups");
    assert_eq!(count, 1, "one isolated corpus query must write one group");
    conn.query_row("SELECT group_id FROM recall_impression_groups", [], |row| {
        row.get(0)
    })
    .expect("sampled recall impression group")
}

fn measure_case(
    conn: &Connection,
    query: &str,
    mut opts: SearchOptions,
    totals: &mut BTreeMap<&'static str, LeverEffect>,
) {
    let (ranked, attribution) =
        hybrid_search_with_attribution(conn, query, &opts).expect("rank attribution");
    let labels = attribution
        .steps
        .iter()
        .map(|step| step.label)
        .collect::<Vec<_>>();
    assert_eq!(labels, BOOST_LEVERS, "production boost sequence drifted");
    for result in &ranked {
        let attributed = attribution
            .final_scores
            .get(&result.entry.id)
            .unwrap_or_else(|| {
                panic!("{} missing from attribution for {query:?}", result.entry.id)
            });
        let observed = result.score.final_score;
        let delta = (attributed - observed).abs();
        let scale = attributed.abs().max(observed.abs()).max(1.0);
        assert!(
            delta <= ATTRIBUTION_CLOCK_SKEW_RELATIVE_BOUND * scale,
            "attribution diverged from the production final score beyond the derived live-clock bound for query {query:?}, candidate {}: attributed={attributed}, production={observed}, delta={delta}",
            result.entry.id,
        );
    }

    for step in &attribution.steps {
        let total = totals
            .get_mut(step.label)
            .expect("every attribution step has a named lever bucket");
        total.rank_changed_candidates += step.rank_changed_candidates();
        total.pairwise_inversions += step.pairwise_rank_inversions();
    }

    opts.record_access = true;
    let mut config = opts.recall_config.take().unwrap_or_default();
    config.impression_sample_rate_bps = 10_000;
    opts.recall_config = Some(config);
    hybrid_search(conn, query, &opts).expect("sampled corpus recall");
    let report = crate::replay_recall_impression_group(conn, &impression_group_id(conn))
        .expect("exact impression replay");
    assert_eq!(
        report.bit_identical_count, report.candidate_count,
        "#1447 replay must reproduce every recorded pre-boost bit"
    );
    assert!(
        !report.post_boost_claimed,
        "decay replay is pre-boost evidence only"
    );
    let decay = totals.get_mut("decay").expect("decay lever bucket");
    decay.rank_changed_candidates += report
        .candidates
        .iter()
        .filter(|candidate| candidate.recorded_pre_boost_rank != candidate.decay_zero_rank)
        .count();
    decay.pairwise_inversions += report.decay_zero_rank_inversions;
}

fn print_effects(corpus: &str, query_count: usize, effects: &BTreeMap<&'static str, LeverEffect>) {
    for lever in ALL_LEVERS {
        let effect = effects[lever];
        println!(
            "RANK_EFFECT corpus={corpus} lever={lever} queries={query_count} rank_changed_candidates={} pairwise_inversions={}",
            effect.rank_changed_candidates, effect.pairwise_inversions
        );
    }
}

fn golden_effects() -> BTreeMap<&'static str, LeverEffect> {
    let mut totals = empty_effects();
    for spec in golden_corpus::QUERIES {
        let mut conn = setup();
        golden_corpus::seed_corpus(&mut conn);
        measure_case(
            &conn,
            spec.query,
            golden_corpus::search_opts(spec),
            &mut totals,
        );
    }
    totals
}

fn ops_audit_effects() -> BTreeMap<&'static str, LeverEffect> {
    let mut totals = empty_effects();
    for case in ops_audit_corpus::CASES {
        let mut conn = setup();
        ops_audit_corpus::seed_corpus(&mut conn);
        measure_case(
            &conn,
            case.query,
            ops_audit_corpus::search_opts(case.surface),
            &mut totals,
        );
    }
    totals
}

fn changed_positions(first: &[String], second: &[String]) -> usize {
    let width = first.len().max(second.len());
    (0..width)
        .filter(|index| first.get(*index) != second.get(*index))
        .count()
}

fn repeated_exposure_case(conn: &Connection, query: &str, mut opts: SearchOptions) -> usize {
    opts.record_access = true;
    assert!(
        opts.recall_config.is_none(),
        "repeated-exposure gate must exercise the unmodified default runtime config"
    );
    let first = hybrid_search(conn, query, &opts).expect("first exposed corpus recall");
    let second = hybrid_search(conn, query, &opts).expect("second exposed corpus recall");
    let expected_display_rows = first.len() + second.len();
    let (total_rows, display_rows, use_rows): (i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), SUM(event_kind = 'display'), SUM(event_kind = 'use') FROM access_history",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("exposure provenance rows");
    assert_eq!(
        (total_rows, display_rows, use_rows),
        (
            expected_display_rows as i64,
            expected_display_rows as i64,
            0
        ),
        "the exposure gate must record every returned row as display and none as use"
    );
    let first_ids = first
        .into_iter()
        .map(|result| result.entry.id)
        .collect::<Vec<_>>();
    let second_ids = second
        .into_iter()
        .map(|result| result.entry.id)
        .collect::<Vec<_>>();
    changed_positions(&first_ids, &second_ids)
}

#[test]
fn default_runtime_repeated_exposure_is_rank_invariant_on_existing_corpora() {
    assert!(
        RecallConfig::default().use_provenance_recency,
        "the default-runtime gate requires genuine-use provenance to be the safe default"
    );

    let mut golden_changed_positions = 0;
    for spec in golden_corpus::QUERIES {
        let mut conn = setup();
        golden_corpus::seed_corpus(&mut conn);
        golden_changed_positions +=
            repeated_exposure_case(&conn, spec.query, golden_corpus::search_opts(spec));
    }

    let mut ops_changed_positions = 0;
    for case in ops_audit_corpus::CASES {
        let mut conn = setup();
        ops_audit_corpus::seed_corpus(&mut conn);
        ops_changed_positions += repeated_exposure_case(
            &conn,
            case.query,
            ops_audit_corpus::search_opts(case.surface),
        );
    }

    println!(
        "EXPOSURE_EFFECT corpus=golden_corpus queries={} changed_positions={golden_changed_positions}",
        golden_corpus::QUERIES.len()
    );
    println!(
        "EXPOSURE_EFFECT corpus=ops_audit_corpus queries={} changed_positions={ops_changed_positions}",
        ops_audit_corpus::CASES.len()
    );
    assert_eq!(
        (golden_changed_positions, ops_changed_positions),
        (0, 0),
        "repeated display exposure changed the exact default-runtime rank order"
    );
}

#[test]
fn existing_corpora_report_exact_per_lever_rank_effects() {
    let golden = golden_effects();
    let ops = ops_audit_effects();
    print_effects("golden_corpus", golden_corpus::QUERIES.len(), &golden);
    print_effects("ops_audit_corpus", ops_audit_corpus::CASES.len(), &ops);
}
