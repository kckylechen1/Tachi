//! Phase 2 **P3 Step-0 PROBE** — measurement only, NOT the provenance band.
//!
//! # Why this exists
//! Glinda's P3 spec proposes a *topical-evidence gate* for agent-judged boosts.
//! As SHIPPED, P3 gates exactly ONE boost: `DECISION_BOOST` applies to a
//! decision-category candidate ONLY if that candidate has topical evidence in a
//! real retrieval channel. `apply_tier_boosts` is deliberately left UNGATED —
//! gating it is a scoped follow-up needing its own probe (no measured
//! tier-driven inversion in this fixture; all three case protagonists are tier
//! "raw"). Residual risk to keep in view: a zero-evidence pattern-tier entry
//! still receives its ×1.15 (non-wiki) / ×1.05 (wiki) tier lift and could
//! overtake a genuine match *within that smaller margin* — this probe does NOT
//! claim general boost safety, only that the DECISION_BOOST inversion is closed.
//!
//! This probe FALSIFIED the naive shape of the gate
//! (`fts > 0 || symbolic > 0 || vector > 0`): the decision seed's `symbolic` is
//! NOT zero — it is the dense-map noise floor of a single shared token, and its
//! `fts` is an IDF-null crumb (~1.2e-7), not a true zero. The shipped gate
//! (`ranking.rs::has_topical_evidence`) is built around exactly that floor:
//! `fts > 1e-3 || vector > 0 || symbolic*|Q_expanded| >= 1.5` (≥2 distinct
//! tokens). The spec's load-bearing numbers were **derived from PRE-P2
//! attribution data**, before the P2 surface split re-cut the
//! `hindsight-research-wiki` case to `Surface::Memory`. This probe re-measures
//! those numbers on the **real post-P2 ranking pipeline** so the band is never
//! built on a stale assumption.
//!
//! It confirms three facts inside the Memory surface for the hindsight query
//! (`"hindsight research evaluation protocol for memory recall quality"`), the
//! same corpus + `search_opts` the re-cut P2 ratchet uses
//! (`ops_audit_corpus.rs`, reused via `pub(super)` — identical fixture):
//!
//! * **(a)** the decision seed `ops-project-decision-priority` FAILS the
//!   topical-evidence gate (sub-epsilon fts crumb, `vector == 0`, and symbolic
//!   overlap below two distinct tokens) — so the gate CLOSES on it and drops its
//!   DECISION_BOOST; its former rank-2 was an artifact of DECISION_BOOST + the
//!   recency tie-break, not topical relevance;
//! * **(b)** the research note `ops-wiki-research-hindsight` HAS topical
//!   evidence (`has_topical_evidence` TRUE — fts 0.55 above epsilon, real
//!   symbolic) AND reaches RANK 1 within Memory once the seed's boost is gated
//!   off — on fts-dominance alone. Entity-recency ×1.08 does NOT fire here (the
//!   note is the sole Memory holder of the `Hindsight` entity, so the `len > 1`
//!   guard drops it) and is NOT required: the revised design abandoned the
//!   ×1.08 margin (Glinda Req-3, "no compensating boost");
//! * **(c)** control: under their OWN queries the decision seed and the
//!   governance seed DO have topical evidence, so the gate legitimately keeps
//!   their boosts — proving the gate discriminates rather than nuking all
//!   boosts.
//!
//! # Read-only
//! This probe changes NO ranking behavior. All channel values it reads are the
//! raw `HybridScore` channels already carried out of `hybrid_search` on every
//! `SearchResult`: every boost in `ranking.rs` mutates only `score.final_score`
//! (DECISION_BOOST, tier, entity-recency, access, quality, lexical), never
//! `fts`/`symbolic`/`vector`/`decay`. No `cfg(test)` accessor was needed to see
//! per-candidate channel scores.
//!
//! For fact (b) the probe calls the PRODUCTION
//! `filtering::newest_by_shared_entity` on the actual returned Memory pool to
//! CONFIRM entity-recency ×1.08 does NOT fire for the note — it does not
//! re-implement the rule. (Its `len > 1` "shared" guard is the crux: within
//! Memory the three `Hindsight`-tagged architecture wikis are `Surface::Docs`
//! and excluded at candidate collection, so the note is the sole Memory holder
//! of `Hindsight` and the boost is correctly withheld post-P2. The revised P3
//! design does not rely on ×1.08 — rank-1 comes from fts-dominance once the
//! zero-evidence seed's DECISION_BOOST is gated off. This is the pre-P2→post-P2
//! drift the probe was built to catch, now resolved by the revised mechanism.)
//!
//! # Gate semantics
//! GREEN (a)+(b)+(c) → build the band on these numbers. RED on any fact →
//! **STOP**, report the actual channel numbers, reconsider the mechanism. Do
//! not "fix" the probe to pass; a red probe is a real finding.

use super::ops_audit_corpus::{search_opts, seed_corpus};
use super::*;
use crate::namespace::Surface;
use crate::types::SearchResult;
use std::collections::{HashMap, HashSet};

const HINDSIGHT_QUERY: &str = "hindsight research evaluation protocol for memory recall quality";
const DECISION_QUERY: &str =
    "what is the current open issue priority project decision for this sprint";
const GOVERNANCE_QUERY: &str =
    "governance framing for component registry cutover when stubs and old roadmap dominate";

const DECISION_SEED: &str = "ops-project-decision-priority";
const RESEARCH_NOTE: &str = "ops-wiki-research-hindsight";
const GOV_SEED: &str = "ops-gov-framing-cutover";

fn results_for(query: &str, surface: Option<Surface>) -> Vec<SearchResult> {
    let mut conn = setup();
    seed_corpus(&mut conn);
    hybrid_search(&conn, query, &search_opts(surface)).unwrap()
}

fn find<'a>(results: &'a [SearchResult], id: &str) -> Option<&'a SearchResult> {
    results.iter().find(|r| r.entry.id == id)
}

/// Per-candidate raw-channel report. `--nocapture` shows every number so a red
/// probe hands Oz the full measurement, not just a pass/fail bit.
fn dump(label: &str, results: &[SearchResult]) {
    println!(
        "── P3 PROBE dump: {label} — {} candidates ──",
        results.len()
    );
    if results.len() >= 40 {
        println!(
            "  ⚠ pool hit the top_k=40 cap; the returned set may be a truncation \
             of the ranked candidate set — entity-recency reconstruction below \
             could be incomplete. (Corpus is ~35 seeds; not expected to trip.)"
        );
    }
    for (i, r) in results.iter().enumerate() {
        println!(
            "  #{:>2} {:<32} fts={:.4} sym={:.4} vec={:.4} decay={:.4} final={:.4} entities={:?}",
            i + 1,
            r.entry.id,
            r.score.fts,
            r.score.symbolic,
            r.score.vector,
            r.score.decay,
            r.score.final_score,
            r.entry.entities,
        );
    }
}

/// Reconstruct entity-recency eligibility with the PRODUCTION rule, fed the
/// actual returned Memory pool. With MMR off and `top_k=40 >` the Memory pool,
/// the returned set equals the `entries_ref` `apply_entity_recency_boosts` saw
/// (no supersession in this corpus), so membership here == "×1.08 fired".
fn entity_recency_ids(results: &[SearchResult]) -> HashSet<String> {
    let map: HashMap<String, &crate::types::MemoryEntry> = results
        .iter()
        .map(|r| (r.entry.id.clone(), &r.entry))
        .collect();
    crate::search::filtering::newest_by_shared_entity(&map)
}

/// FACT (a) — drift sentinel for the REVISED P3 predicate: the decision seed
/// FAILS the topical-evidence gate inside the Memory surface for the hindsight
/// query, so its DECISION_BOOST is withheld and the research note takes rank 1.
///
/// The probe originally asserted `fts == 0.0 && symbolic == 0.0`, but the Step-0
/// measurement falsified that naive shape: the seed's symbolic is NOT zero —
/// it is the dense-map noise floor of a single shared token (`sym ≈ 0.1`,
/// `overlap = symbolic * |Q_expanded| ≈ 1.0`), and its fts is an IDF-null crumb
/// (`≈ 1.2e-7`), not a true zero. The gate is built around exactly this floor:
/// it closes iff `fts <= FTS_TOPICAL_EPSILON`, `vector == 0`, AND
/// `symbolic * |Q_expanded| < MIN_SYMBOLIC_OVERLAP` (< 2 distinct tokens). So
/// the sentinel now asserts the real gate predicate (`!has_topical_evidence`),
/// decomposed into its fts-crumb and sub-two-token-overlap halves, using the
/// SAME expansion the symbolic scorer + gate use. If a corpus/expansion change
/// hands the seed a second real token (overlap ≥ 1.5) or a genuine fts hit, the
/// gate would OPEN and this sentinel fires — STOP and reconsider the margin.
#[test]
fn p3_probe_fact_a_decision_seed_fails_topical_gate_in_memory() {
    use crate::search::ranking::{
        distinct_expanded_query_tokens, has_topical_evidence, FTS_TOPICAL_EPSILON,
        MIN_SYMBOLIC_OVERLAP,
    };

    let results = results_for(HINDSIGHT_QUERY, Some(Surface::Memory));
    dump("hindsight query / Surface::Memory (fact a)", &results);

    let dec = find(&results, DECISION_SEED).unwrap_or_else(|| {
        panic!(
            "(a) decision seed `{DECISION_SEED}` absent from the Memory pool for the \
             hindsight query — P2 left it at rank 2, so it must be present to gate on. \
             ids={:?}",
            results.iter().map(|r| &r.entry.id).collect::<Vec<_>>()
        )
    });

    // Distinct expanded-query token count — the SAME deduplicated denominator
    // the gate + symbolic scorer use (single source of truth, matches BUG-F fix).
    let q_tokens = distinct_expanded_query_tokens(HINDSIGHT_QUERY);
    let overlap = dec.score.symbolic * q_tokens as f64;

    // Decomposed halves (diagnostic, exact-constant): fts is a sub-epsilon
    // crumb and the symbolic overlap is below two distinct tokens.
    assert!(
        dec.score.fts < FTS_TOPICAL_EPSILON,
        "(a) decision seed `{DECISION_SEED}` fts must be a sub-epsilon crumb \
         (< {FTS_TOPICAL_EPSILON}) for the gate to close; got fts={} (sym={}, vec={}, \
         overlap={overlap}). If a genuine fts hit appeared, the gate would OPEN — STOP.",
        dec.score.fts,
        dec.score.symbolic,
        dec.score.vector
    );
    assert!(
        overlap < MIN_SYMBOLIC_OVERLAP,
        "(a) decision seed `{DECISION_SEED}` symbolic overlap must stay below two \
         distinct tokens (symbolic*|Q_expanded| < {MIN_SYMBOLIC_OVERLAP}); got \
         overlap={overlap} (sym={}, q_tokens={q_tokens}). If a second real token \
         appeared, the gate would OPEN — STOP and reconsider the margin.",
        dec.score.symbolic
    );

    // Top-level guarantee — the actual production gate closes on the seed.
    assert!(
        !has_topical_evidence(&dec.score, q_tokens),
        "(a) the P3 topical-evidence gate must CLOSE on decision seed \
         `{DECISION_SEED}` (fts={}, sym={}, vec={}, overlap={overlap}, q_tokens={q_tokens}); \
         it did not — the seed would keep DECISION_BOOST and the research note would \
         not reach rank 1. STOP and reconsider.",
        dec.score.fts,
        dec.score.symbolic,
        dec.score.vector
    );
}

/// FACT (b) — REVISED to the shipped P3 design (Glinda Req-3: NO ×1.08
/// compensating boost). The research note (b.1) HAS real topical evidence, so
/// the gate keeps it a first-class candidate; (b.2) entity-recency ×1.08 does
/// NOT fire and is NOT required — the note is the sole Memory holder of the
/// `Hindsight` entity (the `len > 1` shared-entity guard drops it); and (b.3)
/// it reaches RANK 1 within Memory, strictly above the now-gated decision seed,
/// on fts-dominance ALONE.
///
/// The original assertion required ×1.08 to fire (a pre-revision assumption).
/// The build confirmed rank-1 holds WITHOUT it, so that requirement was stale
/// and contradicted the shipped mechanism. This test is the thin +3.98% margin
/// sentinel: it goes red if the note ever falls to/below the seed.
#[test]
fn p3_probe_fact_b_research_note_is_topical_and_rank1_without_entity_recency() {
    use crate::search::ranking::{distinct_expanded_query_tokens, has_topical_evidence};

    let results = results_for(HINDSIGHT_QUERY, Some(Surface::Memory));
    dump("hindsight query / Surface::Memory (fact b)", &results);

    let note = find(&results, RESEARCH_NOTE).unwrap_or_else(|| {
        panic!(
            "(b) research note `{RESEARCH_NOTE}` absent from the Memory pool. ids={:?}",
            results.iter().map(|r| &r.entry.id).collect::<Vec<_>>()
        )
    });

    // (b.1) the note carries real topical evidence — the gate keeps it, unlike
    // the zero-evidence decision seed of fact (a). Same deduplicated denominator
    // the gate uses (single source of truth).
    let q_tokens = distinct_expanded_query_tokens(HINDSIGHT_QUERY);
    assert!(
        has_topical_evidence(&note.score, q_tokens),
        "(b) research note `{RESEARCH_NOTE}` must have real topical evidence \
         (has_topical_evidence TRUE); got fts={} sym={} vec={} q_tokens={q_tokens}.",
        note.score.fts,
        note.score.symbolic,
        note.score.vector
    );

    // (b.2) entity-recency ×1.08 does NOT fire and is NOT needed. The note is
    // the sole Memory holder of the `Hindsight` entity, so the production
    // `newest_by_shared_entity` `len > 1` guard excludes it. Confirming this
    // documents that rank-1 comes from fts-dominance, not the abandoned margin.
    let entity_recent = entity_recency_ids(&results);
    println!(
        "── P3 PROBE: newest_by_shared_entity set within Memory = {:?}",
        entity_recent
    );
    assert!(
        !entity_recent.contains(RESEARCH_NOTE),
        "(b) entity-recency ×1.08 must NOT fire for `{RESEARCH_NOTE}` under the revised \
         P3 design (Glinda Req-3 abandoned the ×1.08 margin — rank-1 is fts-dominance \
         once the seed's boost is gated off); yet it appears in the production \
         `newest_by_shared_entity` set = {:?}. If a second Memory-surface holder of the \
         `Hindsight` entity was added, the boost now fires — the mechanism changed and \
         the margin reasoning must be re-derived. STOP and reconsider.",
        entity_recent
    );

    // (b.3) THE deliverable: the note is RANK 1 in the Memory pool, strictly
    // above the gated decision seed — the thin +3.98% margin sentinel. Goes red
    // if the note ever drops to/below the seed (DECISION_BOOST leaking back, or
    // the note losing fts-dominance).
    let dec = find(&results, DECISION_SEED);
    let top = results.first().map(|r| r.entry.id.as_str());
    assert_eq!(
        top,
        Some(RESEARCH_NOTE),
        "(b) research note `{RESEARCH_NOTE}` must be RANK 1 within Memory for the \
         hindsight query once the seed's DECISION_BOOST is gated off (rank-1 on \
         fts-dominance, no ×1.08). Got rank-1={top:?}; note.final={:.4}, \
         decision_seed.final={:?}; ids={:?}",
        note.score.final_score,
        dec.map(|d| d.score.final_score),
        results.iter().map(|r| &r.entry.id).collect::<Vec<_>>()
    );
    if let Some(dec) = dec {
        assert!(
            note.score.final_score > dec.score.final_score,
            "(b) note.final ({:.4}) must strictly exceed the gated decision seed's \
             final ({:.4}) — the +3.98% margin the ratchet's rank-1 depends on. If \
             this inverts, DECISION_BOOST leaked back onto the seed or the note lost \
             fts-dominance — STOP.",
            note.score.final_score,
            dec.score.final_score
        );
    }
}

/// FACT (c) — control: the gate must NOT close everywhere. Under their OWN
/// queries the decision seed and the governance seed each satisfy the ACTUAL
/// production gate predicate `has_topical_evidence` (codex Concern B: the old
/// `fts > 0 || symbolic > 0` was weaker than the shipped gate and could pass
/// while the production gate closed — it did not prove "boost retained"). Using
/// the same predicate + the same deduplicated denominator the gate uses proves
/// the gate stays OPEN where real evidence exists, so it discriminates by
/// evidence rather than blanket-dropping agent-judged boosts.
#[test]
fn p3_probe_fact_c_gate_retains_boosts_where_evidence_exists() {
    use crate::search::ranking::{distinct_expanded_query_tokens, has_topical_evidence};

    let dec_results = results_for(DECISION_QUERY, None);
    dump(
        "decision query / fused (fact c — decision control)",
        &dec_results,
    );
    let dec = find(&dec_results, DECISION_SEED).unwrap_or_else(|| {
        panic!(
            "(c) decision seed `{DECISION_SEED}` absent from its own fused query pool. \
             ids={:?}",
            dec_results.iter().map(|r| &r.entry.id).collect::<Vec<_>>()
        )
    });
    let dec_q_tokens = distinct_expanded_query_tokens(DECISION_QUERY);
    assert!(
        has_topical_evidence(&dec.score, dec_q_tokens),
        "(c) decision seed `{DECISION_SEED}` must PASS the production gate under its OWN \
         query (gate stays OPEN → DECISION_BOOST retained); has_topical_evidence was \
         false. got fts={} sym={} vec={} q_tokens={dec_q_tokens} \
         (overlap={:.4}).",
        dec.score.fts,
        dec.score.symbolic,
        dec.score.vector,
        dec.score.symbolic * dec_q_tokens as f64
    );

    let gov_results = results_for(GOVERNANCE_QUERY, None);
    dump(
        "governance query / fused (fact c — governance control)",
        &gov_results,
    );
    let gov = find(&gov_results, GOV_SEED).unwrap_or_else(|| {
        panic!(
            "(c) governance seed `{GOV_SEED}` absent from its own fused query pool. \
             ids={:?}",
            gov_results.iter().map(|r| &r.entry.id).collect::<Vec<_>>()
        )
    });
    let gov_q_tokens = distinct_expanded_query_tokens(GOVERNANCE_QUERY);
    assert!(
        has_topical_evidence(&gov.score, gov_q_tokens),
        "(c) governance seed `{GOV_SEED}` must PASS the production gate under its OWN \
         query (gate stays OPEN → DECISION_BOOST retained); has_topical_evidence was \
         false. got fts={} sym={} vec={} q_tokens={gov_q_tokens} \
         (overlap={:.4}).",
        gov.score.fts,
        gov.score.symbolic,
        gov.score.vector,
        gov.score.symbolic * gov_q_tokens as f64
    );
}
