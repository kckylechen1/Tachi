// search.rs — Hybrid search orchestration
//
// Runs Vec + FTS + Symbolic channels, merges scores in Rust, returns top K.
// Optional graph expansion augments results with memory-graph neighbors.
// This is the hottest path: all computation stays in Rust, zero JS/Python overhead.

use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::{
    db::{
        fetch_by_ids, get_access_times, get_superseded_ids, graph_expand, record_access,
        search_fts, search_vec,
    },
    error::MemoryError,
    scorer::{cosine_similarity, hybrid_score, symbolic_score, tokenize, HybridWeights},
    types::{MemoryEntry, SearchResult},
};

const EXPANDED_FTS_SCORE_FACTOR: f64 = 0.78;
const MAX_EXPANDED_FTS_QUERIES: usize = 6;
const MAX_SYMBOLIC_EXPANSION_TERMS: usize = 16;

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
}

fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("on")
    )
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
        }
    }
}

fn valid_at(entry: &MemoryEntry, as_of: Option<&str>) -> bool {
    let Some(as_of) = as_of else {
        return true;
    };
    let valid_from = if entry.valid_from.trim().is_empty() {
        entry.timestamp.as_str()
    } else {
        entry.valid_from.as_str()
    };
    valid_from <= as_of
        && entry
            .valid_until
            .as_deref()
            .map_or(true, |until| until > as_of)
}

fn resolve_weights(opts: &SearchOptions) -> HybridWeights {
    if opts.weights != HybridWeights::default() {
        return opts.weights.clone();
    }

    let path = opts.path_prefix.as_deref().unwrap_or("");
    if path.starts_with("/guide") {
        HybridWeights {
            decay: 0.02,
            semantic: 0.25,
            fts: 0.45,
            symbolic: 0.28,
            use_rrf: true,
        }
    } else if path.starts_with("/wiki")
        || path.starts_with("/behavior")
        || path.starts_with("/rules")
    {
        HybridWeights {
            decay: 0.02,
            semantic: 0.48,
            fts: 0.30,
            symbolic: 0.20,
            use_rrf: true,
        }
    } else if path.starts_with("/events") || path.starts_with("/notes") {
        HybridWeights {
            decay: 0.25,
            semantic: 0.35,
            fts: 0.25,
            symbolic: 0.15,
            use_rrf: true,
        }
    } else {
        HybridWeights::default()
    }
}

fn push_unique(out: &mut Vec<String>, term: &str) {
    let term = term.trim().to_ascii_lowercase();
    if term.is_empty() || out.iter().any(|existing| existing == &term) {
        return;
    }
    out.push(term);
}

fn token_expansion_variants(token: &str) -> &'static [&'static [&'static str]] {
    match token {
        "mcp" => &[&["model", "context", "protocol"]],
        "llm" => &[&["language", "model"]],
        "rag" => &[&["retrieval", "augmented", "generation"]],
        "fts" => &[&["full", "text", "search"]],
        "rrf" => &[&["reciprocal", "rank", "fusion"]],
        "mmr" => &[&["maximal", "marginal", "relevance"], &["diversity"]],
        "ci" => &[&["workflow"], &["checks"], &["github", "actions"]],
        "pr" => &[&["pull", "request"]],
        "db" => &[&["database"], &["sqlite"]],
        "auth" => &[&["authentication"], &["authorization"]],
        "api" => &[&["endpoint"], &["interface"]],
        "cli" => &[&["command"], &["terminal"]],
        "repo" => &[&["repository"]],
        "vec" | "vector" => &[&["embedding"], &["semantic"]],
        "embedding" | "embeddings" => &[&["vector"], &["semantic"]],
        "semantic" => &[&["embedding"], &["vector"]],
        "recall" => &[&["retrieval"], &["search"]],
        "retrieval" => &[&["recall"], &["search"]],
        "bug" => &[&["error"], &["failure"], &["crash"]],
        "error" => &[&["bug"], &["failure"]],
        "failure" => &[&["error"], &["bug"]],
        "crash" => &[&["failure"], &["panic"]],
        "panic" => &[&["crash"], &["failure"]],
        _ => &[],
    }
}

fn phrase_expansion_variants(tokens: &[String]) -> Vec<String> {
    const PHRASES: &[(&[&str], &[&str])] = &[
        (&["model", "context", "protocol"], &["mcp"]),
        (&["language", "model"], &["llm"]),
        (&["retrieval", "augmented", "generation"], &["rag"]),
        (&["full", "text", "search"], &["fts"]),
        (&["reciprocal", "rank", "fusion"], &["rrf"]),
        (&["pull", "request"], &["pr"]),
        (&["github", "actions"], &["ci"]),
    ];

    let mut variants = Vec::new();
    for (phrase, replacement) in PHRASES {
        if phrase.len() > tokens.len() {
            continue;
        }
        for start in 0..=tokens.len() - phrase.len() {
            if phrase
                .iter()
                .enumerate()
                .all(|(idx, part)| tokens[start + idx] == *part)
            {
                let mut expanded =
                    Vec::with_capacity(tokens.len() - phrase.len() + replacement.len());
                expanded.extend(tokens[..start].iter().cloned());
                expanded.extend(replacement.iter().map(|part| (*part).to_string()));
                expanded.extend(tokens[start + phrase.len()..].iter().cloned());
                variants.push(expanded.join(" "));
            }
        }
    }
    variants
}

fn expanded_fts_queries(query: &str) -> Vec<String> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut queries = Vec::new();
    push_unique(&mut queries, query.trim());
    for (idx, token) in tokens.iter().enumerate() {
        for replacement in token_expansion_variants(token) {
            let mut expanded = Vec::with_capacity(tokens.len() + replacement.len());
            expanded.extend(tokens[..idx].iter().cloned());
            expanded.extend(replacement.iter().map(|part| (*part).to_string()));
            expanded.extend(tokens[idx + 1..].iter().cloned());
            push_unique(&mut queries, &expanded.join(" "));
            if queries.len() >= MAX_EXPANDED_FTS_QUERIES {
                return queries;
            }
        }
    }

    for variant in phrase_expansion_variants(&tokens) {
        push_unique(&mut queries, &variant);
        if queries.len() >= MAX_EXPANDED_FTS_QUERIES {
            break;
        }
    }

    queries
}

fn symbolic_query_with_expansion(query: &str) -> String {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return query.to_string();
    }

    let mut terms = tokens.clone();
    for token in &tokens {
        for replacement in token_expansion_variants(token) {
            for part in *replacement {
                push_unique(&mut terms, part);
                if terms.len() >= tokens.len() + MAX_SYMBOLIC_EXPANSION_TERMS {
                    return terms.join(" ");
                }
            }
        }
    }
    for variant in phrase_expansion_variants(&tokens) {
        for part in tokenize(&variant) {
            push_unique(&mut terms, &part);
            if terms.len() >= tokens.len() + MAX_SYMBOLIC_EXPANSION_TERMS {
                return terms.join(" ");
            }
        }
    }
    terms.join(" ")
}

fn search_fts_with_expansion(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let mut merged = HashMap::new();
    for (idx, fts_query) in expanded_fts_queries(query).into_iter().enumerate() {
        let factor = if idx == 0 {
            1.0
        } else {
            EXPANDED_FTS_SCORE_FACTOR
        };
        for (id, score) in search_fts(
            conn,
            &fts_query,
            limit,
            include_archived,
            include_superseded,
            path_prefix,
            as_of,
        )? {
            let adjusted = score * factor;
            merged
                .entry(id)
                .and_modify(|existing: &mut f64| *existing = existing.max(adjusted))
                .or_insert(adjusted);
        }
    }
    Ok(merged)
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

fn newest_by_shared_entity(entries: &HashMap<String, &MemoryEntry>) -> HashSet<String> {
    let mut by_entity: HashMap<&str, Vec<&MemoryEntry>> = HashMap::new();
    for entry in entries.values() {
        for entity in &entry.entities {
            let entity = entity.trim();
            if !entity.is_empty() {
                by_entity.entry(entity).or_default().push(*entry);
            }
        }
    }

    by_entity
        .into_values()
        .filter(|items| items.len() > 1)
        .filter_map(|items| {
            items
                .into_iter()
                .max_by(|a, b| a.timestamp.cmp(&b.timestamp))
                .map(|entry| entry.id.clone())
        })
        .collect()
}

fn metadata_bool(entry: &MemoryEntry, key: &str) -> bool {
    entry
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn is_search_noise_entry(entry: &MemoryEntry, path_prefix: Option<&str>) -> bool {
    let kanban_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/kanban"));
    let handoff_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/handoff"));
    let wiki_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/wiki"));
    (!wiki_scoped
        && (entry.path == "/wiki/_log"
            || metadata_bool(entry, "wiki_log")
            || entry.topic.eq_ignore_ascii_case("wiki_log")))
        || entry
            .source
            .eq_ignore_ascii_case("foundry_recall_rerank_cache")
        || (!kanban_scoped
            && (entry.path.starts_with("/kanban/")
                || entry.category.eq_ignore_ascii_case("kanban")))
        || (!handoff_scoped
            && (entry.path.starts_with("/handoff/")
                || entry.category.eq_ignore_ascii_case("handoff")))
}

fn quality_multiplier(entry: &MemoryEntry) -> f64 {
    if entry.source.eq_ignore_ascii_case("foundry_distill") {
        return 0.75;
    }
    if metadata_bool(entry, "wiki")
        || entry.domain.as_deref() == Some("wiki")
        || entry.category.eq_ignore_ascii_case("wiki")
    {
        return 1.15;
    }
    if entry.is_guide() {
        return 1.12;
    }
    if matches!(entry.category.as_str(), "kanban" | "handoff")
        || entry.path.starts_with("/kanban/")
        || entry.path.starts_with("/handoff/")
    {
        return 0.65;
    }
    1.0
}

fn normalized_seed_weights(results: &[SearchResult]) -> HashMap<String, f64> {
    let max_score = results
        .iter()
        .map(|result| result.score.final_score)
        .filter(|score| score.is_finite() && *score > 0.0)
        .fold(0.0_f64, f64::max);

    results
        .iter()
        .map(|result| {
            let weight = if max_score > 0.0 {
                result.score.final_score / max_score
            } else {
                1.0
            };
            (result.entry.id.clone(), weight.clamp(0.05, 1.0))
        })
        .collect()
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
    let include_superseded =
        opts.include_superseded || env_truthy("TACHI_SEARCH_INCLUDE_SUPERSEDED");

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
    let fts_scores = search_fts_with_expansion(
        conn,
        query,
        n,
        opts.include_archived,
        include_superseded,
        opts.path_prefix.as_deref(),
        as_of_utc.as_deref(),
    )?;

    // ── Collect all candidate IDs ──────────────────────────────────────────────
    let candidate_ids: Vec<String> = vec_scores
        .keys()
        .chain(fts_scores.keys())
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
            let score = symbolic_score(&symbolic_query, &entry.text, &entry.keywords);
            (id.clone(), score)
        })
        .collect();

    let fetched_ids_vec: Vec<String> = entries_map.keys().cloned().collect();
    let superseded_ids = get_superseded_ids(conn, &fetched_ids_vec).unwrap_or_default();

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
    let access_times = get_access_times(conn, &candidate_ids_vec).unwrap_or_default();

    // ── Hybrid scoring with ACT-R enhancement ─────────────────────────────────
    let weights = resolve_weights(opts);
    let mut scores = hybrid_score(
        &entries_ref,
        &vec_scores,
        &fts_scores,
        &symbolic_scores,
        &weights,
        &access_times,
    );

    if include_superseded {
        for id in &superseded_ids {
            if let Some(score) = scores.get_mut(id) {
                score.final_score *= 0.3;
            }
        }
    }

    let top_pre_quality_score = scores
        .values()
        .map(|score| score.final_score)
        .filter(|score| score.is_finite())
        .fold(0.0_f64, f64::max);
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
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // ── MMR diversity: defer near-duplicate entries to end ─────────────────────
    let ranked_ids: Vec<String> = if let Some(threshold) = opts.mmr_threshold {
        apply_mmr_diversity(&ranked, &entries_map, threshold, opts.top_k)
    } else {
        ranked.iter().map(|(id, _)| id.to_string()).collect()
    };

    // ── Build output ──────────────────────────────────────────────────────────
    let mut results: Vec<SearchResult> = ranked_ids
        .iter()
        .take(opts.top_k)
        .filter_map(|id| {
            let entry = entries_map.get(id)?.clone();
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

            let mut new_entries: Vec<SearchResult> = expand_result
                .entries
                .into_iter()
                .filter(|entry| !existing_ids.contains(&entry.id))
                .filter(|entry| valid_at(entry, as_of_utc.as_deref()))
                .filter(|entry| !is_search_noise_entry(entry, opts.path_prefix.as_deref()))
                .filter(|entry| {
                    if include_superseded {
                        return true;
                    }
                    !get_superseded_ids(conn, std::slice::from_ref(&entry.id))
                        .map(|ids| ids.contains(&entry.id))
                        .unwrap_or(false)
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
                    .partial_cmp(&a.score.final_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.entry.id.cmp(&b.entry.id))
            });

            results.extend(new_entries);
        }
    }

    // ── Record access (bump counters) ─────────────────────────────────────────
    if opts.record_access {
        let accessed_ids: Vec<String> = results.iter().map(|r| r.entry.id.clone()).collect();
        record_access(conn, &accessed_ids)?;
        for r in &mut results {
            r.entry.access_count += 1;
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{add_edge, init_schema, register_sqlite_vec, try_load_sqlite_vec, upsert};
    use crate::types::{MemoryEdge, MemoryEntry};
    use chrono::Utc;
    use rusqlite::Connection;
    use serde_json::json;

    fn setup() -> Connection {
        libsimple::enable_auto_extension().unwrap();
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        try_load_sqlite_vec(&conn);
        conn
    }

    fn insert(conn: &mut Connection, id: &str, text: &str, keywords: &[&str]) {
        let e = memory_entry(id, text, keywords);
        upsert(conn, &e, false).unwrap();
    }

    fn memory_entry(id: &str, text: &str, keywords: &[&str]) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test".into(),
            summary: text.chars().take(30).collect(),
            text: text.into(),
            importance: 0.7,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "".into(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({ "keywords": keywords, "entities": [] }),
            vector: None,
            retention_policy: None,
            domain: None,
        }
    }

    #[test]
    fn hybrid_returns_relevant() {
        let mut conn = setup();
        insert(
            &mut conn,
            "a",
            "Rust is fast and memory safe",
            &["rust", "performance"],
        );
        insert(
            &mut conn,
            "b",
            "Python is great for scripting",
            &["python", "scripting"],
        );

        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "rust performance", &opts).unwrap();
        assert!(!results.is_empty());
        // "a" should score higher for "rust performance" query
        assert_eq!(results[0].entry.id, "a");
    }

    #[test]
    fn hybrid_uses_fts_when_vectors_are_available_but_query_vec_missing() {
        let mut conn = setup();
        insert(
            &mut conn,
            "a",
            "Voyage outage should still allow lexical fallback search",
            &["voyage", "fallback"],
        );
        insert(&mut conn, "b", "Unrelated operational note", &["ops"]);

        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            vec_available: true,
            query_vec: None,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "voyage fallback", &opts).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].entry.id, "a");
        assert!(results[0].score.fts > 0.0);
    }

    #[test]
    fn hybrid_expands_acronym_queries_for_fts() {
        let mut conn = setup();
        insert(
            &mut conn,
            "expanded",
            "Model Context Protocol handshake serialization checklist",
            &["protocol"],
        );

        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "mcp handshake", &opts).unwrap();
        assert!(results.iter().any(|result| result.entry.id == "expanded"));
    }

    #[test]
    fn hybrid_expands_phrase_queries_for_fts() {
        let mut conn = setup();
        insert(
            &mut conn,
            "acronym",
            "MCP handshake serialization checklist",
            &["mcp"],
        );

        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "model context protocol handshake", &opts).unwrap();
        assert!(results.iter().any(|result| result.entry.id == "acronym"));
    }

    #[test]
    fn hybrid_keeps_exact_fts_match_above_expanded_match() {
        let mut conn = setup();
        insert(
            &mut conn,
            "exact",
            "MCP handshake serialization checklist",
            &["mcp"],
        );
        insert(
            &mut conn,
            "expanded",
            "Model Context Protocol handshake serialization checklist",
            &["protocol"],
        );

        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "mcp handshake", &opts).unwrap();
        assert_eq!(results[0].entry.id, "exact");
    }

    #[test]
    fn empty_query_returns_empty() {
        let mut conn = setup();
        insert(&mut conn, "x", "some text", &[]);
        let opts = SearchOptions {
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "", &opts).unwrap();
        // FTS5 with empty query should produce no FTS results; vec channel also empty
        assert!(results.is_empty());
    }

    #[test]
    fn guide_path_uses_operational_weights() {
        let opts = SearchOptions {
            path_prefix: Some("/guide/fix_pattern".to_string()),
            ..Default::default()
        };
        let weights = resolve_weights(&opts);
        assert_eq!(weights.fts, 0.45);
        assert_eq!(weights.symbolic, 0.28);
        assert_eq!(weights.decay, 0.02);
        assert!(weights.use_rrf);
    }

    #[test]
    fn hybrid_hides_superseded_by_default() {
        let mut conn = setup();
        insert(
            &mut conn,
            "old",
            "TrendLock protects trends using the stale rule",
            &["trendlock"],
        );
        insert(
            &mut conn,
            "new",
            "TrendLock protects trends using the canonical rule",
            &["trendlock"],
        );
        crate::db::supersede_memory(&conn, "old", "new").unwrap();

        let opts = SearchOptions {
            top_k: 5,
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
        let ids = results
            .into_iter()
            .map(|result| result.entry.id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&"new".to_string()));
        assert!(!ids.contains(&"old".to_string()));
    }

    #[test]
    fn hybrid_search_respects_as_of_validity_window() {
        let mut conn = setup();
        let mut old = memory_entry("temporal-old", "TemporalHybridNeedle old memory", &[]);
        old.valid_from = "2026-01-01T00:00:00Z".to_string();
        old.valid_until = Some("2026-02-01T00:00:00Z".to_string());
        upsert(&mut conn, &old, false).unwrap();

        let mut new = memory_entry("temporal-new", "TemporalHybridNeedle new memory", &[]);
        new.valid_from = "2026-02-01T00:00:00Z".to_string();
        upsert(&mut conn, &new, false).unwrap();

        let january = hybrid_search(
            &conn,
            "TemporalHybridNeedle",
            &SearchOptions {
                top_k: 5,
                record_access: false,
                as_of: Some("2026-01-15T00:00:00.000Z".to_string()),
                ..Default::default()
            },
        )
        .unwrap()
        .into_iter()
        .map(|result| result.entry.id)
        .collect::<Vec<_>>();
        assert!(january.contains(&"temporal-old".to_string()));
        assert!(!january.contains(&"temporal-new".to_string()));

        let march = hybrid_search(
            &conn,
            "TemporalHybridNeedle",
            &SearchOptions {
                top_k: 5,
                record_access: false,
                as_of: Some("2026-03-01T00:00:00.000Z".to_string()),
                ..Default::default()
            },
        )
        .unwrap()
        .into_iter()
        .map(|result| result.entry.id)
        .collect::<Vec<_>>();
        assert!(!march.contains(&"temporal-old".to_string()));
        assert!(march.contains(&"temporal-new".to_string()));
    }

    #[test]
    fn hybrid_hides_operation_logs() {
        let mut conn = setup();
        insert(
            &mut conn,
            "knowledge",
            "TrendLock durable decision rule for agents",
            &["trendlock"],
        );
        let mut log = memory_entry(
            "wiki-operation-log",
            "TrendLock write operation log should not be recalled",
            &["trendlock", "log"],
        );
        log.path = "/wiki/_log".to_string();
        log.topic = "wiki_log".to_string();
        log.metadata = json!({"wiki_log": true});
        upsert(&mut conn, &log, false).unwrap();

        let opts = SearchOptions {
            top_k: 5,
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
        assert!(results.iter().any(|result| result.entry.id == "knowledge"));
        assert!(!results
            .iter()
            .any(|result| result.entry.id == "wiki-operation-log"));
    }

    #[test]
    fn graph_expansion_orders_neighbors_by_spreading_activation() {
        let mut conn = setup();
        insert(
            &mut conn,
            "seed",
            "TrendLock durable decision rule",
            &["trendlock"],
        );
        insert(
            &mut conn,
            "support",
            "Support note only reachable by graph",
            &["support"],
        );
        insert(
            &mut conn,
            "related",
            "Related note only reachable by graph",
            &["related"],
        );
        add_edge(
            &conn,
            &MemoryEdge {
                source_id: "seed".to_string(),
                target_id: "related".to_string(),
                relation: "related_to".to_string(),
                weight: 1.0,
                metadata: json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        )
        .unwrap();
        add_edge(
            &conn,
            &MemoryEdge {
                source_id: "seed".to_string(),
                target_id: "support".to_string(),
                relation: "supports".to_string(),
                weight: 1.0,
                metadata: json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        )
        .unwrap();

        let opts = SearchOptions {
            top_k: 1,
            record_access: false,
            graph_expand_hops: 1,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
        let ids = results
            .iter()
            .map(|result| result.entry.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["seed", "support", "related"]);
        assert!(results[1].score.final_score > results[2].score.final_score);
    }
}
