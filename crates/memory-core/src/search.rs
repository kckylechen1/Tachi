// search.rs — Hybrid search orchestration
//
// Runs Vec + FTS + Symbolic channels, merges scores in Rust, returns top K.
// Optional graph expansion augments results with memory-graph neighbors.
// This is the hottest path: all computation stays in Rust, zero JS/Python overhead.

use rusqlite::Connection;
use std::sync::Arc;

use crate::{
    db::{fetch_by_ids, record_access_with_updates},
    error::MemoryError,
    recall_config::RecallConfig,
    scorer::{HybridWeights, PrecisionMatcher},
    types::SearchResult,
};

mod candidates;
mod expansion;
mod filtering;
mod graph_expansion;
mod ranking;

#[cfg(test)]
use self::expansion::search_fts_with_expansion_config;
use self::filtering::{env_truthy, scoped_path_can_surface_superseded};
#[cfg(test)]
use self::filtering::{is_search_noise_entry, quality_multiplier, valid_at};

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

pub(super) fn recall_config(opts: &SearchOptions) -> &RecallConfig {
    opts.recall_config
        .as_ref()
        .unwrap_or_else(|| RecallConfig::get())
}

pub(super) fn resolve_weights(opts: &SearchOptions) -> HybridWeights {
    if opts.weights != HybridWeights::default() {
        return opts.weights.clone();
    }

    let path = opts.path_prefix.as_deref().unwrap_or("");
    recall_config(opts).weights_for_path(path)
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
    let as_of_utc = opts
        .as_of
        .as_deref()
        .map(crate::db::normalize_utc_iso)
        .transpose()?;
    let include_superseded = opts.include_superseded
        || env_truthy("TACHI_SEARCH_INCLUDE_SUPERSEDED")
        || scoped_path_can_surface_superseded(opts.path_prefix.as_deref());

    let candidates = candidates::collect_candidates(
        conn,
        query,
        opts,
        include_superseded,
        as_of_utc.as_deref(),
    )?;
    if candidates.candidate_ids.is_empty() {
        return Ok(vec![]);
    }

    // ── Bulk-fetch entries ─────────────────────────────────────────────────────
    let entries_map = fetch_by_ids(conn, &candidates.candidate_ids, opts.include_archived)?;
    let mut results = ranking::rank_candidate_entries(
        conn,
        query,
        opts,
        entries_map,
        &candidates.vec_scores,
        &candidates.fts_scores,
        candidates.exact_id.as_ref(),
        include_superseded,
        as_of_utc.as_deref(),
    )?;

    graph_expansion::append_graph_expansion(
        conn,
        &mut results,
        opts,
        include_superseded,
        as_of_utc.as_deref(),
    )?;

    // ── Record access (bump counters) ─────────────────────────────────────────
    if opts.record_access {
        let accessed_ids: Vec<String> = results.iter().map(|r| r.entry.id.clone()).collect();
        // FTS hits drive recall_count; collect before taking results slice
        let fts_hit_ids: Vec<String> = candidates.fts_scores.keys().cloned().collect();
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
