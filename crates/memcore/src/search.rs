// search.rs — Hybrid search orchestration
//
// Runs Vec + FTS + Symbolic channels, merges scores in Rust, returns top K.
// Optional graph expansion augments results with memory-graph neighbors.
// This is the hottest path: all computation stays in Rust, zero JS/Python overhead.

use rusqlite::Connection;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{
    db::{fetch_by_ids, record_access_with_updates},
    error::MemoryError,
    recall_config::RecallConfig,
    scorer::{DecayPolicy, HybridWeights, PrecisionMatcher, DEFAULT_DECAY_POLICY},
    types::SearchResult,
};

mod candidates;
mod expansion;
mod filtering;
mod graph_expansion;
mod ranking;
mod rerank_blend;

#[cfg(test)]
use self::expansion::search_fts_with_expansion_config;
use self::filtering::{env_truthy, scoped_path_can_surface_superseded};
#[cfg(test)]
use self::filtering::{is_search_noise_entry, quality_multiplier, valid_at};
pub use rerank_blend::{
    apply_blend_relevance, merge_rerank_order_with_hybrid_floor, HYBRID_HEAD_FRACTION,
};

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
    /// Optionally restrict results to a specific domain (e.g. "domain-pack")
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
    /// Library-specific decay policy injected by the caller (MemCore portable
    /// hook). When `None`, the engine uses [`DEFAULT_DECAY_POLICY`]. Downstream
    /// products (HyperMemory trading half-lives, chat affect decay) supply an
    /// `Arc<dyn DecayPolicy>` without forking the kernel scorer.
    pub decay_policy: Option<Arc<dyn DecayPolicy>>,
    /// tachi#1097 PERF-T3 S1: per-call opt-in for phase-attribution
    /// instrumentation. When `false` (the default), `hybrid_search` performs
    /// **zero** `Instant::now()` reads and constructs **no** receipt — this is
    /// the contract's default-off / zero-overhead posture for production.
    /// When `true`, `hybrid_search_with_receipt` returns a populated
    /// [`SearchPhaseReceipt`] breaking down candidate collection / fetch /
    /// rank / graph expansion / access recording; `hybrid_search` itself
    /// ignores the receipt and stays zero-overhead regardless.
    ///
    /// This is a sampling switch only — it never changes ranking, expansion,
    /// access-recording, or any other quality mechanism. Production search
    /// call sites stay out of scope for this leaf; measurement happens in
    /// fixtures (contract `Allowed Scope`).
    pub collect_phase_receipt: bool,
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
            decay_policy: None,
            collect_phase_receipt: false,
        }
    }
}

pub(super) fn recall_config(opts: &SearchOptions) -> &RecallConfig {
    opts.recall_config
        .as_ref()
        .unwrap_or_else(|| RecallConfig::get())
}

/// Resolve the decay policy for this search call (caller inject or default).
pub(super) fn decay_policy(opts: &SearchOptions) -> &dyn DecayPolicy {
    opts.decay_policy
        .as_ref()
        .map(|p| p.as_ref() as &dyn DecayPolicy)
        .unwrap_or(&DEFAULT_DECAY_POLICY)
}

pub(super) fn resolve_weights(opts: &SearchOptions) -> HybridWeights {
    if opts.weights != HybridWeights::default() {
        return opts.weights.clone();
    }

    let path = opts.path_prefix.as_deref().unwrap_or("");
    recall_config(opts).weights_for_path(path)
}

// ---------------------------------------------------------------------------
// tachi#1097 PERF-T3 S1 — Phase-attribution receipts.
//
// Pure observation: this surface adds *no* new scoring, expansion, access, or
// quality logic. Its sole job is to break the wall time of one
// `hybrid_search_with_receipt` call down by phase so a benchmark/eval fixture
// can attribute slow runs to candidate retrieval, ranking DB I/O, graph
// expansion, or the access-recording write — and pair that with the recall
// quality result for the same call.
//
// Honesty rules (issue #1097 D5 / D8, mirroring `db::open::retry_memory_locked`'s
// "we do not report SQLite's own busy_timeout because it is opaque to rusqlite"):
//
//   * Only the whitelisted counters below are ever populated. Raw query text,
//     memory content/summaries, DB file paths, credentials, embedding vectors,
//     and entity names are NEVER placed in the receipt or any log.
//   * `pool_wait` is [`LayerAvailability::Unavailable`] — `hybrid_search` takes
//     a bare `&Connection`, so the read-pool checkout receipt (whose type
//     lives in `memory-server-runtime::ReadPoolCheckoutReceipt`) is owned by
//     a higher layer whose production path (`with_store`) currently discards
//     it. We refuse to fake a zero (#1097 D1).
//   * `sqlite_retry` is [`LayerAvailability::NotApplicable`] —
//     `retry_memory_locked` is wired only into write paths
//     (db::memory_crud::{crud, derived, enrichment, open}); the read path the
//     receipt measures never enters it, so any value here would always be a
//     literal zero. We mark it not-applicable instead of pretending to have
//     measured one (#1097 D1, D8).
//   * No trace/operation/request id is minted — the whole codebase has zero
//     precedent for them, and a benchmark/fixture pairs the receipt with its
//     quality result in the same call (#1097 D2).
// ---------------------------------------------------------------------------

/// Honest classification for a layer this receipt cannot measure. Mirrors the
/// "we don't report it rather than mislabel it" style of `db::open` (see the
/// `retry_memory_locked` doc on SQLite's own `busy_timeout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayerAvailability {
    /// Default. The owning receipt field was not populated (e.g. sampling was
    /// off, so no layer classification happened at all).
    #[default]
    NotSampled,
    /// The layer exists in the codebase but is not reachable from the call
    /// path that produced this receipt — measuring it would have to happen in
    /// a different layer (e.g. pool checkout happens above `hybrid_search`).
    Unavailable,
    /// The layer does not apply to this call path at all (e.g. the SQLite
    /// BUSY/LOCKED retry loop is wired only into write paths; recall is
    /// read-only, so the retry counter would always read zero here).
    NotApplicable,
}

/// One retrieval channel's wall-time + count snapshot.
#[derive(Debug, Clone)]
pub struct ChannelPhaseReceipt {
    pub elapsed: Duration,
    pub candidate_count: usize,
}

/// Per-FTS-query-group timing. `idx == 0` is the original query, `idx > 0` are
/// expanded variants (expansion.rs:241-265); the OR-fallback group is marked
/// `is_fallback = true` and runs only when `merged.is_empty()` (expansion.rs:266).
#[derive(Debug, Clone)]
pub struct FtsExpansionGroupReceipt {
    pub idx: usize,
    pub is_fallback: bool,
    pub elapsed: Duration,
    pub hit_count: usize,
}

/// Candidate-collection phase receipt (vector KNN + FTS-with-expansion + symbolic).
#[derive(Debug, Clone)]
pub struct CandidatePhaseReceipt {
    pub total_elapsed: Duration,
    pub vec_available: bool,
    /// `None` (rather than `Some({0 elapsed, 0 count})`) when the vector
    /// channel was structurally off — `vec_available == false` OR
    /// `query_vec.is_none()`. The branch is candidates.rs:30-31. Honest
    /// "channel did not run" rather than "channel ran and matched zero".
    pub vec: Option<ChannelPhaseReceipt>,
    /// One entry per executed FTS group, in execution order. Empty when the
    /// FTS subcall ran zero iterations (e.g. empty query).
    pub fts_groups: Vec<FtsExpansionGroupReceipt>,
    pub fts_candidate_count: usize,
    pub symbolic: ChannelPhaseReceipt,
    /// Deduplicated union of vec + fts + symbolic + exact-id across all
    /// channels — the size of the candidate set that flows into fetch/rank.
    pub merged_candidate_count: usize,
}

/// Bulk-fetch-by-id phase receipt (`db::fetch_by_ids`).
#[derive(Debug, Clone)]
pub struct FetchPhaseReceipt {
    pub elapsed: Duration,
    pub fetched_count: usize,
}

/// Rank + MMR phase receipt. Per #1097 D3 the two DB I/Os are timed
/// individually so a "rank is slow" report can distinguish DB reads from
/// scoring math; MMR itself is NOT separately timed because splitting it out
/// would mean restructuring `rank_candidate_entries` — a quality change, out
/// of scope for an instrumentation leaf.
#[derive(Debug, Clone)]
pub struct RankPhaseReceipt {
    pub total_elapsed: Duration,
    /// `get_superseded_ids` DB I/O (ranking.rs:47).
    pub get_superseded_ids: ChannelPhaseReceipt,
    /// `get_access_times` DB I/O (ranking.rs:82).
    pub get_access_times: ChannelPhaseReceipt,
    /// `opts.mmr_threshold.is_some()`. The MMR diversity post-filter ran iff
    /// this is `true`; otherwise the ranker returned its plain score-sorted
    /// order. The on/off state — not a separate timer — is the discrimination
    /// signal a benchmark uses to compare the same query both ways.
    pub mmr_enabled: bool,
    pub ranked_result_count: usize,
}

/// Graph-expansion phase receipt. Always `Some` when the receipt was sampled,
/// because `append_graph_expansion` is always invoked; the inner `enabled`
/// flag distinguishes "graph disabled" (`graph_expand_hops == 0`,
/// graph_expansion.rs:24-26 early return) from "graph ran but expanded
/// nothing".
#[derive(Debug, Clone)]
pub struct GraphPhaseReceipt {
    pub enabled: bool,
    pub elapsed: Duration,
    pub expanded_count: usize,
}

/// Access-recording write transaction receipt. Populated iff
/// `opts.record_access == true` (the entire `if opts.record_access { ... }`
/// block at search.rs:189 is skipped otherwise).
#[derive(Debug, Clone)]
pub struct AccessRecordingPhaseReceipt {
    pub elapsed: Duration,
    /// Free read of an existing return value: this is
    /// `record_access_with_updates(...).len()` — the number of existing
    /// memory rows whose `access_count`/`recall_count`/`query_diversity`
    /// were bumped (access.rs:73 returns the map; we read `.len()` on it).
    /// **No new counter logic is added on the access-recording boundary.**
    pub updated_row_count: usize,
}

/// Per-phase attribution for one `hybrid_search_with_receipt` call. See the
/// module-level honesty rules above for what is and is not populated, and
/// what is intentionally marked unavailable / not-applicable.
#[derive(Debug, Clone)]
pub struct SearchPhaseReceipt {
    /// `true` iff the caller opted in via `SearchOptions::collect_phase_receipt`.
    /// When `false`, every field below is `None` / `Duration::ZERO` and the
    /// two layer tags are `LayerAvailability::NotSampled` — this is the
    /// default-off / zero-overhead production posture (#1097 D4).
    pub sampled: bool,
    pub total_elapsed: Duration,
    /// Always `Some` when `sampled` (candidate collection always runs).
    pub candidates: Option<CandidatePhaseReceipt>,
    /// `None` only on the empty-candidate early return (search.rs:160-162).
    pub fetch: Option<FetchPhaseReceipt>,
    /// `None` when ranking never produced a result set (empty candidate set
    /// or every candidate filtered out before scoring, ranking.rs:77-79).
    pub rank: Option<RankPhaseReceipt>,
    /// Always `Some` when `sampled` (function is always invoked; inner
    /// `enabled` flag carries the disabled-vs-empty distinction).
    pub graph_expansion: Option<GraphPhaseReceipt>,
    /// `None` iff `record_access == false`.
    pub access_recording: Option<AccessRecordingPhaseReceipt>,
    /// See [`LayerAvailability::Unavailable`] doc.
    pub pool_wait: LayerAvailability,
    /// See [`LayerAvailability::NotApplicable`] doc.
    pub sqlite_retry: LayerAvailability,
}

impl SearchPhaseReceipt {
    /// Construct the not-sampled placeholder. Every phase field is `None`,
    /// every elapsed is `Duration::ZERO`, and the layer tags are `NotSampled`.
    fn not_sampled() -> Self {
        Self {
            sampled: false,
            total_elapsed: Duration::ZERO,
            candidates: None,
            fetch: None,
            rank: None,
            graph_expansion: None,
            access_recording: None,
            pool_wait: LayerAvailability::NotSampled,
            sqlite_retry: LayerAvailability::NotSampled,
        }
    }
}

/// Execute a full hybrid search, returning ranked `SearchResult`s.
///
/// Execution plan:
///  1. Vector KNN (via sqlite-vec) — if `query_vec` provided
///  2. FTS5 BM25 — always
///  3. Symbolic bag-of-words — computed in Rust on the fetched entries
///  4. Hybrid score merge (weighted sum) with ACT-R decay
///  5. Sort, take top_k, record access
///
/// This is the production entry point. It is **zero-overhead** with respect to
/// `SearchOptions::collect_phase_receipt`: the phase receipt is collected by
/// `hybrid_search_with_receipt` and discarded here, so a production caller
/// that never sets `collect_phase_receipt` pays nothing. See
/// `hybrid_search_with_receipt` for the receipt contract (#1097).
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
) -> Result<Vec<SearchResult>, MemoryError> {
    hybrid_search_with_receipt(conn, query, opts).map(|(results, _)| results)
}

/// Instrumented twin of [`hybrid_search`]: returns the ranked `SearchResult`s
/// **plus** a [`SearchPhaseReceipt`] attributing wall time and counts to the
/// executed phases. Pure observation — no ranking, expansion, access, or
/// quality logic is added or changed.
///
/// When `opts.collect_phase_receipt == false` the receipt is the
/// [`SearchPhaseReceipt::not_sampled()`] placeholder (zero overhead, all
/// phases `None`); set it to `true` to populate the per-phase breakdown.
/// Production search call sites stay out of scope for #1097 S1; measurement
/// happens in benchmark/eval fixtures (contract `Allowed Scope`).
pub fn hybrid_search_with_receipt(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
) -> Result<(Vec<SearchResult>, SearchPhaseReceipt), MemoryError> {
    let sample = opts.collect_phase_receipt;
    let total_start = sample.then(Instant::now);

    let as_of_utc = opts
        .as_of
        .as_deref()
        .map(crate::db::normalize_utc_iso)
        .transpose()?;
    let include_superseded = opts.include_superseded
        || env_truthy("TACHI_SEARCH_INCLUDE_SUPERSEDED")
        || scoped_path_can_surface_superseded(opts.path_prefix.as_deref());

    let (candidates, candidates_receipt) = candidates::collect_candidates(
        conn,
        query,
        opts,
        include_superseded,
        as_of_utc.as_deref(),
        sample,
    )?;
    if candidates.candidate_ids.is_empty() {
        let receipt = finish_receipt(sample, total_start, |b| {
            b.candidates = candidates_receipt;
            // fetch / rank / graph_expansion / access_recording stay None on
            // the empty-candidate early return — honest "did not execute"
            // rather than zero elapsed.
        });
        return Ok((vec![], receipt));
    }

    // ── Bulk-fetch entries ─────────────────────────────────────────────────────
    let fetch_start = sample.then(Instant::now);
    let entries_map = fetch_by_ids(conn, &candidates.candidate_ids, opts.include_archived)?;
    let fetch_receipt = fetch_start.map(|s| FetchPhaseReceipt {
        elapsed: s.elapsed(),
        fetched_count: entries_map.len(),
    });

    let (mut results, rank_receipt) = ranking::rank_candidate_entries(
        conn,
        ranking::CandidateRanking {
            query,
            opts,
            entries_map,
            vec_scores: &candidates.vec_scores,
            fts_scores: &candidates.fts_scores,
            exact_id: candidates.exact_id.as_deref(),
            include_superseded,
            as_of_utc: as_of_utc.as_deref(),
        },
        sample,
    )?;

    // `append_graph_expansion` mutates `results` in place and returns the
    // receipt by value, so the receipt does not borrow `results` and there is
    // no conflict with the in-place mutation.
    let ((), graph_receipt) = graph_expansion::append_graph_expansion(
        conn,
        &mut results,
        opts,
        include_superseded,
        as_of_utc.as_deref(),
        sample,
    )?;

    // ── Record access (bump counters) ─────────────────────────────────────────
    let access_start = sample.then(Instant::now);
    let access_receipt = if opts.record_access {
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
        access_start.map(|s| AccessRecordingPhaseReceipt {
            elapsed: s.elapsed(),
            // Free: `.len()` on the existing return value (access.rs:73). No
            // new counter logic on the access-recording boundary.
            updated_row_count: access_updates.len(),
        })
    } else {
        None
    };

    let receipt = finish_receipt(sample, total_start, |b| {
        b.candidates = candidates_receipt;
        b.fetch = fetch_receipt;
        b.rank = rank_receipt;
        b.graph_expansion = graph_receipt;
        b.access_recording = access_receipt;
    });

    Ok((results, receipt))
}

/// Receipt-builder closure. Keeps the per-phase plumbing in one place so the
/// honesty invariants (pool_wait / sqlite_retry tags, total_elapsed capture)
/// are not duplicated across the two return paths.
fn finish_receipt(
    sample: bool,
    total_start: Option<Instant>,
    populate: impl FnOnce(&mut SearchPhaseReceipt),
) -> SearchPhaseReceipt {
    if !sample {
        return SearchPhaseReceipt::not_sampled();
    }
    let mut receipt = SearchPhaseReceipt {
        sampled: true,
        total_elapsed: total_start.map(|s| s.elapsed()).unwrap_or(Duration::ZERO),
        candidates: None,
        fetch: None,
        rank: None,
        graph_expansion: None,
        access_recording: None,
        // D1: pool checkout happens above `hybrid_search` (which takes a bare
        // `&Connection`), and the production read path discards the receipt
        // at the `with_store` boundary. Unavailable, not zero.
        pool_wait: LayerAvailability::Unavailable,
        // D1: `retry_memory_locked` is wired only into write paths
        // (crud/derived/enrichment/open). Recall is read-only and never
        // enters the retry loop, so any value here would always read zero.
        sqlite_retry: LayerAvailability::NotApplicable,
    };
    populate(&mut receipt);
    receipt
}

#[cfg(test)]
mod tests;
