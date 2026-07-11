//! Candidate scoring and top-k ranking for hybrid search.

use rusqlite::Connection;
use std::collections::HashMap;

use crate::{
    db::{get_access_times, get_superseded_ids},
    error::MemoryError,
    scorer::{cosine_similarity, is_id_like_exact_query, DecayPolicyContext},
    types::{HybridScore, MemoryEntry, SearchResult},
};

use super::{
    decay_policy,
    expansion::symbolic_query_with_expansion,
    filtering::{is_search_noise_entry, newest_by_shared_entity, quality_multiplier, valid_at},
    recall_config, resolve_weights, SearchOptions,
};

pub(super) struct CandidateRanking<'a> {
    pub(super) query: &'a str,
    pub(super) opts: &'a SearchOptions,
    pub(super) entries_map: HashMap<String, MemoryEntry>,
    pub(super) vec_scores: &'a HashMap<String, f64>,
    pub(super) fts_scores: &'a HashMap<String, f64>,
    pub(super) exact_id: Option<&'a str>,
    pub(super) include_superseded: bool,
    pub(super) as_of_utc: Option<&'a str>,
}

pub(super) fn rank_candidate_entries(
    conn: &Connection,
    ranking: CandidateRanking<'_>,
) -> Result<Vec<SearchResult>, MemoryError> {
    let CandidateRanking {
        query,
        opts,
        entries_map,
        vec_scores,
        fts_scores,
        exact_id,
        include_superseded,
        as_of_utc,
    } = ranking;
    let symbolic_scores = symbolic_scores(query, &entries_map);
    let fetched_ids_vec: Vec<String> = entries_map.keys().cloned().collect();
    let superseded_ids = get_superseded_ids(conn, &fetched_ids_vec)?;

    let entries_ref: HashMap<String, &MemoryEntry> = entries_map
        .iter()
        .filter(|(id, e)| {
            if !valid_at(e, as_of_utc) {
                return false;
            }
            if !include_superseded && superseded_ids.contains(*id) {
                return false;
            }
            if is_search_noise_entry(e, opts.path_prefix.as_deref()) {
                return false;
            }
            if let Some(prefix) = &opts.path_prefix {
                if !e.path.starts_with(prefix.as_str()) {
                    return false;
                }
            }
            if let Some(domain) = &opts.domain {
                match &e.domain {
                    Some(d) if d == domain => {}
                    _ => return false,
                }
            }
            true
        })
        .map(|(k, v)| (k.clone(), v))
        .collect();

    if entries_ref.is_empty() {
        return Ok(vec![]);
    }

    let candidate_ids_vec: Vec<String> = entries_ref.keys().cloned().collect();
    let access_times = get_access_times(conn, &candidate_ids_vec)?;
    let weights = resolve_weights(opts);
    let mut scores = crate::scorer::hybrid_score_with_policy(
        &entries_ref,
        vec_scores,
        fts_scores,
        &symbolic_scores,
        &weights,
        &access_times,
        DecayPolicyContext::new(recall_config(opts), decay_policy(opts)),
    );
    if let Some(exact_id) = exact_id.filter(|id| entries_ref.contains_key(*id)) {
        scores.insert(
            exact_id.to_string(),
            HybridScore {
                vector: 1.0,
                fts: 1.0,
                symbolic: 1.0,
                decay: 1.0,
                final_score: 10.0,
            },
        );
    }

    if include_superseded {
        for id in &superseded_ids {
            if let Some(score) = scores.get_mut(id) {
                score.final_score *= 0.3;
            }
        }
    }

    apply_precision_boosts(query, opts, &entries_ref, &weights, &mut scores);
    apply_quality_boosts(opts.path_prefix.as_deref(), &entries_ref, &mut scores);
    apply_access_feedback(&entries_ref, &mut scores);
    apply_tier_boosts(&entries_ref, &mut scores);
    apply_entity_recency_boosts(&entries_ref, &superseded_ids, &mut scores);
    apply_decision_and_research_boosts(query, &entries_ref, &mut scores);
    apply_lexical_overlap_boost(query, opts.path_prefix.as_deref(), &entries_ref, &mut scores);

    // Decorate each candidate with its parsed instant once (epoch millis), then
    // sort — the comparator compares the pre-parsed key, never the raw string
    // (tachi#718 CP2/CP3).
    let mut ranked: Vec<(&String, f64, i64)> = scores
        .iter()
        .filter(|(id, _)| entries_ref.contains_key(*id))
        .map(|(id, hs)| {
            let ms = entries_ref
                .get(id)
                .map(|e| crate::scorer::timestamp_epoch_millis(&e.timestamp))
                .unwrap_or(i64::MIN);
            (id, hs.final_score, ms)
        })
        .collect();
    ranked.sort_by(|a, b| crate::scorer::cmp_recall_rank((a.1, a.2, a.0), (b.1, b.2, b.0)));

    let ranked_ids: Vec<String> = if let Some(threshold) = opts.mmr_threshold {
        apply_mmr_diversity(&ranked, &entries_map, threshold, opts.top_k)
    } else {
        ranked.iter().map(|(id, _, _)| id.to_string()).collect()
    };
    drop(entries_ref);

    let mut entries_map = entries_map;
    Ok(ranked_ids
        .iter()
        .take(opts.top_k)
        .filter_map(|id| {
            let entry = entries_map.remove(id)?;
            let score = scores.get(id)?.clone();
            Some(SearchResult { entry, score })
        })
        .collect())
}

fn symbolic_scores(
    query: &str,
    entries_map: &HashMap<String, MemoryEntry>,
) -> HashMap<String, f64> {
    let symbolic_query = symbolic_query_with_expansion(query);
    entries_map
        .iter()
        .map(|(id, entry)| {
            let score = crate::scorer::symbolic_score_entry(&symbolic_query, entry);
            (id.clone(), score)
        })
        .collect()
}

fn apply_precision_boosts(
    query: &str,
    opts: &SearchOptions,
    entries_ref: &HashMap<String, &MemoryEntry>,
    weights: &crate::scorer::HybridWeights,
    scores: &mut HashMap<String, HybridScore>,
) {
    let is_id_like = is_id_like_exact_query(query);
    for (id, entry) in entries_ref {
        let mut multiplier = crate::scorer::generic_precision_multiplier_impl_with_config(
            is_id_like,
            query,
            entry,
            recall_config(opts),
        );
        for matcher in &opts.precision_matchers {
            if let Some(boost) = matcher.boost(query, entry) {
                if boost.is_finite() && boost > 1.0 {
                    multiplier = multiplier.max(boost);
                }
            }
        }
        if multiplier > 1.0 {
            if !weights.use_rrf {
                multiplier = multiplier.clamp(1.0, 3.0);
            }
            if let Some(score) = scores.get_mut(id) {
                score.final_score *= multiplier;
                if multiplier >= 10.0 {
                    score.symbolic = 1.0;
                }
            }
        }
    }
}

fn apply_quality_boosts(
    path_prefix: Option<&str>,
    entries_ref: &HashMap<String, &MemoryEntry>,
    scores: &mut HashMap<String, HybridScore>,
) {
    let top_pre_quality_score = scores
        .values()
        .map(|score| score.final_score)
        .filter(|score| score.is_finite())
        .fold(0.0_f64, f64::max);
    let quality_boost_floor = top_pre_quality_score * 0.85;
    for (id, entry) in entries_ref {
        let multiplier = quality_multiplier(entry, path_prefix);
        if (multiplier - 1.0).abs() > f64::EPSILON {
            if let Some(score) = scores.get_mut(id) {
                if multiplier > 1.0 && score.final_score < quality_boost_floor {
                    continue;
                }
                score.final_score *= multiplier;
            }
        }
    }
}

/// Same-store precision helpers for ops-audit / #708 Phase D follow-ons.
///
/// - High-importance **decisions** must surface over keyword-flooded wiki/stubs.
/// - When the **query** looks research-shaped, `/wiki/**/research/**` notes
///   get a path boost so denser architecture wikis do not always steal rank 1.
///
/// Provisional multipliers — calibrate only via ops_audit + golden_corpus.
fn apply_decision_and_research_boosts(
    query: &str,
    entries_ref: &HashMap<String, &MemoryEntry>,
    scores: &mut HashMap<String, HybridScore>,
) {
    /// importance floor for decision promotion (matches ops-audit decision seeds).
    const DECISION_IMPORTANCE_FLOOR: f64 = 0.85;
    /// provisional decision boost (tachi#708/#896 same-store precision).
    const DECISION_BOOST: f64 = 1.55;
    /// Provisional governance-intent decision boost (tachi#958): a query
    /// asking for a governance framing seeks the owner decision, not the
    /// repeated inventory vocabulary of registry stubs.
    const GOVERNANCE_DECISION_BOOST: f64 = 1.20;
    /// provisional research-path boost under /wiki/**/research/**
    /// (calibrated so labeled research notes beat denser architecture wikis
    /// on the ops-audit adjacent-wiki case).
    const RESEARCH_PATH_BOOST: f64 = 2.85;

    let research_query = query_looks_research_shaped(query);
    let governance_query = query_looks_governance_shaped(query);

    for (id, entry) in entries_ref {
        let mut mult = 1.0_f64;
        if entry.category.eq_ignore_ascii_case("decision")
            && entry.importance >= DECISION_IMPORTANCE_FLOOR
        {
            mult *= DECISION_BOOST;
            if governance_query {
                mult *= GOVERNANCE_DECISION_BOOST;
            }
        }
        if research_query && is_research_wiki_path(&entry.path) {
            mult *= RESEARCH_PATH_BOOST;
        }
        if mult > 1.0 {
            if let Some(score) = scores.get_mut(id) {
                if score.final_score.is_finite() && score.final_score > 0.0 {
                    score.final_score *= mult;
                }
            }
        }
    }
}

fn query_looks_research_shaped(query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    q.contains("research")
        || q.contains("hindsight")
        || q.contains("study")
        || q.contains("paper")
        || q.contains("arxiv")
        || q.contains("evaluation protocol")
}

fn query_looks_governance_shaped(query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    q.contains("governance")
        || q.contains("framing")
        || q.contains("owner stance")
        || q.contains("owner ratified")
        || q.contains("adjudicat")
}

fn is_research_wiki_path(path: &str) -> bool {
    // Allocation-free case-insensitive scan (Gemini #903): avoid
    // `to_ascii_lowercase()` per candidate under load. Match any path segment
    // whose name starts with `research` (covers /research, /research-*, /research_*).
    if path.len() < 6 || !path.as_bytes()[..6].eq_ignore_ascii_case(b"/wiki/") {
        return false;
    }
    path.as_bytes()
        .windows(9)
        .any(|w| w.eq_ignore_ascii_case(b"/research"))
}


/// Phase C (#708): lexical-overlap precision boost via soft-stem token coverage
/// and char 4-gram Jaccard. Lifts paraphrase-heavy summary queries without
/// needing external rerank.
fn apply_lexical_overlap_boost(
    query: &str,
    path_prefix: Option<&str>,
    entries_ref: &HashMap<String, &MemoryEntry>,
    scores: &mut HashMap<String, HybridScore>,
) {
    let wiki_scoped = path_prefix.is_some_and(|p| p == "/wiki" || p.starts_with("/wiki/"));
    let guide_scoped = path_prefix.is_some_and(|p| p == "/guide" || p.starts_with("/guide/"));

    // Calibrated on golden_corpus summary misses under rrf_k=20; re-measure
    // ops_audit + golden before changing.
    const TOKEN_COVERAGE_FLOOR: f64 = 0.40;
    const NGRAM_JACCARD_FLOOR: f64 = 0.12;
    const MAX_BOOST: f64 = 2.4;

    let q_tokens = soft_token_set(query);
    let q_ngrams = char_ngrams(query, 4);
    if q_tokens.len() < 3 && q_ngrams.len() < 8 {
        return;
    }

    for (id, entry) in entries_ref {
        // Unscoped mixed search: do not let dense wiki/guide bags steal paraphrase
        // boost from notes (ops-audit adjacent-wiki + golden summary).
        if entry.is_wiki() && !wiki_scoped {
            continue;
        }
        if entry.is_guide() && !guide_scoped {
            continue;
        }
        let mut text = String::new();
        text.push_str(&entry.summary);
        text.push(' ');
        text.push_str(&entry.text);
        for kw in &entry.keywords {
            text.push(' ');
            text.push_str(kw);
        }
        let e_tokens = soft_token_set(&text);
        let e_ngrams = char_ngrams(&text, 4);
        if e_tokens.is_empty() && e_ngrams.is_empty() {
            continue;
        }

        let token_cov = if q_tokens.is_empty() {
            0.0
        } else {
            q_tokens.intersection(&e_tokens).count() as f64 / q_tokens.len() as f64
        };
        let jaccard = if q_ngrams.is_empty() || e_ngrams.is_empty() {
            0.0
        } else {
            let inter = q_ngrams.intersection(&e_ngrams).count() as f64;
            let union = q_ngrams.union(&e_ngrams).count() as f64;
            inter / union.max(1.0)
        };

        if token_cov < TOKEN_COVERAGE_FLOOR && jaccard < NGRAM_JACCARD_FLOOR {
            continue;
        }

        // Blend: stronger signal wins; clamp boost.
        let strength = (token_cov * 1.2 + jaccard * 3.0).clamp(0.0, 1.5);
        let boost = 1.0 + (MAX_BOOST - 1.0) * (strength / 1.5);
        if boost > 1.0 + f64::EPSILON {
            if let Some(score) = scores.get_mut(id) {
                if score.final_score.is_finite() && score.final_score > 0.0 {
                    score.final_score *= boost;
                }
            }
        }
    }
}

fn soft_token_set(text: &str) -> std::collections::HashSet<String> {
    crate::scorer::tokenize(text)
        .into_iter()
        .map(|t| soft_stem_token(&t))
        .filter(|t| t.chars().count() >= 2)
        .collect()
}

fn soft_stem_token(token: &str) -> String {
    let mut s = token.to_ascii_lowercase();
    if !s.chars().all(|c| c.is_ascii_alphabetic()) {
        return s;
    }
    for _ in 0..2 {
        let before = s.clone();
        if s.len() > 5 && s.ends_with("tion") {
            s.truncate(s.len() - 4);
        } else if s.len() > 5 && s.ends_with("ing") {
            s.truncate(s.len() - 3);
        } else if s.len() > 5 && s.ends_with("ies") {
            s.truncate(s.len() - 3);
            s.push('y');
        } else if s.len() > 4 && (s.ends_with("ed") || s.ends_with("es")) {
            s.truncate(s.len() - 2);
        } else if s.len() > 5 && s.ends_with("ure") {
            // failure → fail
            s.truncate(s.len() - 3);
        } else if s.len() > 3 && s.ends_with('s') {
            s.truncate(s.len() - 1);
        }
        if s == before {
            break;
        }
    }
    s
}

fn char_ngrams(text: &str, n: usize) -> std::collections::HashSet<String> {
    let chars: Vec<char> = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || crate::noise::is_cjk(*c))
        .collect();
    if chars.len() < n {
        return std::collections::HashSet::new();
    }
    chars
        .windows(n)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

fn apply_access_feedback(
    entries_ref: &HashMap<String, &MemoryEntry>,
    scores: &mut HashMap<String, HybridScore>,
) {
    for (id, entry) in entries_ref {
        if entry.access_count >= 2 {
            let boost = 1.0 + (entry.access_count as f64).ln_1p() * 0.03;
            if let Some(score) = scores.get_mut(id) {
                score.final_score *= boost.min(1.25);
            }
        }
    }
}

fn apply_tier_boosts(
    entries_ref: &HashMap<String, &MemoryEntry>,
    scores: &mut HashMap<String, HybridScore>,
) {
    for (id, entry) in entries_ref {
        // Wiki pattern/consolidated pages already carry dense keyword bags; keep
        // tier boosts milder so labeled research notes can compete (ops-audit
        // adjacent-wiki). Non-wiki pattern knowledge retains the stronger lift.
        let tier_multiplier = match (entry.tier.as_str(), entry.is_wiki()) {
            ("pattern", true) => 1.05,
            ("pattern", false) => 1.15,
            ("consolidated", true) => 1.03,
            ("consolidated", false) => 1.08,
            _ => 1.0,
        };
        if tier_multiplier > 1.0 {
            if let Some(score) = scores.get_mut(id) {
                score.final_score *= tier_multiplier;
            }
        }
    }
}

fn apply_entity_recency_boosts(
    entries_ref: &HashMap<String, &MemoryEntry>,
    superseded_ids: &std::collections::HashSet<String>,
    scores: &mut HashMap<String, HybridScore>,
) {
    let newest_by_entity = newest_by_shared_entity(entries_ref);
    for id in newest_by_entity {
        if !superseded_ids.contains(&id) {
            if let Some(score) = scores.get_mut(&id) {
                score.final_score *= 1.08;
            }
        }
    }
}

/// MMR-inspired diversity filter: greedily select results that are both
/// relevant (high score) and diverse (low similarity to already-selected).
///
/// Candidates with cosine similarity > `threshold` to any already-selected
/// entry are deferred to the end rather than dropped entirely.
fn apply_mmr_diversity(
    ranked: &[(&String, f64, i64)],
    entries: &HashMap<String, MemoryEntry>,
    threshold: f64,
    needed: usize,
) -> Vec<String> {
    if ranked.len() <= 1 {
        return ranked.iter().map(|(id, _, _)| id.to_string()).collect();
    }

    let mut selected: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    for (idx, (id, _, _)) in ranked.iter().enumerate() {
        if selected.len() >= needed {
            deferred.extend(
                ranked[idx..]
                    .iter()
                    .map(|(rest_id, _, _)| (*rest_id).to_string()),
            );
            break;
        }

        let candidate = entries.get(*id);
        let c_vec = candidate.and_then(|e| e.vector.as_ref());

        let too_similar = selected.iter().any(|sel_id| {
            let sel_entry = entries.get(sel_id);
            let s_vec = sel_entry.and_then(|e| e.vector.as_ref());

            match (s_vec, c_vec) {
                (Some(sv), Some(cv)) if !sv.is_empty() && !cv.is_empty() => {
                    cosine_similarity(sv, cv) > threshold
                }
                _ => false,
            }
        });

        if too_similar {
            deferred.push(id.to_string());
        } else {
            selected.push(id.to_string());
        }
    }

    selected.extend(deferred);
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    // tachi#911 follow-up to #903: the golden/ops-audit corpora only guard
    // recall@10 / MRR floors and rank-1 promotion for specific known-broken
    // cases; nothing asserted that the DECISION_BOOST (1.55x) / RESEARCH_PATH_BOOST
    // (2.85x) multipliers leave an *already-correct* rank order unchanged.
    // These tests exercise `apply_decision_and_research_boosts` directly
    // (unit-level, no DB) against seeded scores whose pre-boost order already
    // matches the intended/golden order, and assert the boost does not
    // reshuffle it.

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
            last_access: None,
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
        // None of these entries are decision-category-above-floor, and the
        // query is not research-shaped, so neither boost should apply at
        // all: scores and the already-correct rank order must be untouched.
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

        apply_decision_and_research_boosts(
            "ordinary lookup query with no special terms",
            &entries_ref,
            &mut scores,
        );

        assert_eq!(scores.get("a").unwrap().final_score, before["a"].final_score);
        assert_eq!(scores.get("b").unwrap().final_score, before["b"].final_score);
        assert_eq!(scores.get("c").unwrap().final_score, before["c"].final_score);
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

        let mut scores: HashMap<String, HybridScore> = HashMap::new();
        scores.insert("d1".to_string(), score(3.0));
        scores.insert("d2".to_string(), score(2.0));
        scores.insert("d3".to_string(), score(1.0));

        apply_decision_and_research_boosts(
            "ordinary lookup query with no special terms",
            &entries_ref,
            &mut scores,
        );

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

        let mut scores: HashMap<String, HybridScore> = HashMap::new();
        scores.insert("ops-project-decision-priority".to_string(), score(1.2));
        scores.insert("ops-roadmap-noise".to_string(), score(1.0));
        scores.insert("ops-review-noise".to_string(), score(0.9));

        apply_decision_and_research_boosts(
            "what is the current open issue priority project decision for this sprint",
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
    fn research_path_boost_preserves_relative_order_among_equally_qualifying_entries() {
        // Two wiki/research-path entries under a research-shaped query both
        // qualify for RESEARCH_PATH_BOOST; the uniform multiplier must not
        // invert their existing relative order.
        let entries: HashMap<String, MemoryEntry> = [
            entry("r1", "/wiki/research/hindsight-eval", "fact", 0.7),
            entry("r2", "/wiki/research/protocol-notes", "fact", 0.7),
        ]
        .into_iter()
        .map(|e| (e.id.clone(), e))
        .collect();
        let entries_ref: HashMap<String, &MemoryEntry> =
            entries.iter().map(|(k, v)| (k.clone(), v)).collect();

        let mut scores: HashMap<String, HybridScore> = HashMap::new();
        scores.insert("r1".to_string(), score(2.0));
        scores.insert("r2".to_string(), score(1.0));

        apply_decision_and_research_boosts(
            "hindsight research evaluation protocol for memory recall quality",
            &entries_ref,
            &mut scores,
        );

        assert_eq!(scores.get("r1").unwrap().final_score, 2.0 * 2.85);
        assert_eq!(scores.get("r2").unwrap().final_score, 1.0 * 2.85);
        assert_eq!(
            ranked_ids(&scores, &["r1", "r2"]),
            vec!["r1".to_string(), "r2".to_string()]
        );
    }
}
