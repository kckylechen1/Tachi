//! Phase 2 **P3 Step-0 PROBE** — measurement only, NOT the provenance band.
//!
//! # Why this exists
//! Glinda's P3 spec proposes a *topical-evidence gate*: agent-judged boosts
//! (DECISION_BOOST, tier) apply to a candidate ONLY if that candidate has
//! topical evidence in at least one retrieval channel
//! (`fts > 0 || symbolic > 0 || vector > 0`). The spec's load-bearing numbers
//! were **derived from PRE-P2 attribution data**, before the P2 surface split
//! re-cut the `hindsight-research-wiki` case to `Surface::Memory`. This probe
//! re-measures those numbers on the **real post-P2 ranking pipeline** so the
//! band is never built on a stale assumption.
//!
//! It confirms three facts inside the Memory surface for the hindsight query
//! (`"hindsight research evaluation protocol for memory recall quality"`), the
//! same corpus + `search_opts` the re-cut P2 ratchet uses
//! (`ops_audit_corpus.rs`, reused via `pub(super)` — identical fixture):
//!
//! * **(a)** the decision seed `ops-project-decision-priority` has
//!   `fts == 0.0 && symbolic == 0.0` (so the gate would CLOSE on it — its rank-2
//!   is an artifact of DECISION_BOOST + the recency tie-break, not topical
//!   relevance);
//! * **(b)** the research note `ops-wiki-research-hindsight` has
//!   `symbolic > 0.0` (it IS the real topical scorer) AND entity-recency ×1.08
//!   fires for it within Memory (Glinda's ~7.5% winning margin depends on it);
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
//! For fact (b)'s entity-recency check the probe calls the PRODUCTION
//! `filtering::newest_by_shared_entity` on the actual returned Memory pool — it
//! does not re-implement the rule. (Its `len > 1` "shared" guard is the crux:
//! within Memory the three `Hindsight`-tagged architecture wikis are
//! `Surface::Docs` and excluded at candidate collection, so if the note is the
//! sole Memory holder of `Hindsight` the boost does NOT fire post-P2. That is
//! precisely the pre-P2→post-P2 drift this probe is built to catch.)
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

/// FACT (a): the decision seed has ZERO topical evidence inside the Memory
/// surface for the hindsight query — so the topical-evidence gate CLOSES on it
/// and drops its DECISION_BOOST. If `fts` or `symbolic` is nonzero here the
/// gate as designed would NOT fire; the assertion message carries the actual
/// values for that case.
#[test]
fn p3_probe_fact_a_decision_seed_has_no_topical_evidence_in_memory() {
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

    assert_eq!(
        dec.score.fts, 0.0,
        "(a) decision seed `{DECISION_SEED}` fts must be 0.0 for the topical-evidence \
         gate to close on it; got fts={} (sym={}, vec={}). If nonzero, symbolic/FTS \
         expansion handed it a token and the gate would NOT fire — STOP and reconsider.",
        dec.score.fts, dec.score.symbolic, dec.score.vector
    );
    assert_eq!(
        dec.score.symbolic, 0.0,
        "(a) decision seed `{DECISION_SEED}` symbolic must be 0.0 for the gate to close; \
         got sym={} (fts={}, vec={}). If nonzero, symbolic expansion gave it a token and \
         the gate would NOT fire — STOP and reconsider.",
        dec.score.symbolic, dec.score.fts, dec.score.vector
    );
}

/// FACT (b): the research note IS the real topical scorer (`symbolic > 0`) AND
/// entity-recency ×1.08 fires for it within Memory (Glinda's ~7.5% margin
/// depends on it). The entity-recency half is the highest-risk claim: within
/// Memory the `Hindsight`-tagged architecture wikis are Docs (excluded), so if
/// the note is the sole Memory holder of `Hindsight`, `newest_by_shared_entity`
/// (`len > 1` guard) returns it EMPTY and the boost does not fire — a genuine
/// post-P2 regression of the spec's pre-P2 derivation.
#[test]
fn p3_probe_fact_b_research_note_is_topical_and_entity_recent_in_memory() {
    let results = results_for(HINDSIGHT_QUERY, Some(Surface::Memory));
    dump("hindsight query / Surface::Memory (fact b)", &results);

    let note = find(&results, RESEARCH_NOTE).unwrap_or_else(|| {
        panic!(
            "(b) research note `{RESEARCH_NOTE}` absent from the Memory pool. ids={:?}",
            results.iter().map(|r| &r.entry.id).collect::<Vec<_>>()
        )
    });

    assert!(
        note.score.symbolic > 0.0,
        "(b) research note `{RESEARCH_NOTE}` must be the real topical scorer \
         (symbolic > 0.0); got sym={} (fts={}, vec={}).",
        note.score.symbolic,
        note.score.fts,
        note.score.vector
    );

    let entity_recent = entity_recency_ids(&results);
    println!(
        "── P3 PROBE: newest_by_shared_entity set within Memory = {:?}",
        entity_recent
    );
    assert!(
        entity_recent.contains(RESEARCH_NOTE),
        "(b) entity-recency ×1.08 must fire for `{RESEARCH_NOTE}` within Memory \
         (Glinda's ~7.5% winning margin depends on it), but it is NOT in the \
         production `newest_by_shared_entity` set = {:?}. Most likely cause: within \
         Surface::Memory the note is the SOLE holder of its `Hindsight` entity (the \
         Hindsight-tagged architecture wikis are Surface::Docs, excluded at candidate \
         collection), so the `len > 1` shared-entity guard drops it and the boost does \
         not fire. This is the pre-P2→post-P2 drift the probe exists to catch — STOP, \
         report, reconsider the margin.",
        entity_recent
    );
}

/// FACT (c) — control: the gate must NOT close everywhere. Under their OWN
/// queries the decision seed and the governance seed each carry real topical
/// evidence (`fts > 0 || symbolic > 0`), so the gate keeps their boosts. This
/// proves the gate discriminates by evidence rather than blanket-dropping
/// agent-judged boosts.
#[test]
fn p3_probe_fact_c_gate_retains_boosts_where_evidence_exists() {
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
    assert!(
        dec.score.fts > 0.0 || dec.score.symbolic > 0.0,
        "(c) decision seed `{DECISION_SEED}` must retain topical evidence under its OWN \
         query (gate stays OPEN); got fts={} sym={} vec={}.",
        dec.score.fts,
        dec.score.symbolic,
        dec.score.vector
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
    assert!(
        gov.score.fts > 0.0 || gov.score.symbolic > 0.0,
        "(c) governance seed `{GOV_SEED}` must retain topical evidence under its OWN \
         query (gate stays OPEN); got fts={} sym={} vec={}.",
        gov.score.fts,
        gov.score.symbolic,
        gov.score.vector
    );
}
