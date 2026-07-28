//! Candidate scoring and top-k ranking for hybrid search.

use rusqlite::Connection;
use std::collections::HashMap;
use std::time::Instant;

use crate::{
    db::{get_access_times, get_superseded_ids, get_use_access_times},
    error::MemoryError,
    recall_impressions::{RecallImpressionPayload, RecallImpressionRowDraft},
    scorer::{
        apply_pre_boost_adjustment, cosine_similarity, is_id_like_exact_query, DecayPolicyContext,
        PreBoostAdjustment,
    },
    types::{HybridScore, MemoryEntry, SearchResult},
};

use super::{
    decay_policy,
    expansion::symbolic_query_with_expansion,
    filtering::{is_search_noise_entry, newest_by_shared_entity, quality_multiplier, valid_at},
    recall_config, resolve_weights, ChannelPhaseReceipt, RankPhaseReceipt, SearchOptions,
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

type RankedEntries = (
    Vec<SearchResult>,
    Vec<String>,
    Option<RankPhaseReceipt>,
    Option<RecallImpressionPayload>,
);

/// Importance floor for the decision prior. This is candidate eligibility,
/// not retrieval evidence: the later topical-evidence gate still decides
/// whether the prior may alter rank.
const DECISION_IMPORTANCE_FLOOR: f64 = 0.85;

pub(super) fn rank_candidate_entries(
    conn: &Connection,
    ranking: CandidateRanking<'_>,
    sample: bool,
    capture_impression: bool,
) -> Result<RankedEntries, MemoryError> {
    let phase_start = sample.then(Instant::now);
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
    let requires_pair_evidence = query_requires_pair_evidence(query, recall_config(opts));
    let minimum_symbolic_coverage = minimum_symbolic_query_coverage(query, recall_config(opts));
    let retrieval_evidence = RetrievalEvidence {
        vec_scores,
        fts_scores,
        symbolic_scores: &symbolic_scores,
        exact_id,
        recall_config: recall_config(opts),
        minimum_symbolic_coverage,
        requires_pair_evidence,
    };
    let fetched_ids_vec: Vec<String> = entries_map.keys().cloned().collect();
    // Per #1097 D3: `get_superseded_ids` (ranking.rs:47) is one of two DB I/O
    // hot spots inside `rank_candidate_entries`. Time it on its own so a
    // "rank is slow" report can distinguish DB reads from scoring math.
    let superseded_start = sample.then(Instant::now);
    let fetched_ids_count = fetched_ids_vec.len();
    let superseded_ids = get_superseded_ids(conn, &fetched_ids_vec)?;
    let superseded_elapsed = superseded_start.map(|s| s.elapsed());

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
            if below_minimum_retrieval_evidence(id, e, &retrieval_evidence) {
                return false;
            }
            true
        })
        .map(|(k, v)| (k.clone(), v))
        .collect();

    if entries_ref.is_empty() {
        // #1097 r1 codex review ②: `get_superseded_ids` (ranking.rs:55)
        // already executed and was timed before this filter ran — the rank
        // phase DID run, it just produced zero survivors. Previously this
        // returned `None`, erasing the already-executed DB I/O from the
        // receipt and making "rank is slow" reports lie ("rank never ran").
        // Now we return a `Some(...)` receipt carrying that DB I/O's timing
        // + count so an executed phase is always visible, even with a zero
        // result. `get_access_times` is `None` here, not a zero: it runs
        // AFTER this filter (ranking.rs:96), so when the filter zeros out
        // it never executed — honest "did not run" rather than "ran and
        // matched zero".
        let receipt = phase_start.map(|s| RankPhaseReceipt {
            total_elapsed: s.elapsed(),
            get_superseded_ids: ChannelPhaseReceipt {
                elapsed: superseded_elapsed.unwrap_or_default(),
                candidate_count: fetched_ids_count,
            },
            get_access_times: None,
            mmr_enabled: opts.mmr_threshold.is_some(),
            ranked_result_count: 0,
        });
        return Ok((vec![], vec![], receipt, None));
    }

    let candidate_ids_vec: Vec<String> = entries_ref.keys().cloned().collect();
    // Per #1097 D3: `get_access_times` (ranking.rs:82) is the second DB I/O.
    let access_start = sample.then(Instant::now);
    let access_candidate_count = candidate_ids_vec.len();
    let access_times = read_access_times(conn, opts, &candidate_ids_vec)?;
    let access_elapsed = access_start.map(|s| s.elapsed());
    let weights = resolve_weights(opts);
    let mut scores = merge_pre_boost_scores(
        opts,
        &entries_ref,
        vec_scores,
        fts_scores,
        &symbolic_scores,
        &weights,
        &access_times,
        exact_id,
        include_superseded,
        &superseded_ids,
    );
    let pre_boost_scores = capture_impression.then(|| scores.clone());

    apply_precision_boosts(query, opts, &entries_ref, &weights, &mut scores);
    apply_quality_boosts(opts.path_prefix.as_deref(), &entries_ref, &mut scores);
    apply_access_feedback(
        &entries_ref,
        &access_times,
        recall_config(opts),
        &mut scores,
    );
    apply_tier_boosts(&entries_ref, &mut scores);
    apply_entity_recency_boosts(&entries_ref, &superseded_ids, &mut scores);
    apply_decision_boost(query, &entries_ref, &mut scores);
    apply_lexical_overlap_boost(
        query,
        opts.path_prefix.as_deref(),
        &entries_ref,
        &mut scores,
    );

    // The scorer invariant: these are exactly the post-merge/post-boost score
    // keys, before MMR or `top_k` can remove displayed results. Sorting makes
    // the persistence input deterministic; keys are inherently deduplicated.
    let mut scored_ids: Vec<String> = scores
        .keys()
        .filter(|id| entries_ref.contains_key(*id))
        .cloned()
        .collect();
    scored_ids.sort();

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

    // #1097 D3: MMR is NOT separately timed — splitting it out would require
    // restructuring this function (scoring logic), a quality change out of
    // scope for an instrumentation leaf. The on/off state is recorded via
    // `opts.mmr_threshold.is_some()` so a benchmark can compare the same
    // query both ways without a per-MMR timer.
    let mmr_enabled = opts.mmr_threshold.is_some();
    let ranked_ids: Vec<String> = if let Some(threshold) = opts.mmr_threshold {
        apply_mmr_diversity(&ranked, &entries_map, threshold, opts.top_k)
    } else {
        ranked.iter().map(|(id, _, _)| id.to_string()).collect()
    };
    let impression = pre_boost_scores.map(|pre_scores| {
        build_impression_payload(
            query,
            opts,
            &entries_ref,
            vec_scores,
            fts_scores,
            &symbolic_scores,
            &pre_scores,
            &scores,
            &ranked_ids,
            exact_id,
            include_superseded,
            &superseded_ids,
            &weights,
        )
    });
    drop(entries_ref);

    let mut entries_map = entries_map;
    let results: Vec<SearchResult> = ranked_ids
        .iter()
        .take(opts.top_k)
        .filter_map(|id| {
            let entry = entries_map.remove(id)?;
            let score = scores.get(id)?.clone();
            Some(SearchResult { entry, score })
        })
        .collect();
    let ranked_result_count = results.len();
    let receipt = phase_start.map(|s| RankPhaseReceipt {
        total_elapsed: s.elapsed(),
        get_superseded_ids: ChannelPhaseReceipt {
            elapsed: superseded_elapsed.unwrap_or_default(),
            candidate_count: fetched_ids_count,
        },
        get_access_times: Some(ChannelPhaseReceipt {
            elapsed: access_elapsed.unwrap_or_default(),
            candidate_count: access_candidate_count,
        }),
        mmr_enabled,
        ranked_result_count,
    });
    Ok((results, scored_ids, receipt, impression))
}

fn score_rank_map(
    scores: &HashMap<String, f64>,
    entries: &HashMap<String, &MemoryEntry>,
) -> HashMap<String, usize> {
    let mut ranked = scores
        .iter()
        .map(|(id, score)| {
            let timestamp = entries
                .get(id)
                .map(|entry| crate::scorer::timestamp_epoch_millis(&entry.timestamp))
                .unwrap_or(i64::MIN);
            (id, *score, timestamp)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| crate::scorer::cmp_recall_rank((a.1, a.2, a.0), (b.1, b.2, b.0)));
    ranked
        .into_iter()
        .enumerate()
        .map(|(index, (id, _, _))| (id.clone(), index + 1))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_impression_payload(
    query: &str,
    opts: &SearchOptions,
    entries: &HashMap<String, &MemoryEntry>,
    vec_scores: &HashMap<String, f64>,
    fts_scores: &HashMap<String, f64>,
    symbolic_scores: &HashMap<String, f64>,
    pre_scores: &HashMap<String, HybridScore>,
    final_scores: &HashMap<String, HybridScore>,
    ranked_ids: &[String],
    exact_id: Option<&str>,
    include_superseded: bool,
    superseded_ids: &std::collections::HashSet<String>,
    weights: &crate::scorer::HybridWeights,
) -> RecallImpressionPayload {
    #[cfg(test)]
    IMPRESSION_PAYLOAD_CONSTRUCTIONS.with(|count| count.set(count.get() + 1));
    let vec_ranks = score_rank_map(vec_scores, entries);
    let fts_ranks = score_rank_map(fts_scores, entries);
    let sym_ranks = score_rank_map(symbolic_scores, entries);
    let pre_rank_scores = pre_scores
        .iter()
        .filter(|(id, _)| entries.contains_key(*id))
        .map(|(id, score)| (id.clone(), score.final_score))
        .collect::<HashMap<_, _>>();
    let pre_ranks = score_rank_map(&pre_rank_scores, entries);
    let final_ranks = ranked_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index + 1))
        .collect::<HashMap<_, _>>();
    let mut ids = pre_scores
        .keys()
        .filter(|id| entries.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>();
    ids.sort();
    let rows = ids
        .into_iter()
        .map(|id| {
            let pre = &pre_scores[&id];
            let final_score = &final_scores[&id];
            let is_exact = exact_id == Some(id.as_str());
            let is_superseded = include_superseded && superseded_ids.contains(&id);
            let merge_adjustment = match (is_exact, is_superseded) {
                (true, true) => PreBoostAdjustment::ExactIdSupersededScale,
                (true, false) => PreBoostAdjustment::ExactId,
                (false, true) => PreBoostAdjustment::SupersededScale,
                (false, false) => PreBoostAdjustment::None,
            };
            RecallImpressionRowDraft {
                memory_id: id.clone(),
                vector_score: pre.vector,
                fts_score: pre.fts,
                symbolic_score: pre.symbolic,
                decay_score: pre.decay,
                vec_rank: vec_ranks.get(&id).copied(),
                fts_rank: fts_ranks.get(&id).copied(),
                sym_rank: sym_ranks.get(&id).copied(),
                merge_adjustment,
                pre_boost_score: pre.final_score,
                pre_boost_rank: pre_ranks[&id],
                tie_break_epoch_millis: crate::scorer::timestamp_epoch_millis(
                    &entries[&id].timestamp,
                ),
                final_score: final_score.final_score,
                final_rank: final_ranks[&id],
                scored: true,
                scored_returned: false,
                access_count_at_recall: entries[&id].access_count,
            }
        })
        .collect();
    let weights_profile = if opts.weights != crate::scorer::HybridWeights::default() {
        "custom"
    } else {
        match opts.path_prefix.as_deref().unwrap_or("") {
            path if path.starts_with("/guide") => "guide",
            path if path.starts_with("/wiki")
                || path.starts_with("/behavior")
                || path.starts_with("/rules") =>
            {
                "wiki"
            }
            path if path.starts_with("/events") || path.starts_with("/notes") => "events_notes",
            _ => "default",
        }
    };
    RecallImpressionPayload {
        group_id: uuid::Uuid::new_v4().to_string(),
        created_at: crate::db::now_utc_iso(),
        query_hash: crate::db::query_hash(query),
        weights_profile: weights_profile.to_string(),
        weights: weights.clone(),
        rrf_k: recall_config(opts).rrf_k,
        top_k: opts.top_k,
        rows,
        displayed_count: 0,
    }
}

#[cfg(test)]
thread_local! {
    static IMPRESSION_PAYLOAD_CONSTRUCTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn impression_payload_constructions() -> usize {
    IMPRESSION_PAYLOAD_CONSTRUCTIONS.with(std::cell::Cell::get)
}

/// Pure merge of the four channel scores (vector/FTS/symbolic + ACT-R decay)
/// into a `HybridScore` per candidate, followed by the two score overrides
/// that happen before any multiplicative boost: the exact-id override (score
/// 10.0) and the 0.3x superseded-entry scaling.
///
/// Factored out of `rank_candidate_entries` (tachi#1344 boost-attribution
/// harness) as a pure code-motion — identical calls, identical order,
/// identical values; `rank_candidate_entries` itself is otherwise byte-for-
/// byte unchanged. This lets the `#[cfg(test)]`-gated attribution harness
/// (see `mod attribution` below) reconstruct the exact same pre-boost
/// baseline instead of re-deriving this merge, so no scoring math is
/// duplicated between the production path and the observation path.
#[allow(clippy::too_many_arguments)]
fn merge_pre_boost_scores(
    opts: &SearchOptions,
    entries_ref: &HashMap<String, &MemoryEntry>,
    vec_scores: &HashMap<String, f64>,
    fts_scores: &HashMap<String, f64>,
    symbolic_scores: &HashMap<String, f64>,
    weights: &crate::scorer::HybridWeights,
    access_times: &HashMap<String, Vec<f64>>,
    exact_id: Option<&str>,
    include_superseded: bool,
    superseded_ids: &std::collections::HashSet<String>,
) -> HashMap<String, HybridScore> {
    let mut scores = crate::scorer::hybrid_score_with_policy(
        entries_ref,
        vec_scores,
        fts_scores,
        symbolic_scores,
        weights,
        access_times,
        DecayPolicyContext::new(recall_config(opts), decay_policy(opts)),
    );
    if let Some(exact_id) = exact_id.filter(|id| entries_ref.contains_key(*id)) {
        if let Some(score) = scores.get_mut(exact_id) {
            score.final_score =
                apply_pre_boost_adjustment(score.final_score, PreBoostAdjustment::ExactId);
        }
    }

    if include_superseded {
        for id in superseded_ids {
            if let Some(score) = scores.get_mut(id) {
                score.final_score = apply_pre_boost_adjustment(
                    score.final_score,
                    PreBoostAdjustment::SupersededScale,
                );
            }
        }
    }
    scores
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

/// A weak vector score plus sparse lexical overlap is not recall evidence
/// strong enough to display or reinforce. Filter it before access-history
/// reads and every subsequent ranking boost, while preserving exact IDs,
/// qualified FTS hits, strong vectors, and sufficient symbolic coverage.
struct RetrievalEvidence<'a> {
    vec_scores: &'a HashMap<String, f64>,
    fts_scores: &'a HashMap<String, f64>,
    symbolic_scores: &'a HashMap<String, f64>,
    exact_id: Option<&'a str>,
    recall_config: &'a crate::RecallConfig,
    minimum_symbolic_coverage: f64,
    requires_pair_evidence: bool,
}

fn below_minimum_retrieval_evidence(
    id: &str,
    entry: &MemoryEntry,
    evidence: &RetrievalEvidence<'_>,
) -> bool {
    if evidence.exact_id == Some(id) {
        return false;
    }
    let has_fts_evidence = evidence
        .fts_scores
        .get(id)
        .is_some_and(|score| score.is_finite() && *score > 0.0);
    let symbolic = evidence
        .symbolic_scores
        .get(id)
        .copied()
        .filter(|score| score.is_finite())
        .unwrap_or(0.0);
    let has_minimum_symbolic_coverage =
        symbolic + f64::EPSILON >= evidence.minimum_symbolic_coverage;
    if has_fts_evidence
        && (!evidence.requires_pair_evidence
            || has_minimum_symbolic_coverage
            || is_high_importance_decision(entry))
    {
        return false;
    }
    let vector_is_strong = evidence.vec_scores.get(id).is_some_and(|score| {
        score.is_finite() && *score >= evidence.recall_config.vector_only_similarity_floor
    });
    if vector_is_strong {
        return false;
    }
    !has_minimum_symbolic_coverage
}

fn query_requires_pair_evidence(query: &str, recall_config: &crate::RecallConfig) -> bool {
    let query_term_count = crate::scorer::tokenize(query)
        .into_iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    recall_config.or_fallback_fts_pair_min_query_terms >= 2
        && query_term_count >= recall_config.or_fallback_fts_pair_min_query_terms
}

fn is_high_importance_decision(entry: &MemoryEntry) -> bool {
    entry.category.eq_ignore_ascii_case("decision") && entry.importance >= DECISION_IMPORTANCE_FLOOR
}

fn minimum_symbolic_query_coverage(query: &str, recall_config: &crate::RecallConfig) -> f64 {
    let required_symbolic_matches = if query_requires_pair_evidence(query, recall_config) {
        2.0
    } else {
        1.0
    };
    // `symbolic_scores` uses the expansion-aware query, so recover its actual
    // matched-token count with that same denominator. The raw count above is
    // intentionally retained only for deciding whether this was a rich query.
    let expanded_query_term_count = crate::scorer::tokenize(&symbolic_query_with_expansion(query))
        .into_iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    required_symbolic_matches / expanded_query_term_count.max(1) as f64
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

/// Phase 2 P3 — topical-evidence gate for agent-judged boosts.
///
/// The `category == "decision"` signal is an *agent-authored prior*, not
/// evidence that any retrieval channel matched the query. On the ops-audit
/// hindsight fixture the decision seed reaches rank 2 inside `Surface::Memory`
/// purely on DECISION_BOOST × the recency tie-break, with ZERO topical
/// standing: its only symbolic score is the dense-map noise floor of a single
/// shared token. This gate withholds the boost from candidates a real
/// retrieval channel never matched, so an agent category can no longer
/// out-rank a genuinely-retrieved research note.
///
/// A candidate has topical evidence when ANY real channel matched:
/// * `fts > FTS_TOPICAL_EPSILON` — a genuine bm25 hit. The epsilon rejects the
///   IDF-null crumb (measured ~1.2e-7 for a term present in every doc) while
///   admitting a real match (measured ~0.55);
/// * `vector > 0.0` — any dense-embedding similarity;
/// * `symbolic * |Q_expanded| >= MIN_SYMBOLIC_OVERLAP` — at least two distinct
///   expanded-query tokens overlap. `symbolic` is (distinct matched tokens /
///   distinct expanded query size), so `symbolic * |Q_expanded|` recovers the
///   raw distinct matched-token count; a single shared token (the corpus floor,
///   `overlap == 1`) is the dense-map noise crumb and does NOT count.
///
/// # On `FTS_TOPICAL_EPSILON` (codex Concern A)
/// The epsilon is **fixture-calibrated, not structurally derived**: `fts` here
/// is the raw `-bm25` channel score and there is no guaranteed distributional
/// gap at exactly `1e-3` separating an IDF-null crumb from a genuine weak match.
/// The gate is safe anyway because it is an **OR of three channels**: a real
/// match that happens to be weak in fts still opens the gate through
/// `symbolic * |Q_expanded| >= 1.5` (≥2 distinct query tokens) or `vector > 0`.
/// The *only* candidate this epsilon can false-negative is one whose SOLE signal
/// is an fts score in the narrow band `(1e-3, genuine)` with exactly one
/// distinct symbolic token and no vector — i.e. a near-IDF-null crumb, not a
/// meaningful topical match. So the epsilon bounds a benign residual, and the
/// two evidence-bearing channels carry any real weak-fts match. (If a future
/// corpus shows genuine matches landing in that band, ε needs a structural
/// re-derivation, not a fixture retune — that is Glinda's constant to move.)
pub(super) const FTS_TOPICAL_EPSILON: f64 = 1e-3; // measured: bm25 IDF-null crumb 1.2e-7 vs genuine 0.55
pub(super) const MIN_SYMBOLIC_OVERLAP: f64 = 1.5; // symbolic*|Q_expanded| >= 1.5  ==  >=2 distinct query tokens (1 = corpus floor)

pub(super) fn has_topical_evidence(score: &HybridScore, expanded_query_tokens: usize) -> bool {
    score.fts > FTS_TOPICAL_EPSILON
        || score.vector > 0.0
        || score.symbolic * expanded_query_tokens as f64 >= MIN_SYMBOLIC_OVERLAP
}

/// The symbolic scorer's exact denominator: the count of **distinct** expanded-
/// query tokens. `scorer::symbolic_score_fields` (scorer/text.rs:90-112) builds
/// `query_tokens` as a DEDUPLICATED `HashSet` of `tokenize(expanded_query)` and
/// divides the overlap by its length, so `symbolic = distinct_overlap /
/// distinct_Q`. The gate reconstructs `distinct_overlap = symbolic * this`, so
/// it MUST use the same deduplicated count — a plain `tokenize().len()` (which
/// keeps duplicate query terms) inflates the product and would leak
/// DECISION_BOOST onto a single-distinct-token overlap when the query repeats a
/// term (e.g. "alpha alpha beta": non-dedup 3 vs distinct 2). Single source of
/// truth: the probe reconstructs the gate through this same helper.
pub(super) fn distinct_expanded_query_tokens(query: &str) -> usize {
    crate::scorer::tokenize(&symbolic_query_with_expansion(query))
        .into_iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// Same-store precision helper for ops-audit / #708 Phase D follow-ons:
/// high-importance **decisions** must surface over keyword-flooded wiki/stubs —
/// but ONLY when the decision itself was matched by a real retrieval channel
/// (P3: gate on `has_topical_evidence`). A decision that stands purely on its
/// agent-authored category, with no channel match, keeps its mechanical score.
///
/// (The former research-path boost that lifted `/wiki/**/research/**` notes
/// over denser architecture wikis was retired in Phase 2 — research notes are
/// a distinct retrieval surface, `Surface::Memory`, so the ops-audit
/// adjacent-wiki steal is dissolved by surface scoping, not a magic
/// multiplier. See `ops_audit_corpus.rs` `hindsight-research-wiki`.)
///
/// Provisional multiplier — calibrate only via ops_audit + golden_corpus.
fn apply_decision_boost(
    query: &str,
    entries_ref: &HashMap<String, &MemoryEntry>,
    scores: &mut HashMap<String, HybridScore>,
) {
    /// provisional decision boost (tachi#708/#896 same-store precision).
    const DECISION_BOOST: f64 = 1.55;

    // Distinct expanded-query token count — the symbolic scorer's own
    // (deduplicated) denominator, so `symbolic * q_tokens` in the gate recovers
    // the true distinct matched-token count (see `distinct_expanded_query_tokens`).
    let q_tokens = distinct_expanded_query_tokens(query);

    for (id, entry) in entries_ref {
        if !is_high_importance_decision(entry) {
            continue;
        }
        if let Some(score) = scores.get_mut(id) {
            // P3 gate: the agent-authored decision category is a prior, not
            // evidence. Only lift a candidate a real channel matched.
            if !has_topical_evidence(score, q_tokens) {
                continue;
            }
            if score.final_score.is_finite() && score.final_score > 0.0 {
                score.final_score *= DECISION_BOOST;
            }
        }
    }
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
    /// Provisional candidate-pool cutoff: a lexical-overlap boost is precision
    /// evidence only when it includes a query term that distinguishes a
    /// bounded share of the candidate set. Terms repeated across many
    /// candidates are already represented by the base retrieval channels and
    /// must not amplify templated inventories.
    const DISCRIMINATIVE_TOKEN_DOCUMENT_FREQUENCY_DENOMINATOR: usize = 4;

    let q_tokens = soft_token_set(query);
    let q_ngrams = char_ngrams(query, 4);
    if q_tokens.len() < 3 && q_ngrams.len() < 8 {
        return;
    }
    // Pools below four candidates keep base retrieval ordering because quarter-share evidence is undefined.
    if entries_ref.len() < DISCRIMINATIVE_TOKEN_DOCUMENT_FREQUENCY_DENOMINATOR {
        return;
    }
    let mut query_token_document_frequencies = HashMap::new();
    for entry in entries_ref.values() {
        let mut text = String::new();
        text.push_str(&entry.summary);
        text.push(' ');
        text.push_str(&entry.text);
        for keyword in &entry.keywords {
            text.push(' ');
            text.push_str(keyword);
        }
        let entry_tokens = soft_token_set(&text);
        for token in q_tokens.intersection(&entry_tokens) {
            *query_token_document_frequencies
                .entry(token.clone())
                .or_insert(0_usize) += 1;
        }
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
        let has_discriminative_token_overlap = q_tokens.intersection(&e_tokens).any(|token| {
            query_token_document_frequencies
                .get(token)
                .is_some_and(|frequency| {
                    frequency.saturating_mul(DISCRIMINATIVE_TOKEN_DOCUMENT_FREQUENCY_DENOMINATOR)
                        <= entries_ref.len()
                })
        });
        if !has_discriminative_token_overlap {
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

/// tachi#1446 lever 5 — the single decision of *which provenance* the ACT-R
/// base-level-activation floor is allowed to see, for both the production
/// ranker and its `#[cfg(test)]` attribution twin.
///
/// Off (rollback): `access_history` rows written by the search path itself,
/// i.e. the pre-#1446 behaviour, byte-identical because every row that
/// predates the `event_kind` column carries `display`.
/// On (default): rows written by `db::record_memory_use` only, so the system's own act
/// of displaying a result can no longer be read back as evidence about the
/// memory — at the floor (`scorer::default_decay_score_actr_with_config`), at
/// the decay frequency term (lever 2) and at [`apply_access_feedback`]
/// (lever 4), all three of which consume this one map.
fn read_access_times(
    conn: &Connection,
    opts: &SearchOptions,
    candidate_ids: &[String],
) -> Result<HashMap<String, Vec<f64>>, MemoryError> {
    if recall_config(opts).use_provenance_recency {
        get_use_access_times(conn, candidate_ids)
    } else {
        get_access_times(conn, candidate_ids)
    }
}

fn apply_access_feedback(
    entries_ref: &HashMap<String, &MemoryEntry>,
    access_times: &HashMap<String, Vec<f64>>,
    recall_config: &crate::recall_config::RecallConfig,
    scores: &mut HashMap<String, HybridScore>,
) {
    for (id, entry) in entries_ref {
        // tachi#1446 lever 4. This multiplies the FINAL score, so unlike
        // levers 1-3 it is not scaled by `weights.decay` — on a `/guide` or
        // `/wiki` profile (decay 0.02) it is the dominant exposure channel.
        //
        // Knob on: the count is the number of `event_kind = 'use'` rows
        // `read_access_times` already fetched for this candidate — no extra
        // query, no maintained column. Knob off: `entry.access_count`,
        // incremented once per row per search, exactly as before.
        //
        // tachi#1459: `access_count` observes the search path only; reads
        // through path-listing routes do not increment it. The knob-off boost
        // is thus paid out for search exposure alone, and a memory that is read
        // constantly through `list_by_path` and friends never earns it.
        let count = if recall_config.use_provenance_recency {
            access_times.get(id).map_or(0, Vec::len) as i64
        } else {
            entry.access_count
        };
        if count >= 2 {
            let boost = 1.0 + (count as f64).ln_1p() * 0.03;
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
        // Wiki pattern/consolidated pages already carry dense keyword bags, so
        // keep their tier boosts milder to avoid over-lifting keyword-dense
        // reference docs within their own result set. Non-wiki pattern
        // knowledge (decisions, notes, research) retains the stronger lift.
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

// ---------------------------------------------------------------------------
// tachi#1344 (Phase 0 boost-attribution harness) — observation-only rank
// decomposition. See `search/tests/rank_attribution.rs` for the JSONL driver
// that runs this against the golden_corpus / ops_audit_corpus fixtures.
//
// Zero production overhead: this entire module is `#[cfg(test)]`-gated (the
// same convention `mod tests` below already uses) — a default `cargo build`
// / `cargo build --release` / `cargo clippy` (without `--tests`) does not
// compile any of it, so there is no runtime branch, no allocation, and no
// code-size cost on the production path. The only non-test-gated change
// this leaf makes to `rank_candidate_entries` above is the
// `merge_pre_boost_scores` extraction — a pure code-motion (identical calls,
// identical order, identical values); `rank_candidate_entries`'s own
// behavior is unchanged.
//
// This does NOT re-derive the boost math: every step below calls the exact
// same private `apply_*_boost` functions the production sequence
// (ranking.rs `rank_candidate_entries`, the `apply_precision_boosts` .. `
// apply_lexical_overlap_boost` calls) invokes, in the same order, on a
// snapshot-observed clone of the identical pre-boost baseline
// (`merge_pre_boost_scores`). The one thing NOT shared by construction is
// the CALL SEQUENCE ITSELF (7 one-line calls, listed a second time below) —
// if a future change adds/removes/reorders a boost in `rank_candidate_entries`,
// this module's list must be updated to match by hand; there is no
// compile-time link between the two sequences, only this comment.
#[cfg(test)]
pub(super) mod attribution {
    use super::*;

    /// One boost step's effect on every candidate it touched (or didn't).
    /// `before`/`after` are `final_score` snapshots keyed by candidate id,
    /// taken immediately before and after this ONE `apply_*` call in the
    /// real production sequence — every other boost is held at whatever
    /// state it was actually in at that point (this is the real sequence,
    /// snapshotted, not an isolated/idealized replay).
    #[derive(Debug, Clone)]
    pub(crate) struct BoostStep {
        pub(crate) label: &'static str,
        before: HashMap<String, f64>,
        after: HashMap<String, f64>,
    }

    impl BoostStep {
        /// This step's multiplier on `id` (`after / before`), or `None` if
        /// `id` had no score at this step (filtered out / not a candidate)
        /// or its pre-step score was exactly `0.0` (multiplier undefined —
        /// `0.0 * anything` stays `0.0`, so "what multiplier was applied"
        /// has no determinate answer).
        pub(crate) fn multiplier_for(&self, id: &str) -> Option<f64> {
            let b = *self.before.get(id)?;
            let a = *self.after.get(id)?;
            if b == 0.0 {
                return None;
            }
            Some(a / b)
        }

        /// `true` iff this step changed `id`'s score by more than float
        /// noise. Every boost in this file is a multiplier `>= 1.0`, so any
        /// change this small is measurement noise, not an applied boost.
        pub(crate) fn hit(&self, id: &str) -> bool {
            self.multiplier_for(id)
                .is_some_and(|m| (m - 1.0).abs() > 1e-9)
        }
    }

    /// Full attribution for one ranking call: the pre-boost baseline
    /// `HybridScore` per candidate (vector/FTS/symbolic/decay merge, before
    /// any boost), each of the 7 boost steps in production order, and the
    /// resulting final scores.
    #[derive(Debug, Clone)]
    pub(crate) struct RankAttribution {
        pub(crate) base_scores: HashMap<String, HybridScore>,
        pub(crate) steps: Vec<BoostStep>,
        pub(crate) final_scores: HashMap<String, f64>,
    }

    /// Attribution twin of `rank_candidate_entries`. Re-derives the same
    /// pre-boost baseline via the shared `merge_pre_boost_scores` (no
    /// scoring math duplicated there), then walks the SAME 7 boost calls
    /// `rank_candidate_entries` makes, in the SAME order, snapshotting
    /// scores before/after each. Does not sort, apply MMR, or truncate to
    /// `top_k` — callers that need actual production rank order should call
    /// `hybrid_search`/`rank_candidate_entries` separately (see
    /// `hybrid_search_with_attribution` in `search.rs`, which does exactly
    /// that pairing).
    pub(crate) fn rank_candidate_entries_with_attribution(
        conn: &Connection,
        ranking: CandidateRanking<'_>,
    ) -> Result<RankAttribution, MemoryError> {
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
        let requires_pair_evidence = query_requires_pair_evidence(query, recall_config(opts));
        let minimum_symbolic_coverage = minimum_symbolic_query_coverage(query, recall_config(opts));
        let retrieval_evidence = RetrievalEvidence {
            vec_scores,
            fts_scores,
            symbolic_scores: &symbolic_scores,
            exact_id,
            recall_config: recall_config(opts),
            minimum_symbolic_coverage,
            requires_pair_evidence,
        };
        let fetched_ids_vec: Vec<String> = entries_map.keys().cloned().collect();
        let superseded_ids = get_superseded_ids(conn, &fetched_ids_vec)?;

        // Same candidate filter as `rank_candidate_entries` (ranking.rs
        // above) — kept as a second copy rather than shared because the
        // production copy is entangled with phase-receipt timing this
        // observation-only twin does not need.
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
                if below_minimum_retrieval_evidence(id, e, &retrieval_evidence) {
                    return false;
                }
                true
            })
            .map(|(k, v)| (k.clone(), v))
            .collect();

        if entries_ref.is_empty() {
            return Ok(RankAttribution {
                base_scores: HashMap::new(),
                steps: Vec::new(),
                final_scores: HashMap::new(),
            });
        }

        let candidate_ids_vec: Vec<String> = entries_ref.keys().cloned().collect();
        let access_times = read_access_times(conn, opts, &candidate_ids_vec)?;
        let weights = resolve_weights(opts);
        let mut scores = merge_pre_boost_scores(
            opts,
            &entries_ref,
            vec_scores,
            fts_scores,
            &symbolic_scores,
            &weights,
            &access_times,
            exact_id,
            include_superseded,
            &superseded_ids,
        );
        let base_scores = scores.clone();

        let mut steps: Vec<BoostStep> = Vec::with_capacity(7);
        fn snapshot(scores: &HashMap<String, HybridScore>) -> HashMap<String, f64> {
            scores
                .iter()
                .map(|(k, v)| (k.clone(), v.final_score))
                .collect()
        }

        let before = snapshot(&scores);
        apply_precision_boosts(query, opts, &entries_ref, &weights, &mut scores);
        steps.push(BoostStep {
            label: "precision",
            before,
            after: snapshot(&scores),
        });

        let before = snapshot(&scores);
        apply_quality_boosts(opts.path_prefix.as_deref(), &entries_ref, &mut scores);
        steps.push(BoostStep {
            label: "quality",
            before,
            after: snapshot(&scores),
        });

        let before = snapshot(&scores);
        apply_access_feedback(
            &entries_ref,
            &access_times,
            recall_config(opts),
            &mut scores,
        );
        steps.push(BoostStep {
            label: "access_feedback",
            before,
            after: snapshot(&scores),
        });

        let before = snapshot(&scores);
        apply_tier_boosts(&entries_ref, &mut scores);
        steps.push(BoostStep {
            label: "tier",
            before,
            after: snapshot(&scores),
        });

        let before = snapshot(&scores);
        apply_entity_recency_boosts(&entries_ref, &superseded_ids, &mut scores);
        steps.push(BoostStep {
            label: "entity_recency",
            before,
            after: snapshot(&scores),
        });

        let before = snapshot(&scores);
        // Renamed on main when Phase 2 dissolved the research-path boost
        // (`apply_decision_and_research_boosts` -> `apply_decision_boost`).
        // The label moves with it: a report that still said
        // "decision_and_research" would name a boost this build does not apply.
        apply_decision_boost(query, &entries_ref, &mut scores);
        steps.push(BoostStep {
            label: "decision",
            before,
            after: snapshot(&scores),
        });

        let before = snapshot(&scores);
        apply_lexical_overlap_boost(
            query,
            opts.path_prefix.as_deref(),
            &entries_ref,
            &mut scores,
        );
        steps.push(BoostStep {
            label: "lexical_overlap",
            before,
            after: snapshot(&scores),
        });

        let final_scores = snapshot(&scores);

        Ok(RankAttribution {
            base_scores,
            steps,
            final_scores,
        })
    }
}

#[cfg(test)]
mod tests {
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
        let mut scores: HashMap<String, HybridScore> =
            [("dec".to_string(), sc)].into_iter().collect();

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
}
