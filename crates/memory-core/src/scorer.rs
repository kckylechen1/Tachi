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
pub(crate) use text::generic_precision_multiplier_impl_with_config;
pub use text::{
    entry_has_exact_query_token, generic_precision_multiplier, is_id_like_exact_query,
    symbolic_score, tokenize, PrecisionMatcher,
};

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
    let now = Utc::now();
    let reference = entry
        .last_access
        .as_ref()
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
    let frequency = (1.0 + entry.access_count as f64).log10();
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
    let d = tier_actr_d(&entry.tier);
    match access_ages {
        Some(ages) if !ages.is_empty() => {
            let bla = base_level_activation(ages, d);
            // Normalize to [0, 1] range: BLA typically ranges from -5 to +5
            let normalized = (bla + 5.0) / 10.0;
            normalized
                .clamp(0.0, 1.0)
                .max(decay_score_with_config(entry, recall_config))
                .max(entry.importance * 0.3)
        }
        _ => decay_score_with_config(entry, recall_config),
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

    // Component 4: Low-access high-importance = overlooked valuable memory
    let overlooked = if entry.access_count == 0 && entry.importance > 0.7 {
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

fn rank_map(scores: &HashMap<String, f64>) -> HashMap<String, usize> {
    let mut ranked = scores.iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.total_cmp(a.1));
    ranked
        .into_iter()
        .enumerate()
        .map(|(idx, (id, _))| (id.clone(), idx + 1))
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
    let all_ids: std::collections::HashSet<&String> = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .chain(symbolic_scores.keys())
        .collect();

    let mut out: HashMap<String, HybridScore> = HashMap::new();
    let vec_ranks = weights.use_rrf.then(|| rank_map(vec_scores));
    let fts_ranks = weights.use_rrf.then(|| rank_map(fts_scores));
    let symbolic_ranks = weights.use_rrf.then(|| rank_map(symbolic_scores));

    for id in all_ids {
        let vs = normalize(*vec_scores.get(id).unwrap_or(&0.0));
        let fs = normalize(*fts_scores.get(id).unwrap_or(&0.0));
        let ss = normalize(*symbolic_scores.get(id).unwrap_or(&0.0));

        // Use ACT-R enhanced decay when access history exists, else fallback
        let ds = entries
            .get(id.as_str())
            .map(|e| {
                let ages = access_times.get(id).map(|v| v.as_slice());
                decay_score_actr_with_config(e, ages, recall_config)
            })
            .unwrap_or(0.0);

        let final_score = if weights.use_rrf {
            // Reciprocal Rank Fusion: rewards agreement across channels
            // without overtrusting raw score calibration differences.
            let rrf_k = 60.0;
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
