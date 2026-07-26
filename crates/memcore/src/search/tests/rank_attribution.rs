//! Boost-attribution harness (tachi#1344 Phase 0 — algorithm calibration).
//!
//! # What this is
//! `ranking.rs`'s scoring stack carries a stack of hand-tuned multipliers
//! (`RESEARCH_PATH_BOOST`, `DECISION_BOOST`, the lexical-overlap
//! `MAX_BOOST`, entity-recency, tier, quality, precision) feeding two test
//! gates (`golden_corpus_meets_spec_targets`, `ops_audit_corpus_ratchet_floors`)
//! that have collided at least once (tachi#1344). Before touching any of
//! those constants, this harness makes "which boost put this record at this
//! rank" observable instead of guessed: for every labeled case in
//! `golden_corpus` and `ops_audit_corpus`, it prints one JSONL line naming
//! the expected entry's actual rank, its top competitor, a step-by-step
//! multiplier breakdown for both, and which boost(s) are rank-determining
//! (removing them from the losing side, or from the winning side, flips the
//! outcome).
//!
//! # How it observes without changing scoring
//! This file is pure driver/printer. The actual observation machinery lives
//! in `ranking.rs`'s `#[cfg(test)] pub(super) mod attribution` (a twin of
//! `rank_candidate_entries` that snapshots `scores` before/after each of the
//! same 7 `apply_*_boost` calls the production function makes, in the same
//! order) and `search.rs`'s `hybrid_search_with_attribution` (pairs that
//! breakdown with the real ranked order from the unmodified `hybrid_search`).
//! Neither is reachable outside a test build — see those modules' own doc
//! comments for the zero-production-overhead argument.
//!
//! # Corpus reuse
//! Reuses `golden_corpus::{seed_corpus, QUERIES, Slice}` and
//! `ops_audit_corpus::{seed_corpus, CASES}` verbatim (bumped to `pub(super)`
//! for this file) — no parallel corpus is invented here.
//!
//! # How to run (Oz)
//!     cargo test -p memcore --lib rank_attribution_report -- --ignored --nocapture
//! `#[ignore]`, like `golden_corpus_report` / `ops_audit_corpus_report`: a
//! diagnostic dump, not a CI gate. stdout carries one JSONL object per case;
//! nothing else is printed to stdout (no `--nocapture` noise from other
//! tests, since this is invoked as a single named test).

use super::golden_corpus::{self, Slice};
use super::ops_audit_corpus;
use super::*;

/// One boost step's contribution to a single candidate id, JSONL-ready.
fn step_json(step: &ranking::attribution::BoostStep, id: &str) -> serde_json::Value {
    json!({
        "boost": step.label,
        "hit": step.hit(id),
        "multiplier": step.multiplier_for(id),
    })
}

/// One candidate's full breakdown: pre-boost baseline, each step, final.
fn candidate_json(
    attribution: &ranking::attribution::RankAttribution,
    id: &str,
) -> serde_json::Value {
    let base = attribution.base_scores.get(id);
    json!({
        "id": id,
        "base_final_score": base.map(|b| b.final_score),
        "base_signal": base.map(|b| json!({
            "vector": b.vector,
            "fts": b.fts,
            "symbolic": b.symbolic,
            "decay": b.decay,
        })),
        "steps": attribution
            .steps
            .iter()
            .map(|s| step_json(s, id))
            .collect::<Vec<_>>(),
        "final_score": attribution.final_scores.get(id).copied(),
    })
}

/// Would removing `step`'s effect on `id` change whether `expected` currently
/// outranks `competitor` (by `final_score`, the same key production sorts
/// on)? `None` when `id` was not a candidate or the step never fired for it
/// (multiplier undefined/1.0 — nothing to remove, so it cannot be decisive).
fn flips_if_removed(
    step: &ranking::attribution::BoostStep,
    id_to_strip: &str,
    expected_final: f64,
    competitor_final: f64,
    expected_id: &str,
) -> Option<bool> {
    let mult = step.multiplier_for(id_to_strip)?;
    if (mult - 1.0).abs() <= 1e-9 {
        return None; // step did not fire on this id — nothing to remove.
    }
    let currently_expected_wins = expected_final >= competitor_final;
    let (hyp_expected, hyp_competitor) = if id_to_strip == expected_id {
        (expected_final / mult, competitor_final)
    } else {
        (expected_final, competitor_final / mult)
    };
    let hyp_expected_wins = hyp_expected >= hyp_competitor;
    Some(currently_expected_wins != hyp_expected_wins)
}

/// Run one labeled case: print its JSONL attribution record.
fn print_case(
    conn: &Connection,
    corpus: &str,
    name: &str,
    query: &str,
    expected: &str,
    opts: SearchOptions,
) {
    let (ranked, attribution) =
        hybrid_search_with_attribution(conn, query, &opts).expect("hybrid_search_with_attribution");

    let expected_rank = ranked
        .iter()
        .position(|r| r.entry.id == expected)
        .map(|i| i + 1);

    // Competitor = the entry actually contesting `expected`'s spot: rank-1 if
    // `expected` is not already there, else rank-2 (the runner-up `expected`
    // is holding off).
    let competitor_id = match ranked.first() {
        Some(top) if top.entry.id == expected => ranked.get(1).map(|r| r.entry.id.clone()),
        Some(top) => Some(top.entry.id.clone()),
        None => None,
    };

    let mut decisive_boosts: Vec<serde_json::Value> = Vec::new();
    if let Some(competitor_id) = &competitor_id {
        let expected_final = attribution
            .final_scores
            .get(expected)
            .copied()
            .unwrap_or(0.0);
        let competitor_final = attribution
            .final_scores
            .get(competitor_id.as_str())
            .copied()
            .unwrap_or(0.0);
        for step in &attribution.steps {
            for (side, id_to_strip) in [
                ("expected", expected),
                ("competitor", competitor_id.as_str()),
            ] {
                if let Some(true) = flips_if_removed(
                    step,
                    id_to_strip,
                    expected_final,
                    competitor_final,
                    expected,
                ) {
                    decisive_boosts.push(json!({
                        "boost": step.label,
                        "removed_from": side,
                    }));
                }
            }
        }
    }

    let mut candidates_json = vec![candidate_json(&attribution, expected)];
    if let Some(competitor_id) = &competitor_id {
        candidates_json.push(candidate_json(&attribution, competitor_id));
    }

    println!(
        "{}",
        json!({
            "corpus": corpus,
            "case": name,
            "query": query,
            "expected_id": expected,
            "expected_rank": expected_rank,
            "competitor_id": competitor_id,
            "candidates": candidates_json,
            "decisive_boosts": decisive_boosts,
        })
    );
}

/// REPORT — not a gate. Prints one JSONL object per golden_corpus /
/// ops_audit_corpus labeled case. `#[ignore]`; see module doc for the run
/// command.
#[test]
#[ignore = "diagnostic report only — run with --ignored --nocapture"]
fn rank_attribution_report() {
    // ---- golden_corpus ----
    let mut gconn = setup();
    golden_corpus::seed_corpus(&mut gconn);
    for spec in golden_corpus::QUERIES {
        let opts = SearchOptions {
            top_k: 40,
            candidates_per_channel: 128,
            record_access: false,
            mmr_threshold: None,
            path_prefix: (spec.slice == Slice::WikiScoped).then(|| "/wiki".to_string()),
            ..Default::default()
        };
        print_case(
            &gconn,
            "golden_corpus",
            spec.query,
            spec.query,
            spec.expected,
            opts,
        );
    }

    // ---- ops_audit_corpus ----
    let mut oconn = setup();
    ops_audit_corpus::seed_corpus(&mut oconn);
    for case in ops_audit_corpus::CASES {
        let opts = SearchOptions {
            top_k: 40,
            candidates_per_channel: 128,
            record_access: false,
            mmr_threshold: None,
            ..Default::default()
        };
        print_case(
            &oconn,
            "ops_audit_corpus",
            case.name,
            case.query,
            case.expected,
            opts,
        );
    }
}
