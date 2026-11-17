//! Boost-attribution harness (tachi#1344 Phase 0 — algorithm calibration).
//!
//! # What this is
//! `ranking.rs`'s scoring stack carries a stack of hand-tuned multipliers
//! (`DECISION_BOOST`, the lexical-overlap `MAX_BOOST`, entity-recency, tier,
//! quality, precision) feeding two test gates
//! (`golden_corpus_meets_spec_targets`, `ops_audit_corpus_ratchet_floors`)
//! that have collided at least once (tachi#1344). Before touching any of
//! those constants, this harness makes "which boost put this record at this
//! rank" observable instead of guessed:
//!
//! The constant that originally motivated this harness, `RESEARCH_PATH_BOOST`,
//! has since been retired on main (Phase 2 dissolved the research-path boost,
//! and its own boost step was renamed `apply_decision_and_research_boosts` ->
//! `apply_decision_boost`). The harness outlives it: the collision it exists
//! to make visible is structural, not specific to that one multiplier, and
//! `p3_probe.rs` still names gating `apply_tier_boosts` as an open follow-up
//! "needing its own probe" — which is exactly what this answers, across the
//! whole corpus rather than three hand-picked cases. for every labeled case in
//! `golden_corpus` and `ops_audit_corpus`, it prints one JSONL line naming
//! the expected entry's actual rank, its top competitor, a step-by-step
//! multiplier breakdown for both, and which boost(s) are rank-determining
//! (removing them from the losing side, or from the winning side, flips the
//! outcome).
//!
//! # How it observes without changing scoring
//! This file is pure driver/printer. The actual observation machinery lives
//! in `ranking.rs`'s `#[cfg(test)] pub(super) mod attribution` (a twin of
//! `rank_candidate_entries` that gives the one production boost sequence a
//! snapshot observer before/after each sequential marginal step) and
//! `search.rs`'s `hybrid_search_with_attribution` (pairs that breakdown with
//! the real ranked order from the unmodified `hybrid_search`).
//! The observation output remains test-only; the observer cannot alter the
//! production calls, arguments, or rank order.
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

use super::golden_corpus;
use super::ops_audit_corpus;
use super::*;
use std::collections::BTreeMap;

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
        "depends_on_decision_pair_exemption": attribution.decision_pair_exemption_ids.contains(id),
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

/// Keep the diagnostic report on the exact ops-audit retrieval contract. In
/// particular, the hindsight case is Memory-scoped; dropping that surface
/// changes both its candidate set and the attribution being reported.
fn ops_report_opts(case: &ops_audit_corpus::CaseSpec) -> SearchOptions {
    ops_audit_corpus::search_opts(case.surface)
}

#[test]
fn rank_attribution_report_preserves_ops_case_surfaces() {
    for case in ops_audit_corpus::CASES {
        assert_eq!(
            ops_report_opts(case).surface,
            case.surface,
            "rank attribution options dropped the ops-audit surface for {}",
            case.name
        );
    }

    let case = ops_audit_corpus::CASES
        .iter()
        .find(|case| case.name == "hindsight-research-wiki")
        .expect("hindsight-research-wiki case");
    let mut conn = setup();
    ops_audit_corpus::seed_corpus(&mut conn);
    let (ranked, _) = hybrid_search_with_attribution(&conn, case.query, &ops_report_opts(case))
        .expect("surface-equivalent rank attribution search");
    assert_eq!(
        ranked.first().map(|result| result.entry.id.as_str()),
        Some(case.expected),
        "the report must attribute the hindsight case within its canonical Memory surface"
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
        let opts = golden_corpus::search_opts(spec);
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
        print_case(
            &oconn,
            "ops_audit_corpus",
            case.name,
            case.query,
            case.expected,
            ops_report_opts(case),
        );
    }
}

// #2352: sensitivity is restricted to the production returned ID pool after
// all filtering. Dropping a row does not restore tail candidates or recompute
// pool boundaries, RRF ranks, lexical document frequency or typo activation.
// A promising result still needs a full pipeline replay.
struct DecisionVariant {
    name: &'static str,
    multiplier: Option<f64>,
    drop_pair_exemptions: bool,
}

const DECISION_VARIANTS: &[DecisionVariant] = &[
    DecisionVariant {
        name: "keep",
        multiplier: None,
        drop_pair_exemptions: false,
    },
    DecisionVariant {
        name: "tighten_pair_fixed_returned_pool",
        multiplier: None,
        drop_pair_exemptions: true,
    },
    DecisionVariant {
        name: "retune_1_40_fixed_returned_pool",
        multiplier: Some(1.40),
        drop_pair_exemptions: false,
    },
    DecisionVariant {
        name: "retune_1_25_fixed_returned_pool",
        multiplier: Some(1.25),
        drop_pair_exemptions: false,
    },
    DecisionVariant {
        name: "remove_boost_fixed_returned_pool",
        multiplier: Some(1.0),
        drop_pair_exemptions: false,
    },
];

struct DecisionDiagnostic<'a> {
    corpus: &'static str,
    name: &'a str,
    query: &'a str,
    expected: &'a str,
    challenger: Option<&'a str>,
    target_max_rank: usize,
}

#[derive(Default)]
struct DecisionTotals {
    cases: usize,
    reciprocal_rank_sum: f64,
    failures: Vec<String>,
}

fn decision_sensitivity_order(
    entries: &std::collections::HashMap<String, MemoryEntry>,
    attribution: &ranking::attribution::RankAttribution,
    variant: &DecisionVariant,
    returned_ids: &[String],
) -> Vec<String> {
    let decision_step = attribution
        .steps
        .iter()
        .find(|step| step.label == "decision");
    let mut scores = returned_ids
        .iter()
        .filter(|id| {
            !variant.drop_pair_exemptions || !attribution.decision_pair_exemption_ids.contains(*id)
        })
        .map(|id| {
            let mut adjusted = *attribution
                .final_scores
                .get(id)
                .expect("returned candidate missing attribution; sensitivity report is incomplete");
            if let (Some(replacement), Some(step)) = (variant.multiplier, decision_step) {
                if step.hit(id) {
                    // Strip the observed multiplier, not an assumed 1.55;
                    // candidates whose topical gate stayed closed are intact.
                    adjusted = adjusted / step.multiplier_for(id).unwrap() * replacement;
                }
            }
            let entry = entries
                .get(id)
                .expect("every returned candidate must be fetched");
            (
                id.clone(),
                adjusted,
                crate::scorer::timestamp_epoch_millis(&entry.timestamp),
            )
        })
        .collect::<Vec<_>>();
    scores.sort_by(|left, right| {
        crate::scorer::cmp_recall_rank((left.1, left.2, &left.0), (right.1, right.2, &right.0))
    });
    scores.into_iter().map(|(id, _, _)| id).collect()
}

fn print_decision_diagnostic(
    conn: &Connection,
    case: DecisionDiagnostic<'_>,
    opts: SearchOptions,
    totals: &mut BTreeMap<(&'static str, &'static str), DecisionTotals>,
) {
    assert!(opts.mmr_threshold.is_none() && !opts.record_access);
    let (ranked, attribution) = hybrid_search_with_attribution(conn, case.query, &opts)
        .expect("decision calibration search");
    let production_ids = ranked
        .iter()
        .map(|row| row.entry.id.clone())
        .collect::<Vec<_>>();
    let entries = crate::db::fetch_by_ids(conn, &production_ids, opts.include_archived)
        .expect("returned calibration candidates");
    // The keep row below is the actual returned order. Require the observer's
    // baseline to agree within that frozen pool. Attribution also contains
    // rows removed by later filtering; they must never re-enter sensitivity.
    assert_eq!(
        decision_sensitivity_order(
            &entries,
            &attribution,
            &DECISION_VARIANTS[0],
            &production_ids
        ),
        production_ids,
        "attribution order differs from production for {}; sensitivity report is incomplete",
        case.name,
    );
    let challenger = case.challenger.or_else(|| {
        production_ids
            .iter()
            .find(|id| id.as_str() != case.expected)
            .map(String::as_str)
    });
    let mut evidence_ids = vec![case.expected];
    if let Some(id) = challenger {
        if id != case.expected {
            evidence_ids.push(id);
        }
    }
    let q_tokens = ranking::distinct_expanded_query_tokens(case.query);
    let candidate_evidence = evidence_ids
        .into_iter()
        .map(|id| {
            let mut value = candidate_json(&attribution, id);
            value["in_frozen_returned_pool"] = json!(entries.contains_key(id));
            value["expanded_distinct_token_overlap"] = json!(attribution
                .base_scores
                .get(id)
                .map(|score| score.symbolic * q_tokens as f64));
            value
        })
        .collect::<Vec<_>>();
    let mut exemption_ids = attribution
        .decision_pair_exemption_ids
        .iter()
        .filter(|id| entries.contains_key(*id))
        .collect::<Vec<_>>();
    exemption_ids.sort();
    let variants = DECISION_VARIANTS.iter().map(|variant| {
        let ids = if variant.name == "keep" {
            production_ids.clone()
        } else {
            decision_sensitivity_order(&entries, &attribution, variant, &production_ids)
        };
        let rank = ids.iter().position(|id| id == case.expected).map(|index| index + 1);
        let challenger_rank = challenger.and_then(|id| ids.iter().position(|candidate| candidate == id)).map(|index| index + 1);
        let passed = rank.is_some_and(|rank| rank <= case.target_max_rank);
        let reciprocal_rank = rank.map_or(0.0, |rank| 1.0 / rank as f64);
        let total = totals.entry((case.corpus, variant.name)).or_default();
        total.cases += 1;
        total.reciprocal_rank_sum += reciprocal_rank;
        if !passed {
            total.failures.push(case.name.to_string());
        }
        json!({
            "variant": variant.name,
            "measurement": if variant.name == "keep" { "production_order" } else { "fixed_returned_pool_sensitivity" },
            "replacement_multiplier": variant.multiplier,
            "expected_rank": rank,
            "challenger_rank": challenger_rank,
            "reciprocal_rank_at_returned_k": reciprocal_rank,
            "target_status": if passed { "PASS" } else { "FAIL" },
            "returned_count": ids.len(),
            "top10": ids.into_iter().take(10).collect::<Vec<_>>(),
        })
    }).collect::<Vec<_>>();
    println!(
        "{}",
        json!({
            "report": "decision_prior_calibration",
            "kind": "case",
            "diagnostic_only": true,
            "corpus": case.corpus,
            "case": case.name,
            "query": case.query,
            "surface": format!("{:?}", opts.surface),
            "path_prefix": opts.path_prefix,
            "top_k": opts.top_k,
            "expected_id": case.expected,
            "challenger_id": challenger,
            "target_max_rank": case.target_max_rank,
            "target_is_ci_floor": case.corpus != "decision_prior_diagnostic",
            "expanded_distinct_query_tokens": q_tokens,
            "attributed_candidate_count": attribution.final_scores.len(),
            "frozen_returned_pool_count": production_ids.len(),
            "pool_boundary_and_tail_recovery_simulated": false,
            "decision_pair_exemption_ids": exemption_ids,
            "candidate_evidence": candidate_evidence,
            "variants": variants,
        })
    );
}

/// REPORT — proposed rank-1 targets may print FAIL without failing this test.
/// Existing ops/golden gates remain mandatory. Counterfactuals freeze the
/// returned ID pool, channel ranks and other boosts; no policy is selected.
#[test]
#[ignore = "diagnostic sensitivity report only — run with --ignored --nocapture"]
fn decision_prior_calibration_report() {
    let mut totals = BTreeMap::new();
    let mut golden = setup();
    golden_corpus::seed_corpus(&mut golden);
    for spec in golden_corpus::QUERIES {
        print_decision_diagnostic(
            &golden,
            DecisionDiagnostic {
                corpus: "golden_corpus",
                name: spec.query,
                query: spec.query,
                expected: spec.expected,
                challenger: None,
                target_max_rank: 10,
            },
            golden_corpus::search_opts(spec),
            &mut totals,
        );
    }
    let mut ops = setup();
    ops_audit_corpus::seed_corpus(&mut ops);
    for case in ops_audit_corpus::CASES {
        let target_max_rank = match case.class {
            ops_audit_corpus::DefectClass::GovernanceMiss => 5,
            _ => 1,
        };
        print_decision_diagnostic(
            &ops,
            DecisionDiagnostic {
                corpus: "ops_audit_corpus",
                name: case.name,
                query: case.query,
                expected: case.expected,
                challenger: None,
                target_max_rank,
            },
            ops_report_opts(case),
            &mut totals,
        );
    }
    let mut diagnostic = setup();
    ops_audit_corpus::seed_decision_prior_corpus(&mut diagnostic);
    for case in ops_audit_corpus::DECISION_PRIOR_CASES {
        print_decision_diagnostic(
            &diagnostic,
            DecisionDiagnostic {
                corpus: "decision_prior_diagnostic",
                name: case.name,
                query: case.query,
                expected: case.expected,
                challenger: Some(case.challenger),
                target_max_rank: 1,
            },
            ops_audit_corpus::search_opts(Some(crate::namespace::Surface::Memory)),
            &mut totals,
        );
    }
    for ((corpus, variant), total) in totals {
        println!(
            "{}",
            json!({
                "report": "decision_prior_calibration",
                "kind": "summary",
                "diagnostic_only": true,
                "corpus": corpus,
                "variant": variant,
                "case_count": total.cases,
                "target_failure_count": total.failures.len(),
                "target_failures": total.failures,
                "mean_reciprocal_rank_at_returned_k": total.reciprocal_rank_sum / total.cases as f64,
                "policy_recommendation": "not_selected",
            })
        );
    }
}
