// search.rs — Hybrid search orchestration
//
// Runs Vec + FTS + Symbolic channels, merges scores in Rust, returns top K.
// Optional graph expansion augments results with memory-graph neighbors.
// This is the hottest path: all computation stays in Rust, zero JS/Python overhead.

use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::{
    db::{
        fetch_by_ids, get_access_times, get_superseded_ids, graph_expand,
        record_access_with_updates, search_symbolic_candidates, search_vec,
    },
    error::MemoryError,
    recall_config::RecallConfig,
    scorer::{
        cosine_similarity, is_id_like_exact_query, symbolic_score, HybridWeights, PrecisionMatcher,
    },
    types::{HybridScore, MemoryEntry, SearchResult},
};

mod expansion;
mod filtering;

use self::expansion::{search_fts_with_expansion_config, symbolic_query_with_expansion};
use self::filtering::{
    env_truthy, is_search_noise_entry, newest_by_shared_entity, normalized_seed_weights,
    quality_multiplier, scoped_path_can_surface_superseded, valid_at,
};

const SYMBOLIC_CANDIDATE_MULTIPLIER: usize = 10;

/// Options for a hybrid search query.
pub struct SearchOptions {
    /// Number of candidates to pull from each channel before merging.
    pub candidates_per_channel: usize,
    /// Final top-K to return after scoring.
    pub top_k: usize,
    /// Scoring weights.
    pub weights: HybridWeights,
    /// Optionally restrict results to a path prefix (e.g. "/openclaw")
    pub path_prefix: Option<String>,
    /// Optionally restrict results to a specific domain (e.g. "finance")
    pub domain: Option<String>,
    /// Pre-computed query embedding; if None, skip vector channel.
    pub query_vec: Option<Vec<f32>>,
    /// Whether the sqlite-vec extension is available for vector search.
    pub vec_available: bool,
    /// Whether to bump access_count after retrieval (disable in bulk/bench mode).
    pub record_access: bool,
    /// Whether to include archived entries in query results.
    pub include_archived: bool,
    /// Whether to include entries superseded by a newer memory.
    pub include_superseded: bool,
    /// MMR diversity threshold: cosine similarity > threshold → defer to end.
    /// Set to None to disable MMR. Default: Some(0.85).
    pub mmr_threshold: Option<f64>,
    /// Graph expand hops: 0 = disabled, 1-2 = expand through memory_edges after ranking.
    /// Expanded entries are appended after the ranked results (lower priority).
    pub graph_expand_hops: u32,
    /// Optional filter for graph edges: "causes", "follows", "related_to", etc.
    /// None = traverse all relation types.
    pub graph_relation_filter: Option<String>,
    /// Point-in-time validity filter. When set, only memories valid at this ISO
    /// timestamp are returned.
    pub as_of: Option<String>,
    /// Domain-specific precision boosters injected by the caller. The generic
    /// engine applies only `generic_precision_multiplier`; each matcher here can
    /// additionally multiply an entry's score when it judges the (query, entry)
    /// pair an exact match in its domain. Default: empty (fully generic).
    pub precision_matchers: Vec<Arc<dyn PrecisionMatcher>>,
    /// Optional per-call recall config override for simulation/eval. Production
    /// callers normally leave this unset so the process-wide config is used.
    pub recall_config: Option<RecallConfig>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            candidates_per_channel: 20,
            top_k: 6,
            weights: HybridWeights::default(),
            path_prefix: None,
            domain: None,
            query_vec: None,
            vec_available: false,
            record_access: true,
            include_archived: false,
            include_superseded: false,
            mmr_threshold: Some(0.85),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            as_of: None,
            precision_matchers: Vec::new(),
            recall_config: None,
        }
    }
}

fn recall_config(opts: &SearchOptions) -> &RecallConfig {
    opts.recall_config
        .as_ref()
        .unwrap_or_else(|| RecallConfig::get())
}

fn exact_memory_id_query(query: &str) -> Option<String> {
    let trimmed = query.trim().trim_matches(|c| matches!(c, '`' | '"' | '\''));
    uuid::Uuid::parse_str(trimmed)
        .ok()
        .map(|_| trimmed.to_string())
}

fn resolve_weights(opts: &SearchOptions) -> HybridWeights {
    if opts.weights != HybridWeights::default() {
        return opts.weights.clone();
    }

    let path = opts.path_prefix.as_deref().unwrap_or("");
    recall_config(opts).weights_for_path(path)
}

fn symbolic_match_text(entry: &MemoryEntry) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        entry.id, entry.path, entry.topic, entry.summary, entry.text
    )
}

/// MMR-inspired diversity filter: greedily select results that are both
/// relevant (high score) and diverse (low similarity to already-selected).
///
/// Candidates with cosine similarity > `threshold` to any already-selected
/// entry are deferred to the end rather than dropped entirely.
///
/// Ported from memory-lancedb-pro's `applyMMRDiversity()`.
fn apply_mmr_diversity(
    ranked: &[(&String, f64)],
    entries: &HashMap<String, MemoryEntry>,
    threshold: f64,
    needed: usize,
) -> Vec<String> {
    if ranked.len() <= 1 {
        return ranked.iter().map(|(id, _)| id.to_string()).collect();
    }

    let mut selected: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    for (idx, (id, _)) in ranked.iter().enumerate() {
        if selected.len() >= needed {
            deferred.extend(
                ranked[idx..]
                    .iter()
                    .map(|(rest_id, _)| (*rest_id).to_string()),
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
                _ => false, // can't compare without vectors
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

/// Execute a full hybrid search, returning ranked `SearchResult`s.
///
/// Execution plan:
///  1. Vector KNN (via sqlite-vec) — if `query_vec` provided
///  2. FTS5 BM25 — always
///  3. Symbolic bag-of-words — computed in Rust on the fetched entries
///  4. Hybrid score merge (weighted sum) with ACT-R decay
///  5. Sort, take top_k, record access
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
) -> Result<Vec<SearchResult>, MemoryError> {
    let n = opts.candidates_per_channel;
    let as_of_utc = opts
        .as_of
        .as_deref()
        .map(crate::db::normalize_utc_iso)
        .transpose()?;
    let include_superseded = opts.include_superseded
        || env_truthy("TACHI_SEARCH_INCLUDE_SUPERSEDED")
        || scoped_path_can_surface_superseded(opts.path_prefix.as_deref());

    // ── Channel 1: Vector ─────────────────────────────────────────────────────
    let vec_scores: HashMap<String, f64> = if opts.vec_available {
        if let Some(qv) = &opts.query_vec {
            search_vec(
                conn,
                qv,
                n,
                opts.include_archived,
                include_superseded,
                opts.path_prefix.as_deref(),
                as_of_utc.as_deref(),
            )?
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };

    // ── Channel 2: FTS5 ───────────────────────────────────────────────────────
    let fts_scores = search_fts_with_expansion_config(
        conn,
        query,
        n,
        opts.include_archived,
        include_superseded,
        opts.path_prefix.as_deref(),
        as_of_utc.as_deref(),
        recall_config(opts),
    )?;

    // ── Channel 3 seed: exact symbolic candidates ────────────────────────────
    // FTS5 can miss hyphenated slugs, exact ids, and short technical tokens
    // (`clean-cli`, `dry-run`, `RECALL_PROBE_*`). Pull a bounded lexical set so
    // symbolic scoring can add candidates instead of merely re-ranking FTS/vec.
    let symbolic_candidate_entries = search_symbolic_candidates(
        conn,
        query,
        n.saturating_mul(SYMBOLIC_CANDIDATE_MULTIPLIER)
            .max(opts.top_k),
        opts.include_archived,
        include_superseded,
        opts.path_prefix.as_deref(),
        as_of_utc.as_deref(),
    )?;
    let exact_id = exact_memory_id_query(query);

    // ── Collect all candidate IDs ──────────────────────────────────────────────
    let candidate_ids: Vec<String> = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .chain(symbolic_candidate_entries.iter().map(|entry| &entry.id))
        .chain(exact_id.as_ref())
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if candidate_ids.is_empty() {
        return Ok(vec![]);
    }

    // ── Bulk-fetch entries ─────────────────────────────────────────────────────
    let entries_map = fetch_by_ids(conn, &candidate_ids, opts.include_archived)?;

    // ── Channel 3: Symbolic ───────────────────────────────────────────────────
    let symbolic_query = symbolic_query_with_expansion(query);
    let symbolic_scores: HashMap<String, f64> = entries_map
        .iter()
        .map(|(id, entry)| {
            let score = symbolic_score(
                &symbolic_query,
                &symbolic_match_text(entry),
                &entry.keywords,
                &entry.entities,
            );
            (id.clone(), score)
        })
        .collect();

    let fetched_ids_vec: Vec<String> = entries_map.keys().cloned().collect();
    let superseded_ids = get_superseded_ids(conn, &fetched_ids_vec)?;

    // ── Optional path-prefix filter ───────────────────────────────────────────
    let entries_ref: HashMap<String, &MemoryEntry> = entries_map
        .iter()
        .filter(|(id, e)| {
            if !valid_at(e, as_of_utc.as_deref()) {
                return false;
            }
            if !include_superseded && superseded_ids.contains(*id) {
                return false;
            }
            if is_search_noise_entry(e, opts.path_prefix.as_deref()) {
                return false;
            }
            // Path prefix filter
            if let Some(prefix) = &opts.path_prefix {
                if !e.path.starts_with(prefix.as_str()) {
                    return false;
                }
            }
            // Domain filter
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

    // ── ACT-R access history (Spreading Activation: 越用越靠前) ──────────────
    let candidate_ids_vec: Vec<String> = entries_ref.keys().cloned().collect();
    let access_times = get_access_times(conn, &candidate_ids_vec)?;

    // ── Hybrid scoring with ACT-R enhancement ─────────────────────────────────
    let weights = resolve_weights(opts);
    let mut scores = crate::scorer::hybrid_score_with_config(
        &entries_ref,
        &vec_scores,
        &fts_scores,
        &symbolic_scores,
        &weights,
        &access_times,
        recall_config(opts),
    );
    if let Some(exact_id) = exact_id.as_ref().filter(|id| entries_ref.contains_key(*id)) {
        scores.insert(
            exact_id.clone(),
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

    // Precision boosts: the generic id-like exact-match boost plus any
    // caller-injected domain matchers (tickers, ICD codes, …). In non-RRF mode,
    // cap the multiplier so it amplifies but doesn't overwhelm.
    let is_id_like = is_id_like_exact_query(query);
    for (id, entry) in &entries_ref {
        let mut multiplier = crate::scorer::generic_precision_multiplier_impl_with_config(
            is_id_like,
            query,
            entry,
            recall_config(opts),
        );
        for matcher in &opts.precision_matchers {
            if let Some(boost) = matcher.boost(query, entry) {
                // Guard against a buggy/malicious matcher returning NaN, Inf, or
                // a value <= 1.0 corrupting or degrading the score.
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

    let top_pre_quality_score = scores
        .values()
        .map(|score| score.final_score)
        .filter(|score| score.is_finite())
        .fold(0.0_f64, f64::max);
    // Quality boosts are only applied to entries already scoring above the 85th
    // percentile floor. This prevents low-relevance wiki/guide entries from being
    // boosted into the top results purely on type. Penalties apply unconditionally.
    let quality_boost_floor = top_pre_quality_score * 0.85;
    for (id, entry) in &entries_ref {
        let multiplier = quality_multiplier(entry);
        if (multiplier - 1.0).abs() > f64::EPSILON {
            if let Some(score) = scores.get_mut(id) {
                if multiplier > 1.0 && score.final_score < quality_boost_floor {
                    continue;
                }
                score.final_score *= multiplier;
            }
        }
    }

    // Frequently recalled memories get a modest relevance boost (access feedback).
    for (id, entry) in &entries_ref {
        if entry.access_count >= 2 {
            let boost = 1.0 + (entry.access_count as f64).ln_1p() * 0.03;
            if let Some(score) = scores.get_mut(id) {
                score.final_score *= boost.min(1.25);
            }
        }
    }

    // ── Tier-based retrieval boosts ───────────────────────────────────────────
    for (id, entry) in &entries_ref {
        let tier_multiplier = match entry.tier.as_str() {
            "pattern" => 1.15,
            "consolidated" => 1.08,
            _ => 1.0, // raw
        };
        if tier_multiplier > 1.0 {
            if let Some(score) = scores.get_mut(id) {
                score.final_score *= tier_multiplier;
            }
        }
    }

    let newest_by_entity = newest_by_shared_entity(&entries_ref);
    for id in newest_by_entity {
        if !superseded_ids.contains(&id) {
            if let Some(score) = scores.get_mut(&id) {
                score.final_score *= 1.08;
            }
        }
    }
    // ── Sort and take top K ───────────────────────────────────────────────────
    let mut ranked: Vec<(&String, f64)> = scores
        .iter()
        .filter(|(id, _)| entries_ref.contains_key(*id))
        .map(|(id, hs)| (id, hs.final_score))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    // ── MMR diversity: defer near-duplicate entries to end ─────────────────────
    let ranked_ids: Vec<String> = if let Some(threshold) = opts.mmr_threshold {
        apply_mmr_diversity(&ranked, &entries_map, threshold, opts.top_k)
    } else {
        ranked.iter().map(|(id, _)| id.to_string()).collect()
    };
    drop(entries_ref);

    // ── Build output ──────────────────────────────────────────────────────────
    let mut entries_map = entries_map;
    let mut results: Vec<SearchResult> = ranked_ids
        .iter()
        .take(opts.top_k)
        .filter_map(|id| {
            let entry = entries_map.remove(id)?;
            let score = scores.get(id)?.clone();
            Some(SearchResult { entry, score })
        })
        .collect();

    // ── Graph expansion (post-search augmentation) ────────────────────────────
    // If graph_expand_hops > 0, BFS from result IDs to find related entries
    // and append them to results. This enriches search with causally/temporally
    // linked memories without making them score higher than direct matches.
    if opts.graph_expand_hops > 0 && !results.is_empty() {
        let seed_ids: Vec<String> = results.iter().map(|r| r.entry.id.clone()).collect();
        let rel_filter = opts.graph_relation_filter.as_deref();

        // Graph expansion is a best-effort enrichment; failures are non-fatal.
        if let Ok(expand_result) = graph_expand(conn, &seed_ids, opts.graph_expand_hops, rel_filter)
        {
            let existing_ids: std::collections::HashSet<String> =
                results.iter().map(|r| r.entry.id.clone()).collect();

            let min_score = results
                .last()
                .map(|r| r.score.final_score * 0.5)
                .unwrap_or(0.1);

            let seed_weights = normalized_seed_weights(&results);
            let activations = crate::scorer::graph_spreading_activation_with_seed_weights(
                &seed_weights,
                &expand_result.edges,
                opts.graph_expand_hops,
                0.5,
            );

            let expanded_entries: Vec<MemoryEntry> = expand_result
                .entries
                .into_iter()
                .filter(|entry| !existing_ids.contains(&entry.id))
                .filter(|entry| valid_at(entry, as_of_utc.as_deref()))
                .filter(|entry| !is_search_noise_entry(entry, opts.path_prefix.as_deref()))
                .collect();
            let expanded_ids: Vec<String> = expanded_entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect();
            let expanded_superseded_ids = if include_superseded {
                HashSet::new()
            } else {
                get_superseded_ids(conn, &expanded_ids)?
            };

            let mut new_entries: Vec<SearchResult> = expanded_entries
                .into_iter()
                .filter(|entry| {
                    if include_superseded {
                        return true;
                    }
                    !expanded_superseded_ids.contains(&entry.id)
                })
                .map(|entry| {
                    let distance = expand_result.distances.get(&entry.id).copied().unwrap_or(1);
                    let activation = activations.get(&entry.id).copied().unwrap_or(0.0);
                    let graph_boost = min_score
                        * (0.4 / (distance as f64 + 1.0) + 0.6 * activation).clamp(0.0, 1.0);
                    SearchResult {
                        entry,
                        score: crate::types::HybridScore {
                            vector: 0.0,
                            fts: 0.0,
                            symbolic: 0.0,
                            decay: 0.0,
                            final_score: graph_boost,
                        },
                    }
                })
                .collect();
            new_entries.sort_by(|a, b| {
                b.score
                    .final_score
                    .total_cmp(&a.score.final_score)
                    .then_with(|| a.entry.id.cmp(&b.entry.id))
            });

            results.extend(new_entries);
        }
    }

    // ── Record access (bump counters) ─────────────────────────────────────────
    if opts.record_access {
        let accessed_ids: Vec<String> = results.iter().map(|r| r.entry.id.clone()).collect();
        // FTS hits drive recall_count; collect before taking results slice
        let fts_hit_ids: Vec<String> = fts_scores.keys().cloned().collect();
        let access_updates =
            record_access_with_updates(conn, &accessed_ids, &fts_hit_ids, Some(query))?;
        for r in &mut results {
            if let Some(update) = access_updates.get(&r.entry.id) {
                r.entry.access_count = update.access_count;
                r.entry.last_access = update.last_access.clone();
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests;
