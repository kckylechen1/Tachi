// search.rs — Hybrid search orchestration
//
// Runs Vec + FTS + Symbolic channels, merges scores in Rust, returns top K.
// Optional graph expansion augments results with memory-graph neighbors.
// This is the hottest path: all computation stays in Rust, zero JS/Python overhead.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::{
    db::{
        fetch_by_ids, get_access_times, get_superseded_ids, graph_expand,
        record_access_with_updates, search_fts, search_symbolic_candidates, search_vec,
    },
    error::MemoryError,
    scorer::{
        cosine_similarity, hybrid_score, precision_query_multiplier, symbolic_score, tokenize,
        HybridWeights,
    },
    types::{MemoryEntry, SearchResult},
};

const EXPANDED_FTS_SCORE_FACTOR: f64 = 0.78;
const MAX_EXPANDED_FTS_QUERIES: usize = 6;
const MAX_SYMBOLIC_EXPANSION_TERMS: usize = 16;
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
}

fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("on")
    )
}

fn scoped_path_can_surface_superseded(path_prefix: Option<&str>) -> bool {
    let Some(prefix) = path_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
    else {
        return false;
    };
    if prefix == "/"
        || prefix == "/wiki"
        || prefix.starts_with("/wiki/")
        || prefix == "/kanban"
        || prefix.starts_with("/kanban/")
    {
        return false;
    }
    prefix.trim_matches('/').split('/').count() >= 3
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

fn parse_utc_timestamp(ts: &str) -> Option<DateTime<Utc>> {
    let raw = ts.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| raw.parse::<DateTime<Utc>>())
        .ok()
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
    let Some(as_of_dt) = parse_utc_timestamp(as_of) else {
        return false;
    };

    let starts_before_as_of = parse_utc_timestamp(valid_from)
        .map(|valid_from_dt| valid_from_dt <= as_of_dt)
        .unwrap_or_else(|| valid_from <= as_of);
    let ends_after_as_of = entry
        .valid_until
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|until| {
            parse_utc_timestamp(until)
                .map(|until_dt| until_dt > as_of_dt)
                .unwrap_or_else(|| until > as_of)
        })
        .unwrap_or(true);

    starts_before_as_of && ends_after_as_of
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

fn is_sft_training_entry(entry: &MemoryEntry) -> bool {
    metadata_bool(entry, "training_sample")
        || entry.path.starts_with("/sft/")
        || entry.topic.eq_ignore_ascii_case("sft-memory")
}

fn is_recall_cache_entry(entry: &MemoryEntry) -> bool {
    entry
        .source
        .eq_ignore_ascii_case("foundry_recall_rerank_cache")
        || entry
            .topic
            .eq_ignore_ascii_case("foundry_recall_rerank_cache")
        || entry.topic.eq_ignore_ascii_case("recall_rerank_cache")
        || entry.id.starts_with("foundry:recall-cache:")
        || entry.path.contains("/recall-cache/")
        || entry.path.contains("foundry_recall_rerank_cache")
        || metadata_bool(entry, "recall_rerank_cache")
        || entry
            .metadata
            .get("cache_key")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("foundry_recall_rerank_cache"))
}

fn is_openclaw_low_signal_entry(entry: &MemoryEntry) -> bool {
    entry.path == "/openclaw/legacy"
        || entry.path.contains("/unnamed")
        || entry.topic.trim().is_empty() && entry.path.starts_with("/openclaw/")
}

fn is_search_noise_entry(entry: &MemoryEntry, path_prefix: Option<&str>) -> bool {
    let kanban_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/kanban"));
    let handoff_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/handoff"));
    let wiki_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/wiki"));
    let recall_cache_scoped = path_prefix.is_some_and(|prefix| prefix.contains("/recall-cache"));
    (!wiki_scoped
        && (entry.path == "/wiki/_log"
            || metadata_bool(entry, "wiki_log")
            || entry.topic.eq_ignore_ascii_case("wiki_log")))
        || (!recall_cache_scoped && is_recall_cache_entry(entry))
        || (!kanban_scoped
            && (entry.path.starts_with("/kanban/")
                || entry.category.eq_ignore_ascii_case("kanban")))
        || (!handoff_scoped
            && (entry.path.starts_with("/handoff/")
                || entry.category.eq_ignore_ascii_case("handoff")))
}

fn quality_multiplier(entry: &MemoryEntry) -> f64 {
    let base = if is_sft_training_entry(entry) {
        0.45
    } else if is_openclaw_low_signal_entry(entry) {
        0.55
    } else if entry.is_foundry_distill() {
        0.75
    } else if entry.is_wiki() {
        1.15
    } else if entry.is_guide() {
        1.12
    } else if entry.is_kanban() || entry.is_handoff() {
        0.65
    } else {
        1.0
    };
    // High-importance entries get a floor of 1.0 so they aren't suppressed,
    // but foundry_distill and SFT training examples stay penalized regardless
    // of importance. Training examples are useful references when explicitly
    // scoped, but they should not crowd out distilled operational memory.
    if entry.importance >= 0.9
        && base < 1.0
        && !entry.is_foundry_distill()
        && !is_sft_training_entry(entry)
        && !is_openclaw_low_signal_entry(entry)
    {
        1.0
    } else {
        base
    }
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
    let fts_scores = search_fts_with_expansion(
        conn,
        query,
        n,
        opts.include_archived,
        include_superseded,
        opts.path_prefix.as_deref(),
        as_of_utc.as_deref(),
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

    // ── Collect all candidate IDs ──────────────────────────────────────────────
    let candidate_ids: Vec<String> = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .chain(symbolic_candidate_entries.iter().map(|entry| &entry.id))
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

    // Precision boosts for exact tickers and high-signal trading terms.
    // In non-RRF mode, cap the multiplier so it amplifies but doesn't overwhelm.
    for (id, entry) in &entries_ref {
        let mut multiplier = precision_query_multiplier(query, entry);
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

    fn insert_entry(conn: &mut Connection, entry: MemoryEntry) {
        upsert(conn, &entry, false).unwrap();
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
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn hybrid_symbolic_candidates_can_seed_path_scoped_short_technical_terms() {
        let mut conn = setup();
        let mut target = memory_entry(
            "clean-cli-memory",
            "The memory-server CLI clean bridge defaults to dry-run and requires --force for deletion.",
            &["clean-cli", "target-clean", "dry-run"],
        );
        target.path = "/scratch/tachi/clean-cli-integration".to_string();
        insert_entry(&mut conn, target);

        let mut other = memory_entry(
            "other-clean-memory",
            "Another cleanup note mentions dry-run but belongs elsewhere.",
            &["cleanup", "dry-run"],
        );
        other.path = "/scratch/other".to_string();
        insert_entry(&mut conn, other);

        let opts = SearchOptions {
            top_k: 3,
            candidates_per_channel: 0,
            path_prefix: Some("/scratch/tachi/clean-cli-integration".to_string()),
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "dry-run", &opts).unwrap();
        assert_eq!(results[0].entry.id, "clean-cli-memory");
        assert!(results[0].score.symbolic > 0.0);
    }

    #[test]
    fn hybrid_search_propagates_access_history_errors() {
        let mut conn = setup();
        insert(
            &mut conn,
            "access-history-error",
            "AccessHistoryError should not silently degrade search scoring",
            &["accesshistoryerror"],
        );
        conn.execute("DROP TABLE access_history", []).unwrap();

        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            ..Default::default()
        };
        let err = hybrid_search(&conn, "AccessHistoryError", &opts)
            .expect_err("access history query errors should propagate");
        let msg = err.to_string();
        assert!(
            msg.contains("access_history") || msg.contains("no such table"),
            "expected access_history error, got: {msg}"
        );
    }

    #[test]
    fn hybrid_symbolic_candidates_rank_exact_probe_token_above_siblings() {
        let mut conn = setup();
        insert(
            &mut conn,
            "alpha",
            "RECALL_PROBE_ALPHA_20260607 clean-cli bridge dry-run force-delete subcommands",
            &["recall-probe", "clean-cli", "dry-run"],
        );
        insert(
            &mut conn,
            "beta",
            "RECALL_PROBE_BETA_20260607 cleanup defaults preview before deletion",
            &["recall-probe", "cleanup"],
        );
        insert(
            &mut conn,
            "delta",
            "RECALL_PROBE_DELTA_20260607 profile routing requested_profile tool_profile",
            &["recall-probe", "profile"],
        );

        let opts = SearchOptions {
            top_k: 3,
            candidates_per_channel: 0,
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "RECALL_PROBE_ALPHA_20260607", &opts).unwrap();
        assert_eq!(results[0].entry.id, "alpha");
        assert!(results[0].score.symbolic > results[1].score.symbolic);
    }

    #[test]
    fn hybrid_search_returns_post_record_access_fields() {
        let mut conn = setup();
        let mut entry = memory_entry(
            "access-return",
            "AccessReturnProbe unique searchable memory",
            &["access-return"],
        );
        entry.access_count = 7;
        insert_entry(&mut conn, entry);

        let opts = SearchOptions {
            top_k: 1,
            candidates_per_channel: 0,
            record_access: true,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "AccessReturnProbe", &opts).unwrap();

        assert_eq!(results[0].entry.id, "access-return");
        assert_eq!(results[0].entry.access_count, 8);
        assert!(results[0].entry.last_access.is_some());
    }

    #[test]
    fn record_access_deduplicates_repeated_ids_before_incrementing() {
        let mut conn = setup();
        insert(
            &mut conn,
            "duplicate-access",
            "DuplicateAccessProbe unique searchable memory",
            &["duplicate-access"],
        );

        let ids = vec![
            "duplicate-access".to_string(),
            "duplicate-access".to_string(),
        ];
        let updates =
            record_access_with_updates(&conn, &ids, &ids, Some("DuplicateAccessProbe")).unwrap();

        assert_eq!(updates["duplicate-access"].access_count, 1);
        let (access_count, recall_count): (i64, i64) = conn
            .query_row(
                "SELECT access_count, recall_count FROM memories WHERE id = ?1",
                rusqlite::params!["duplicate-access"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(access_count, 1);
        assert_eq!(recall_count, 1);

        let history_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
                rusqlite::params!["duplicate-access"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_count, 1);
    }

    #[test]
    fn superseded_path_gate_matches_reserved_prefixes_exactly() {
        assert!(!scoped_path_can_surface_superseded(Some("/wiki")));
        assert!(!scoped_path_can_surface_superseded(Some(
            "/wiki/agent/tachi"
        )));
        assert!(!scoped_path_can_surface_superseded(Some("/kanban")));
        assert!(!scoped_path_can_surface_superseded(Some(
            "/kanban/active/task"
        )));

        assert!(scoped_path_can_surface_superseded(Some(
            "/wiki_rules/agent/tachi"
        )));
        assert!(scoped_path_can_surface_superseded(Some(
            "/kanbanboard/active/task"
        )));
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
    fn valid_at_compares_offset_timestamps_by_instant() {
        let mut entry = memory_entry("offset", "Offset timestamp memory", &[]);
        entry.valid_from = "2026-01-01T08:00:00+08:00".to_string();
        entry.valid_until = Some("2026-01-02T08:00:00+08:00".to_string());

        assert!(valid_at(&entry, Some("2026-01-01T00:00:00Z")));
        assert!(!valid_at(&entry, Some("2026-01-02T00:00:00Z")));
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
    fn hybrid_surfaces_superseded_when_explicitly_scoped_to_deep_path() {
        let mut conn = setup();
        let mut old = memory_entry(
            "old-path-memory",
            "clean-cli integration defaults to dry-run and requires --force",
            &["clean-cli", "dry-run"],
        );
        old.path = "/scratch/tachi/clean-cli-integration".to_string();
        insert_entry(&mut conn, old);
        let mut new = memory_entry(
            "new-release-memory",
            "release prep summary for tachi version bump",
            &["release-prep"],
        );
        new.path = "/scratch/tachi/v1.5-release-prep".to_string();
        insert_entry(&mut conn, new);
        crate::db::supersede_memory(&conn, "old-path-memory", "new-release-memory").unwrap();

        let opts = SearchOptions {
            top_k: 5,
            path_prefix: Some("/scratch/tachi/clean-cli-integration".to_string()),
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "dry-run", &opts).unwrap();
        assert_eq!(results[0].entry.id, "old-path-memory");
        assert!(results[0].score.final_score < 1.0);
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
    fn quality_multiplier_demotes_sft_training_samples() {
        let mut sample = memory_entry(
            "sft-sample",
            "DaemonAdapterTimeoutFix root cause and verified production fix",
            &["daemon", "timeout", "fix"],
        );
        sample.importance = 0.95;
        sample.path = "/sft/v4/strict/engineering/123".to_string();
        sample.topic = "sft-memory".to_string();
        sample.metadata = json!({"training_sample": true});
        assert_eq!(quality_multiplier(&sample), 0.45);

        let mut handoff = memory_entry(
            "handoff",
            "DaemonAdapterTimeoutFix operational handoff",
            &["daemon", "timeout", "fix"],
        );
        handoff.category = "handoff".to_string();
        handoff.importance = 0.95;
        assert_eq!(quality_multiplier(&handoff), 1.0);
    }

    #[test]
    fn quality_multiplier_demotes_openclaw_low_signal_entries() {
        let mut legacy = memory_entry(
            "openclaw-legacy",
            "Legacy migrated raw session note",
            &["openclaw", "legacy"],
        );
        legacy.importance = 0.95;
        legacy.path = "/openclaw/legacy".to_string();
        assert_eq!(quality_multiplier(&legacy), 0.55);

        let mut unnamed = memory_entry(
            "openclaw-unnamed",
            "Unnamed migrated memory should not dominate recall",
            &["openclaw", "unnamed"],
        );
        unnamed.importance = 0.95;
        unnamed.path = "/openclaw/agent-main/unnamed".to_string();
        assert_eq!(quality_multiplier(&unnamed), 0.55);
    }

    #[test]
    fn recall_cache_variants_are_search_noise_by_default() {
        let mut cache = memory_entry(
            "openclaw-recall-cache",
            "Recall rerank cache for query: Scout pipeline fixes",
            &["recall", "cache"],
        );
        cache.path = "/openclaw/agent-main/recall-cache/Scout_pipeline".to_string();
        cache.topic = "recall_rerank_cache".to_string();
        assert!(is_search_noise_entry(&cache, None));
        assert!(is_search_noise_entry(&cache, Some("/openclaw/agent-main")));
        assert!(!is_search_noise_entry(
            &cache,
            Some("/openclaw/agent-main/recall-cache")
        ));
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
