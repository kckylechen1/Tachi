use super::*;
use chrono::Utc;

// tachi#911 follow-up to #903: the golden/ops-audit corpora only guard
// recall@10 / MRR floors and rank-1 promotion for specific known-broken
// cases; nothing asserted that the DECISION_BOOST (1.55x) multiplier
// leaves an *already-correct* rank order unchanged. These tests exercise
// `apply_decision_boost` directly (unit-level, no DB) against seeded
// scores whose pre-boost order already matches the intended/golden order,
// and assert the boost does not reshuffle it.

fn entry(id: &str, path: &str, category: &str, importance: f64) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: path.to_string(),
        summary: id.to_string(),
        text: id.to_string(),
        importance,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: category.to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "test".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: serde_json::json!({}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn score(final_score: f64) -> HybridScore {
    HybridScore {
        vector: 0.0,
        fts: 0.0,
        symbolic: 0.0,
        decay: 0.0,
        final_score,
    }
}

/// A score whose channels carry real topical evidence (fts above the P3
/// gate epsilon), so `apply_decision_boost`'s topical-evidence gate opens.
/// Order-preservation tests use this because P3 gates the boost on a real
/// channel match — a zero-channel `score()` now legitimately keeps its
/// mechanical value.
fn score_ev(final_score: f64) -> HybridScore {
    HybridScore {
        vector: 0.0,
        fts: 0.55,
        symbolic: 0.0,
        decay: 0.0,
        final_score,
    }
}

fn lexical_entry(id: &str, text: &str) -> MemoryEntry {
    let mut entry = entry(id, &format!("/notes/{id}"), "fact", 0.5);
    entry.summary = text.to_string();
    entry.text = text.to_string();
    entry
}

/// Ranks (descending by `final_score`) among `ids`, using the same
/// tie-break precedence (`final_score` desc) the rest of this module
/// uses; ties are not exercised by these fixtures.
fn ranked_ids(scores: &HashMap<String, HybridScore>, ids: &[&str]) -> Vec<String> {
    let mut ranked: Vec<(&str, f64)> = ids
        .iter()
        .map(|id| (*id, scores.get(*id).unwrap().final_score))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    ranked.into_iter().map(|(id, _)| id.to_string()).collect()
}

#[test]
fn boosts_are_noop_when_no_entry_qualifies() {
    // None of these entries are decision-category-above-floor, so the
    // decision boost must not apply at all: scores and the already-correct
    // rank order must be untouched.
    let entries: HashMap<String, MemoryEntry> = [
        entry("a", "/notes/a", "fact", 0.9),
        entry("b", "/notes/b", "fact", 0.6),
        entry("c", "/guide/c", "howto", 0.95),
    ]
    .into_iter()
    .map(|e| (e.id.clone(), e))
    .collect();
    let entries_ref: HashMap<String, &MemoryEntry> =
        entries.iter().map(|(k, v)| (k.clone(), v)).collect();

    let mut scores: HashMap<String, HybridScore> = HashMap::new();
    scores.insert("a".to_string(), score(3.0));
    scores.insert("b".to_string(), score(2.0));
    scores.insert("c".to_string(), score(1.0));
    let before = scores.clone();

    // Query is irrelevant here: no entry is a qualifying decision, so the
    // boost never applies regardless of the topical-evidence gate.
    apply_decision_boost("open issue priority decision", &entries_ref, &mut scores);

    assert_eq!(
        scores.get("a").unwrap().final_score,
        before["a"].final_score
    );
    assert_eq!(
        scores.get("b").unwrap().final_score,
        before["b"].final_score
    );
    assert_eq!(
        scores.get("c").unwrap().final_score,
        before["c"].final_score
    );
    assert_eq!(
        ranked_ids(&scores, &["a", "b", "c"]),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

#[test]
fn decision_boost_preserves_relative_order_among_equally_qualifying_entries() {
    // All three entries qualify for DECISION_BOOST (category=decision,
    // importance >= floor). A uniform multiplier applied to every
    // candidate in a set must never invert their existing relative
    // order — guards the specific "already-correct ranks get reshuffled"
    // regression risk in #903/#911.
    let entries: HashMap<String, MemoryEntry> = [
        entry("d1", "/notes/decisions/1", "decision", 0.95),
        entry("d2", "/notes/decisions/2", "decision", 0.9),
        entry("d3", "/notes/decisions/3", "decision", 0.85),
    ]
    .into_iter()
    .map(|e| (e.id.clone(), e))
    .collect();
    let entries_ref: HashMap<String, &MemoryEntry> =
        entries.iter().map(|(k, v)| (k.clone(), v)).collect();

    // Each carries real topical evidence (fts above the gate epsilon), so
    // the P3 gate opens and the boost fires — the property under test is
    // that a uniform boost does not invert an already-correct order.
    let mut scores: HashMap<String, HybridScore> = HashMap::new();
    scores.insert("d1".to_string(), score_ev(3.0));
    scores.insert("d2".to_string(), score_ev(2.0));
    scores.insert("d3".to_string(), score_ev(1.0));

    apply_decision_boost("open issue priority decision", &entries_ref, &mut scores);

    // Every score moved (boost applied) ...
    assert_eq!(scores.get("d1").unwrap().final_score, 3.0 * 1.55);
    assert_eq!(scores.get("d2").unwrap().final_score, 2.0 * 1.55);
    assert_eq!(scores.get("d3").unwrap().final_score, 1.0 * 1.55);
    // ... but the already-correct rank order is unchanged.
    assert_eq!(
        ranked_ids(&scores, &["d1", "d2", "d3"]),
        vec!["d1".to_string(), "d2".to_string(), "d3".to_string()]
    );
}

#[test]
fn already_correct_ops_audit_style_decision_rank_is_not_inverted() {
    // Mirrors the ops-audit "recent project decision vs. older roadmap
    // noise" shape (ops_audit_corpus.rs case 1), but seeded so the
    // decision is *already* ranked first pre-boost — the boost must not
    // invert an already-correct order due to overshoot.
    let entries: HashMap<String, MemoryEntry> = [
        entry(
            "ops-project-decision-priority",
            "/notes/project/decisions",
            "decision",
            0.9,
        ),
        entry("ops-roadmap-noise", "/notes/roadmap", "fact", 0.7),
        entry("ops-review-noise", "/notes/review", "fact", 0.7),
    ]
    .into_iter()
    .map(|e| (e.id.clone(), e))
    .collect();
    let entries_ref: HashMap<String, &MemoryEntry> =
        entries.iter().map(|(k, v)| (k.clone(), v)).collect();

    // The decision seed carries real topical evidence, so the P3 gate
    // opens and it is boosted; the noise rows are `fact` category and never
    // qualify. The boost must not invert the already-correct order.
    let mut scores: HashMap<String, HybridScore> = HashMap::new();
    scores.insert("ops-project-decision-priority".to_string(), score_ev(1.2));
    scores.insert("ops-roadmap-noise".to_string(), score(1.0));
    scores.insert("ops-review-noise".to_string(), score(0.9));

    apply_decision_boost(
        "open issue priority project decision sprint",
        &entries_ref,
        &mut scores,
    );

    assert_eq!(
        ranked_ids(
            &scores,
            &[
                "ops-project-decision-priority",
                "ops-roadmap-noise",
                "ops-review-noise"
            ]
        ),
        vec![
            "ops-project-decision-priority".to_string(),
            "ops-roadmap-noise".to_string(),
            "ops-review-noise".to_string(),
        ]
    );
}

#[test]
fn lexical_overlap_boost_requires_a_token_in_at_most_one_quarter_of_candidates() {
    // `alpha` occurs in two of five candidates (40%), so neither entry
    // that matches it may receive the auxiliary lexical boost. This is a
    // boundary proof for the exact candidate-pool cutoff; the former
    // rounded cutoff would have incorrectly treated two matches as <=25%.
    let entries: HashMap<String, MemoryEntry> = [
        lexical_entry("a", "alpha beta gamma"),
        lexical_entry("b", "alpha beta gamma"),
        lexical_entry("c", "beta gamma"),
        lexical_entry("d", "beta gamma"),
        lexical_entry("e", "beta gamma"),
    ]
    .into_iter()
    .map(|entry| (entry.id.clone(), entry))
    .collect();
    let entries_ref: HashMap<String, &MemoryEntry> = entries
        .iter()
        .map(|(id, entry)| (id.clone(), entry))
        .collect();
    let mut scores: HashMap<String, HybridScore> =
        entries.keys().map(|id| (id.clone(), score(1.0))).collect();

    apply_lexical_overlap_boost("alpha beta gamma", None, &entries_ref, &mut scores);

    for id in entries.keys() {
        assert_eq!(
            scores[id].final_score, 1.0,
            "{id} used only common query terms"
        );
    }
}

#[test]
fn lexical_overlap_boost_accepts_a_token_at_exactly_one_quarter_frequency() {
    // `alpha` occurs in one of four candidates (25%), so it is a
    // discriminative match and the lexical boost remains available.
    let entries: HashMap<String, MemoryEntry> = [
        lexical_entry("target", "alpha beta gamma"),
        lexical_entry("b", "beta gamma"),
        lexical_entry("c", "beta gamma"),
        lexical_entry("d", "beta gamma"),
    ]
    .into_iter()
    .map(|entry| (entry.id.clone(), entry))
    .collect();
    let entries_ref: HashMap<String, &MemoryEntry> = entries
        .iter()
        .map(|(id, entry)| (id.clone(), entry))
        .collect();
    let mut scores: HashMap<String, HybridScore> =
        entries.keys().map(|id| (id.clone(), score(1.0))).collect();

    apply_lexical_overlap_boost("alpha beta gamma", None, &entries_ref, &mut scores);

    assert!(scores["target"].final_score > 1.0);
    assert_eq!(scores["b"].final_score, 1.0);
    assert_eq!(scores["c"].final_score, 1.0);
    assert_eq!(scores["d"].final_score, 1.0);
}

#[test]
fn lexical_overlap_boost_is_disabled_for_a_pool_of_two() {
    let entries: HashMap<String, MemoryEntry> = [
        lexical_entry("target", "alpha beta gamma"),
        lexical_entry("other", "beta gamma"),
    ]
    .into_iter()
    .map(|entry| (entry.id.clone(), entry))
    .collect();
    let entries_ref: HashMap<String, &MemoryEntry> = entries
        .iter()
        .map(|(id, entry)| (id.clone(), entry))
        .collect();
    let mut scores: HashMap<String, HybridScore> =
        entries.keys().map(|id| (id.clone(), score(1.0))).collect();

    apply_lexical_overlap_boost("alpha beta gamma", None, &entries_ref, &mut scores);

    assert_eq!(scores["target"].final_score, 1.0);
    assert_eq!(scores["other"].final_score, 1.0);
}

// ---- Phase 2 P3: topical-evidence gate -------------------------------

fn channel_score(fts: f64, symbolic: f64, vector: f64) -> HybridScore {
    HybridScore {
        vector,
        fts,
        symbolic,
        decay: 0.0,
        final_score: 1.0,
    }
}

#[test]
fn has_topical_evidence_matches_measured_tuples() {
    // All tuples are the real post-P2 measurements from the p3 probe.
    // Dense-map noise floor: IDF-null bm25 crumb + a single shared token
    // (overlap 0.1*10 = 1.0 < 1.5) → CLOSED.
    assert!(!has_topical_evidence(&channel_score(1.2e-7, 0.1, 0.0), 10));
    // Genuine bm25 hit (fts 0.55 > 1e-3) → OPEN.
    assert!(has_topical_evidence(&channel_score(0.55, 0.2, 0.0), 10));
    // Symbolic-only, two-token overlap (0.2*10 = 2.0 >= 1.5) → OPEN.
    assert!(has_topical_evidence(&channel_score(0.0, 0.2, 0.0), 10));
    // Vector-only similarity → OPEN.
    assert!(has_topical_evidence(&channel_score(0.0, 0.0, 0.5), 10));
    // Single-token overlap floor (0.1*10 = 1.0 < 1.5) → CLOSED (boundary).
    assert!(!has_topical_evidence(&channel_score(0.0, 0.1, 0.0), 10));
}

#[test]
fn gate_failing_candidates_rank_purely_mechanically() {
    // Property: a candidate that FAILS the topical-evidence gate keeps its
    // exact mechanical final_score after `apply_decision_boost`, for ANY
    // category/importance — zero-evidence candidates rank purely
    // mechanically. Replaces the retired band-bound property test; a
    // deterministic grid stands in for a proptest generator (no proptest
    // dependency in this crate).
    let query = "open issue priority project decision sprint governance";
    // Use the production denominator so the test's gate pre-check matches
    // exactly what `apply_decision_boost` computes internally.
    let q_tokens = distinct_expanded_query_tokens(query);
    assert!(q_tokens > 0);
    // symbolic value whose overlap (symbolic * q_tokens) stays below the
    // two-token threshold, so the symbolic half of the gate fails.
    let sym_below_overlap = (MIN_SYMBOLIC_OVERLAP / q_tokens as f64) * 0.99;

    let ftss = [0.0_f64, 5e-4, FTS_TOPICAL_EPSILON]; // all <= epsilon → fts half fails
    let syms = [0.0_f64, sym_below_overlap]; // overlap < 1.5 → symbolic half fails
    let categories = ["decision", "DECISION", "fact", "note"];
    let importances = [0.5_f64, 0.85, 0.9, 1.0];
    let finals = [0.1_f64, 1.0, 3.0, 12.5];

    for &fts in &ftss {
        for &sym in &syms {
            for &cat in &categories {
                for &imp in &importances {
                    for &fin in &finals {
                        // vector stays 0.0 — a positive vector would OPEN the gate.
                        let mut sc = channel_score(fts, sym, 0.0);
                        sc.final_score = fin;
                        assert!(
                            !has_topical_evidence(&sc, q_tokens),
                            "grid point unexpectedly passes the gate: \
                                 fts={fts} sym={sym} q={q_tokens}"
                        );

                        let e = entry("x", "/notes/x", cat, imp);
                        let entries: HashMap<String, MemoryEntry> =
                            [(e.id.clone(), e)].into_iter().collect();
                        let entries_ref: HashMap<String, &MemoryEntry> =
                            entries.iter().map(|(k, v)| (k.clone(), v)).collect();
                        let mut scores: HashMap<String, HybridScore> =
                            [("x".to_string(), sc)].into_iter().collect();

                        apply_decision_boost(query, &entries_ref, &mut scores);

                        assert_eq!(
                            scores["x"].final_score, fin,
                            "zero-evidence candidate (cat={cat} imp={imp}) must keep its \
                                 mechanical final_score {fin}; the gate must have closed"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn repeated_query_term_does_not_leak_boost_via_nondedup_count() {
    // Regression for BUG F (codex cross-vendor review): the gate's q_tokens
    // MUST be the deduplicated distinct expanded-token count — the symbolic
    // scorer's own denominator (scorer/text.rs:90) — not `tokenize().len()`.
    // A query with a repeated term makes `tokenize().len() > distinct_Q`;
    // reconstructing overlap as `symbolic * non_dedup` then inflates a
    // single-distinct-token match past the threshold and leaks DECISION_BOOST.
    let query = "alpha alpha beta"; // neither token expands (scorer/text map)
    let expanded = symbolic_query_with_expansion(query);
    let non_dedup = crate::scorer::tokenize(&expanded).len();
    let distinct = distinct_expanded_query_tokens(query);

    // Premise guards: without an actual duplicate the regression can't
    // manifest and the test would be vacuous.
    assert!(
        non_dedup > distinct,
        "test premise: expected a repeated expanded token \
             (non_dedup={non_dedup} > distinct={distinct}); expanded={expanded:?}"
    );

    // A decision candidate matching exactly ONE distinct query token:
    // symbolic = 1/distinct, so deduped overlap = symbolic*distinct = 1.0
    // (< 1.5 → gate CLOSED after the fix), while the buggy non-dedup overlap
    // = symbolic*non_dedup would reach the threshold and OPEN the gate.
    let symbolic = 1.0 / distinct as f64;
    assert!(
        symbolic * non_dedup as f64 >= MIN_SYMBOLIC_OVERLAP,
        "test premise: non-dedup overlap {} must reach the threshold to \
             exercise the leak (else the test proves nothing)",
        symbolic * non_dedup as f64
    );
    // Sanity: the deduped (correct) overlap is a single token, below floor.
    assert!(symbolic * distinct as f64 <= 1.0 + f64::EPSILON);

    let e = entry("dec", "/notes/dec", "decision", 0.9);
    let entries: HashMap<String, MemoryEntry> = [(e.id.clone(), e)].into_iter().collect();
    let entries_ref: HashMap<String, &MemoryEntry> =
        entries.iter().map(|(k, v)| (k.clone(), v)).collect();
    let mut sc = channel_score(0.0, symbolic, 0.0);
    sc.final_score = 2.0;
    let mut scores: HashMap<String, HybridScore> = [("dec".to_string(), sc)].into_iter().collect();

    apply_decision_boost(query, &entries_ref, &mut scores);

    assert_eq!(
        scores["dec"].final_score,
        2.0,
        "BUG F: a single-distinct-token overlap under a repeated-term query must NOT \
             receive DECISION_BOOST — the gate must use the deduplicated distinct count \
             (symbolic*distinct = 1.0 < {MIN_SYMBOLIC_OVERLAP}), not tokenize().len()={non_dedup} \
             which inflates the overlap to {} and would leak the boost.",
        symbolic * non_dedup as f64
    );
}
