//! Candidate scoring and top-k ranking for hybrid search.

use rusqlite::Connection;
use std::collections::HashMap;

use crate::{
    db::{get_access_times, get_superseded_ids},
    error::MemoryError,
    scorer::{cosine_similarity, is_id_like_exact_query},
    types::{HybridScore, MemoryEntry, SearchResult},
};

use super::{
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
    let mut scores = crate::scorer::hybrid_score_with_config(
        &entries_ref,
        vec_scores,
        fts_scores,
        &symbolic_scores,
        &weights,
        &access_times,
        recall_config(opts),
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
    /// provisional research-path boost under /wiki/**/research/**
    /// (calibrated so labeled research notes beat denser architecture wikis
    /// on the ops-audit adjacent-wiki case).
    const RESEARCH_PATH_BOOST: f64 = 2.85;

    let research_query = query_looks_research_shaped(query);

    for (id, entry) in entries_ref {
        let mut mult = 1.0_f64;
        if entry.category.eq_ignore_ascii_case("decision")
            && entry.importance >= DECISION_IMPORTANCE_FLOOR
        {
            mult *= DECISION_BOOST;
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

fn is_research_wiki_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with("/wiki/")
        && (lower.contains("/research/")
            || lower.contains("/research-")
            || lower.ends_with("/research"))
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
