//! Candidate scoring and top-k ranking for hybrid search.

use rusqlite::Connection;
use std::collections::HashMap;
use std::time::Instant;

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

pub(super) fn rank_candidate_entries(
    conn: &Connection,
    ranking: CandidateRanking<'_>,
    sample: bool,
) -> Result<(Vec<SearchResult>, Option<RankPhaseReceipt>), MemoryError> {
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
        return Ok((vec![], receipt));
    }

    let candidate_ids_vec: Vec<String> = entries_ref.keys().cloned().collect();
    // Per #1097 D3: `get_access_times` (ranking.rs:82) is the second DB I/O.
    let access_start = sample.then(Instant::now);
    let access_candidate_count = candidate_ids_vec.len();
    let access_times = get_access_times(conn, &candidate_ids_vec)?;
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

    apply_precision_boosts(query, opts, &entries_ref, &weights, &mut scores);
    apply_quality_boosts(opts.path_prefix.as_deref(), &entries_ref, &mut scores);
    apply_access_feedback(&entries_ref, &mut scores);
    apply_tier_boosts(&entries_ref, &mut scores);
    apply_entity_recency_boosts(&entries_ref, &superseded_ids, &mut scores);
    apply_decision_and_research_boosts(query, &entries_ref, &mut scores);
    apply_lexical_overlap_boost(
        query,
        opts.path_prefix.as_deref(),
        &entries_ref,
        &mut scores,
    );

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
    Ok((results, receipt))
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
        for id in superseded_ids {
            if let Some(score) = scores.get_mut(id) {
                score.final_score *= 0.3;
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
        let access_times = get_access_times(conn, &candidate_ids_vec)?;
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
        apply_access_feedback(&entries_ref, &mut scores);
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
        apply_decision_and_research_boosts(query, &entries_ref, &mut scores);
        steps.push(BoostStep {
            label: "decision_and_research",
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
}
