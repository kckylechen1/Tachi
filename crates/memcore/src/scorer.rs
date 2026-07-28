// scorer.rs — Pure-Rust hybrid scoring engine (no I/O, no SQLite)
//
// Replaces JS `scorer.ts` (cosineSimilarity / hybridScore / rankHybrid)
// and Python `store.py:hybrid_search` weighting logic.

use crate::recall_config::RecallConfig;
use crate::types::{HybridScore, MemoryEntry};
use chrono::{NaiveDate, Utc};
use std::collections::HashMap;

mod graph;
mod text;

pub use graph::{
    graph_relation_activation_weight, graph_spreading_activation_with_seed_weights, local_pagerank,
};
pub use text::{
    entry_has_exact_query_token, generic_precision_multiplier, is_id_like_exact_query,
    symbolic_score, symbolic_score_entry, tokenize, PrecisionMatcher,
};
pub(crate) use text::{generic_precision_multiplier_impl_with_config, symbolic_score_stored_entry};

fn tier_half_life_with_config(tier: &str, recall_config: &RecallConfig) -> f64 {
    recall_config.half_life_days_for_tier(tier)
}

fn tier_actr_d(tier: &str) -> f64 {
    match tier {
        "pattern" => 0.01,
        "consolidated" => 0.25,
        _ => 0.5, // raw
    }
}

/// Policy hook for library-specific recall decay.
///
/// The kernel owns the scorer call site; downstream libraries may adapt their
/// own semantics into this trait without hardcoding product behavior here.
///
/// Inject via [`crate::SearchOptions::decay_policy`] (`Arc<dyn DecayPolicy>`) so
/// HyperMemory trading half-lives / chat affect decay stay out of the kernel.
/// `Send + Sync` matches [`PrecisionMatcher`] so policies can cross thread
/// boundaries with `Arc`.
pub trait DecayPolicy: Send + Sync {
    fn score_decay(
        &self,
        entry: &MemoryEntry,
        recall_config: &RecallConfig,
        access_ages: Option<&[f64]>,
    ) -> f64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultDecayPolicy;

pub static DEFAULT_DECAY_POLICY: DefaultDecayPolicy = DefaultDecayPolicy;

impl DecayPolicy for DefaultDecayPolicy {
    fn score_decay(
        &self,
        entry: &MemoryEntry,
        recall_config: &RecallConfig,
        access_ages: Option<&[f64]>,
    ) -> f64 {
        default_decay_score_actr_with_config(entry, access_ages, recall_config)
    }
}

#[derive(Clone, Copy)]
pub struct DecayPolicyContext<'a> {
    pub recall_config: &'a RecallConfig,
    pub decay_policy: &'a dyn DecayPolicy,
}

impl<'a> DecayPolicyContext<'a> {
    pub fn new(recall_config: &'a RecallConfig, decay_policy: &'a dyn DecayPolicy) -> Self {
        Self {
            recall_config,
            decay_policy,
        }
    }
}

/// Normalise an f64 to [0, 1].
#[inline]
pub fn normalize(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Cosine similarity between two equal-length f32 slices.
/// Returns 0.0 if dimensions differ or either vector is zero-magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut mag_a = 0.0_f64;
    let mut mag_b = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        mag_a += x * x;
        mag_b += y * y;
    }
    let mag = mag_a.sqrt() * mag_b.sqrt();
    if mag == 0.0 {
        0.0
    } else {
        (dot / mag).clamp(-1.0, 1.0)
    }
}

/// Memory decay score (ACT-R Nowledge Mem formula).
///
/// `decay = max(recency × (1 + 0.2 × log10(1 + access_count)), importance × 0.3)`
/// where `recency = exp(-0.693 × age_days / configured_half_life_days)`
pub fn decay_score(entry: &MemoryEntry) -> f64 {
    decay_score_with_config(entry, RecallConfig::get())
}

pub fn decay_score_with_config(entry: &MemoryEntry, recall_config: &RecallConfig) -> f64 {
    default_decay_score_with_config(entry, recall_config, None)
}

pub fn decay_score_with_policy(
    entry: &MemoryEntry,
    recall_config: &RecallConfig,
    policy: &(impl DecayPolicy + ?Sized),
) -> f64 {
    policy.score_decay(entry, recall_config, None)
}

/// `use_access_ages` is the candidate's `event_kind = 'use'` access ages, as
/// already fetched for the ACT-R floor when
/// `RecallConfig::use_provenance_recency` is on (`db::get_use_access_times`).
/// It is `None` on every knob-off path and on the public
/// [`decay_score_with_config`] entry point, which has no access history to
/// offer — see the `frequency` term below for what it is used for and why
/// `None` is the honest input rather than a fallback to `access_count`.
fn default_decay_score_with_config(
    entry: &MemoryEntry,
    recall_config: &RecallConfig,
    use_access_ages: Option<&[f64]>,
) -> f64 {
    let now = Utc::now();
    // tachi#1446 lever 1. The stored timestamp this reads is the whole of the
    // exposure loop's dominant channel: `last_access` is written for every row
    // a search RETURNS (`record_access_with_updates` in
    // `db/memory_crud/access.rs`), so reading it here
    // makes the system's own act of display the age reference. One exposure
    // sets `age_days = 0` for every returned row simultaneously, which does not
    // merely inflate them — it collapses this channel's dynamic range to a
    // constant, so decay stops discriminating between candidates at all
    // (measured in `search/tests/exposure_loop.rs`).
    //
    // With `use_provenance_recency` ON the reference is `last_use_at`, which is
    // written only by the explicit caller-use path. An entry with no recorded
    // use therefore falls through to the same content-derived references an
    // unexposed entry uses — the leading `[YYYY-MM-DD]` event date, then
    // `timestamp` — so a search display cannot become evidence about memory use.
    //
    // OFF is byte-identical to the pre-#1446 chain; only the first link of the
    // `or_else` fallthrough differs between the two arms.
    let recency_anchor = if recall_config.use_provenance_recency {
        entry.last_use_at.as_ref()
    } else {
        entry.last_access.as_ref()
    };
    let reference = recency_anchor
        .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
        .or_else(|| leading_event_datetime(&entry.text))
        .unwrap_or_else(|| {
            entry
                .timestamp
                .parse::<chrono::DateTime<Utc>>()
                .unwrap_or_else(|_| stale_reference_datetime())
        });
    let age_days = (now - reference).num_seconds().max(0) as f64 / 86_400.0;

    let half_life = tier_half_life_with_config(&entry.tier, recall_config);
    let recency = (-0.693 * age_days / half_life).exp();
    // tachi#1446 lever 2. `access_count` is incremented for every row a search
    // RETURNS (`db/memory_crud/access.rs`'s `record_access_with_updates`), so
    // with the knob off this frequency term is the second channel through
    // which being displayed pays: one exposure multiplies the recency term by
    // `1 + 0.2*log10(2) = 1.060` for every returned row at once.
    //
    // tachi#1459: that counter observes the search path only; reads through
    // path-listing routes do not increment it. So the knob-off frequency term
    // is not "how much this memory is used" — it is "how often search has shown
    // it", and it stays flat at zero for a memory whose readers all arrive
    // through `list_by_path` and friends.
    //
    // With `use_provenance_recency` ON the count comes from the same use-event
    // rows the ACT-R floor is already reading, so it costs no extra query and
    // no maintained column (a `use_count` column would be a second write-side
    // drift surface for a number `access_history` already holds). `None` and
    // the empty slice both mean "no recorded use", i.e. `frequency = 0` —
    // deliberately NOT a fallback to `access_count`, which would re-admit the
    // exposure channel through the back door.
    let frequency_count = if recall_config.use_provenance_recency {
        use_access_ages.map_or(0, <[f64]>::len) as f64
    } else {
        entry.access_count as f64
    };
    let frequency = (1.0 + frequency_count).log10();
    let importance_floor = entry.importance * 0.3;

    (recency * (1.0 + 0.2 * frequency)).max(importance_floor)
}

fn leading_event_datetime(text: &str) -> Option<chrono::DateTime<Utc>> {
    let rest = text.trim_start().strip_prefix('[')?;
    let date = rest.get(0..10)?;
    if !matches!(
        date.as_bytes(),
        [d0, d1, d2, d3, b'-', m0, m1, b'-', day0, day1]
            if d0.is_ascii_digit()
                && d1.is_ascii_digit()
                && d2.is_ascii_digit()
                && d3.is_ascii_digit()
                && m0.is_ascii_digit()
                && m1.is_ascii_digit()
                && day0.is_ascii_digit()
                && day1.is_ascii_digit()
    ) {
        return None;
    }
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some(date.and_hms_opt(12, 0, 0)?.and_utc())
}

fn stale_reference_datetime() -> chrono::DateTime<Utc> {
    NaiveDate::from_ymd_opt(1970, 1, 1)
        .expect("valid epoch date")
        .and_hms_opt(0, 0, 0)
        .expect("valid epoch time")
        .and_utc()
}

/// ACT-R Base-Level Activation: B_i = ln(Σ t_j^(-d))
/// Where t_j is the age of each access in seconds, d is the decay parameter.
/// More frequent and more recent accesses → higher activation.
/// Returns 0.0 if no access history (falls back to existing decay_score).
pub fn base_level_activation(access_ages_secs: &[f64], d: f64) -> f64 {
    if access_ages_secs.is_empty() {
        return 0.0;
    }
    let sum: f64 = access_ages_secs
        .iter()
        .map(|t| (t / 86_400.0).max(1.0 / 24.0).powf(-d))
        .sum();
    if sum > 0.0 {
        sum.ln()
    } else {
        0.0
    }
}

/// Enhanced decay score using ACT-R base-level activation when access history is available.
/// Falls back to the simplified decay_score when no history is provided.
pub fn decay_score_actr(entry: &MemoryEntry, access_ages: Option<&[f64]>) -> f64 {
    decay_score_actr_with_config(entry, access_ages, RecallConfig::get())
}

pub fn decay_score_actr_with_config(
    entry: &MemoryEntry,
    access_ages: Option<&[f64]>,
    recall_config: &RecallConfig,
) -> f64 {
    decay_score_actr_with_policy(entry, access_ages, recall_config, &DEFAULT_DECAY_POLICY)
}

pub fn decay_score_actr_with_policy(
    entry: &MemoryEntry,
    access_ages: Option<&[f64]>,
    recall_config: &RecallConfig,
    policy: &(impl DecayPolicy + ?Sized),
) -> f64 {
    policy.score_decay(entry, recall_config, access_ages)
}

fn default_decay_score_actr_with_config(
    entry: &MemoryEntry,
    access_ages: Option<&[f64]>,
    recall_config: &RecallConfig,
) -> f64 {
    let d = tier_actr_d(&entry.tier);
    // tachi#1446 lever 5 note: with `use_provenance_recency` on, `access_ages`
    // arrives from `db::get_use_access_times` and therefore holds use events
    // only; with it off it arrives from `db::get_access_times` and holds
    // display events only, exactly as before. Which read ran is the caller's
    // decision (`search/ranking.rs`), so this function passes the ages it was
    // given straight through to lever 2's frequency count under the same knob.
    let use_access_ages = recall_config
        .use_provenance_recency
        .then_some(access_ages)
        .flatten();
    match access_ages {
        Some(ages) if !ages.is_empty() => {
            let bla = base_level_activation(ages, d);
            // Normalize to [0, 1] range: BLA typically ranges from -5 to +5
            let normalized = (bla + 5.0) / 10.0;
            normalized
                .clamp(0.0, 1.0)
                .max(default_decay_score_with_config(
                    entry,
                    recall_config,
                    use_access_ages,
                ))
                .max(entry.importance * 0.3)
        }
        _ => default_decay_score_with_config(entry, recall_config, use_access_ages),
    }
}

/// Compute surprise score for a memory entry based on novelty signals.
/// Surprise is higher for:
///   - New topics (topic not matching common patterns)
///   - Entries with contradicting edges (contradiction_count > 0)
///   - High importance combined with low access (unexpected significance)
///
/// Returns a value in [0, 1] that can be used to boost importance.
/// This is a pure computation — no LLM calls needed.
pub fn surprise_score(
    entry: &MemoryEntry,
    avg_importance: f64,
    contradiction_count: u32,
    total_same_topic: u32,
) -> f64 {
    surprise_score_with_config(
        entry,
        avg_importance,
        contradiction_count,
        total_same_topic,
        RecallConfig::get(),
    )
}

/// [`surprise_score`] with an explicit [`RecallConfig`] — tachi#1446 lever 3.
///
/// The knob only reaches component 4 (`overlooked`); every other component is
/// config-independent, so with `use_provenance_recency` off this is the same
/// number `surprise_score` has always returned.
pub fn surprise_score_with_config(
    entry: &MemoryEntry,
    avg_importance: f64,
    contradiction_count: u32,
    total_same_topic: u32,
    recall_config: &RecallConfig,
) -> f64 {
    // Component 1: Importance surprise — normalized to [0, 1] via clamping
    let importance_surprise = (entry.importance - avg_importance).abs().clamp(0.0, 1.0);

    // Component 2: Contradiction signal — normalized to [0, 1] via log1p / cap
    let contradiction_surprise = if contradiction_count > 0 {
        (1.0 + contradiction_count as f64).ln_1p() / 4.0 // cap at ~1.0 at ~50 contradictions
    } else {
        0.0
    };

    // Component 3: Topic novelty — already in [0, 1]
    let topic_novelty = if total_same_topic <= 1 {
        0.5
    } else {
        1.0 / (total_same_topic as f64)
    };

    // Component 4: Low-access high-importance = overlooked valuable memory.
    //
    // tachi#1446 lever 3, and the polarity here is inverted relative to the
    // other levers: exposure does not *earn* this component, it permanently
    // *revokes* it. One search that returns the row takes `access_count` from
    // 0 to 1 forever (nothing decrements it), so a memory stops counting as
    // "overlooked" the first time the system looks at it — which is precisely
    // the state of being overlooked-but-displayed.
    //
    // `last_use_at IS NULL` already means "never used" (nothing else writes
    // that column — `db::record_memory_use` is its only writer), so this lever
    // needs no new column and no count: the knob just swaps which
    // never-touched predicate is read.
    //
    // tachi#1459: `access_count` observes the search path only; reads through
    // path-listing routes do not increment it. Read as "overlooked", the
    // knob-off predicate is therefore over-inclusive in one direction — a
    // heavily-read kanban or handoff row that search never returns scores as
    // overlooked — and the knob-on predicate is over-inclusive in another,
    // since path listing does not mark use either. Neither arm can distinguish
    // "nobody wanted it" from "nobody could have found it this way".
    let never_used = if recall_config.use_provenance_recency {
        entry.last_use_at.is_none()
    } else {
        entry.access_count == 0
    };
    let overlooked = if never_used && entry.importance > 0.7 {
        0.3
    } else {
        0.0
    };

    // Weighted combination — each component now independently in [0, 1]
    let raw = 0.25 * importance_surprise
        + 0.30 * contradiction_surprise
        + 0.25 * topic_novelty
        + 0.20 * overlooked;

    raw.clamp(0.0, 1.0)
}

/// Weights for the hybrid scoring formula.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridWeights {
    pub semantic: f64,
    pub fts: f64,
    pub symbolic: f64,
    pub decay: f64,
    pub use_rrf: bool,
}

impl Default for HybridWeights {
    fn default() -> Self {
        Self {
            semantic: 0.35,
            fts: 0.25,
            symbolic: 0.20,
            decay: 0.20,
            use_rrf: true,
        }
    }
}

/// Parse an ISO-8601 / RFC-3339 timestamp to epoch milliseconds for tie-break
/// ordering. Unparseable or empty → `i64::MIN` (sorts as the oldest), keeping
/// the order total and deterministic.
///
/// Recall sorts compare parsed *instants*, not raw strings (tachi#718 CP2): the
/// `timestamp` column is plain `TEXT NOT NULL` and only new writes are
/// normalized to UTC+millis `Z` (`db/common.rs`), so a lexical compare
/// mis-orders legacy/mixed rows — `...00:00:00Z` vs `...00:00:00.500Z` (`.` <
/// `Z`) and any `+hh:mm` offset both invert real time. Normalizing the column
/// itself is a larger migration deliberately deferred out of this fix; parsing
/// on read is the bounded fallback. Callers parse once per candidate
/// (decorate-sort-undecorate), never inside the comparator.
pub(crate) fn timestamp_epoch_millis(ts: &str) -> i64 {
    let raw = ts.trim();
    if raw.is_empty() {
        return i64::MIN;
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| raw.parse::<chrono::DateTime<Utc>>())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(i64::MIN)
}

/// Deterministic tie-break comparator for every score-ranked recall sort site
/// (tachi#718). Best element sorts to the front:
/// 1. `score` descending (`total_cmp`, NaN-safe);
/// 2. instant descending — a newer memory wins an exact score tie (recall
///    semantics: recency is the default preference). The instant is epoch
///    millis parsed by [`timestamp_epoch_millis`], not the raw string;
/// 3. `id` ascending — ids are unique, so this is the absolute determinism
///    backstop when score and instant both tie.
///
/// Each element is `(score, epoch_millis, id)`. Routing all sorts through one
/// comparator keeps tie order identical run to run and prevents each site from
/// hand-rolling a divergent key.
pub(crate) fn cmp_recall_rank(a: (f64, i64, &str), b: (f64, i64, &str)) -> std::cmp::Ordering {
    let (a_score, a_ms, a_id) = a;
    let (b_score, b_ms, b_id) = b;
    b_score
        .total_cmp(&a_score)
        .then_with(|| b_ms.cmp(&a_ms))
        .then_with(|| a_id.cmp(b_id))
}

fn rank_map(
    scores: &HashMap<String, f64>,
    entries: &HashMap<String, &MemoryEntry>,
) -> HashMap<String, usize> {
    // Decorate each candidate with its parsed instant once, then sort — the
    // comparator never re-parses (tachi#718 CP2/CP3).
    let mut ranked: Vec<(&String, f64, i64)> = scores
        .iter()
        .map(|(id, score)| {
            let ms = entries
                .get(id)
                .map(|e| timestamp_epoch_millis(&e.timestamp))
                .unwrap_or(i64::MIN);
            (id, *score, ms)
        })
        .collect();
    ranked.sort_by(|a, b| cmp_recall_rank((a.1, a.2, a.0), (b.1, b.2, b.0)));
    ranked
        .into_iter()
        .enumerate()
        .map(|(idx, (id, _, _))| (id.clone(), idx + 1))
        .collect()
}

fn blend_rrf_with_vector_signal(
    id: &str,
    rrf_score: f64,
    vec_scores: &HashMap<String, f64>,
    vec_weight: f64,
) -> f64 {
    let Some(cosine) = vec_scores.get(id).copied().map(normalize) else {
        return rrf_score;
    };

    rrf_score * (1.0 + 0.15 * vec_weight.clamp(0.0, 1.0) * cosine)
}

fn retrieval_rrf_weight(weight: f64, total: f64) -> f64 {
    if !weight.is_finite() || weight <= 0.0 || total <= 0.0 {
        0.0
    } else {
        weight / total
    }
}

/// Merge several scored lists into a single HybridScore per doc-id.
///
/// `vec_scores`, `fts_scores`, `symbolic_scores` are maps from doc-id → normalised score [0,1].
/// `access_times` is a map from doc-id → list of access ages in seconds (for ACT-R BLA).
pub fn hybrid_score(
    entries: &HashMap<String, &MemoryEntry>,
    vec_scores: &HashMap<String, f64>,
    fts_scores: &HashMap<String, f64>,
    symbolic_scores: &HashMap<String, f64>,
    weights: &HybridWeights,
    access_times: &HashMap<String, Vec<f64>>,
) -> HashMap<String, HybridScore> {
    hybrid_score_with_config(
        entries,
        vec_scores,
        fts_scores,
        symbolic_scores,
        weights,
        access_times,
        RecallConfig::get(),
    )
}

pub fn hybrid_score_with_config(
    entries: &HashMap<String, &MemoryEntry>,
    vec_scores: &HashMap<String, f64>,
    fts_scores: &HashMap<String, f64>,
    symbolic_scores: &HashMap<String, f64>,
    weights: &HybridWeights,
    access_times: &HashMap<String, Vec<f64>>,
    recall_config: &RecallConfig,
) -> HashMap<String, HybridScore> {
    hybrid_score_with_policy(
        entries,
        vec_scores,
        fts_scores,
        symbolic_scores,
        weights,
        access_times,
        DecayPolicyContext::new(recall_config, &DEFAULT_DECAY_POLICY),
    )
}

pub fn hybrid_score_with_policy(
    entries: &HashMap<String, &MemoryEntry>,
    vec_scores: &HashMap<String, f64>,
    fts_scores: &HashMap<String, f64>,
    symbolic_scores: &HashMap<String, f64>,
    weights: &HybridWeights,
    access_times: &HashMap<String, Vec<f64>>,
    decay_policy_context: DecayPolicyContext<'_>,
) -> HashMap<String, HybridScore> {
    let all_ids: std::collections::HashSet<&String> = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .chain(symbolic_scores.keys())
        .collect();

    let mut out: HashMap<String, HybridScore> = HashMap::new();
    let vec_ranks = weights.use_rrf.then(|| rank_map(vec_scores, entries));
    let fts_ranks = weights.use_rrf.then(|| rank_map(fts_scores, entries));
    let symbolic_ranks = weights.use_rrf.then(|| rank_map(symbolic_scores, entries));

    for id in all_ids {
        let vs = normalize(*vec_scores.get(id).unwrap_or(&0.0));
        let fs = normalize(*fts_scores.get(id).unwrap_or(&0.0));
        let ss = normalize(*symbolic_scores.get(id).unwrap_or(&0.0));

        // Use ACT-R enhanced decay when access history exists, else fallback
        let ds = entries
            .get(id.as_str())
            .map(|e| {
                let ages = access_times.get(id).map(|v| v.as_slice());
                decay_score_actr_with_policy(
                    e,
                    ages,
                    decay_policy_context.recall_config,
                    decay_policy_context.decay_policy,
                )
            })
            .unwrap_or(0.0);

        let final_score = if weights.use_rrf {
            // Reciprocal Rank Fusion: rewards agreement across channels
            // without overtrusting raw score calibration differences.
            let rrf_k = decay_policy_context.recall_config.rrf_k.max(1.0);
            let retrieval_weight_total =
                (weights.semantic + weights.fts + weights.symbolic).max(0.0);
            let vec_weight = retrieval_rrf_weight(weights.semantic, retrieval_weight_total);
            let fts_weight = retrieval_rrf_weight(weights.fts, retrieval_weight_total);
            let symbolic_weight = retrieval_rrf_weight(weights.symbolic, retrieval_weight_total);
            let vec_part = vec_ranks
                .as_ref()
                .and_then(|ranks| ranks.get(id))
                .map(|rank| vec_weight / (rrf_k + *rank as f64))
                .unwrap_or(0.0);
            let fts_part = fts_ranks
                .as_ref()
                .and_then(|ranks| ranks.get(id))
                .map(|rank| fts_weight / (rrf_k + *rank as f64))
                .unwrap_or(0.0);
            let symbolic_part = symbolic_ranks
                .as_ref()
                .and_then(|ranks| ranks.get(id))
                .map(|rank| symbolic_weight / (rrf_k + *rank as f64))
                .unwrap_or(0.0);
            let rrf_score = vec_part + fts_part + symbolic_part;
            let blended = blend_rrf_with_vector_signal(id, rrf_score, vec_scores, vec_weight);
            // Decay re-injected as a proportional bonus so recency still
            // influences ranking in RRF mode (scaled to the RRF score range).
            blended + weights.decay * ds / rrf_k
        } else {
            weights.semantic * vs + weights.fts * fs + weights.symbolic * ss + weights.decay * ds
        };

        out.insert(
            id.clone(),
            HybridScore {
                vector: vs,
                fts: fs,
                symbolic: ss,
                decay: ds,
                final_score,
            },
        );
    }

    out
}

#[cfg(test)]
mod tests;
