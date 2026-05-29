// scorer.rs — Pure-Rust hybrid scoring engine (no I/O, no SQLite)
//
// Replaces JS `scorer.ts` (cosineSimilarity / hybridScore / rankHybrid)
// and Python `store.py:hybrid_search` weighting logic.

use crate::types::{HybridScore, MemoryEntry};
use chrono::{NaiveDate, Utc};
use std::collections::{HashMap, HashSet};

// Half-life for the decay function: 30 days (ACT-R inspired, from Nowledge Mem)
const HALF_LIFE_DAYS: f64 = 30.0;

/// Normalise an f64 to [0, 1].
#[inline]
pub fn normalize(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Cosine similarity between two equal-length f32 slices.
/// Returns 0.0 if either vector is zero-magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len(), "vector dimension mismatch");
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
/// where `recency = exp(-0.693 × age_days / HALF_LIFE_DAYS)`
pub fn decay_score(entry: &MemoryEntry) -> f64 {
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
                .unwrap_or(now)
        });
    let age_days = (now - reference).num_seconds().max(0) as f64 / 86_400.0;

    let recency = (-0.693 * age_days / HALF_LIFE_DAYS).exp();
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
    match access_ages {
        Some(ages) if !ages.is_empty() => {
            let bla = base_level_activation(ages, 0.5);
            // Normalize to [0, 1] range: BLA typically ranges from -5 to +5
            let normalized = (bla + 5.0) / 10.0;
            normalized
                .clamp(0.0, 1.0)
                .max(decay_score(entry))
                .max(entry.importance * 0.3)
        }
        _ => decay_score(entry),
    }
}

/// Local PageRank on a subgraph of MemoryEdges.
/// Returns a map from node_id → PageRank score (normalized to [0, 1]).
/// Uses 5 iterations with damping factor d=0.85.
/// Nodes with more incoming edges from important nodes rank higher.
pub fn local_pagerank(edges: &[crate::types::MemoryEdge], damping: f64) -> HashMap<String, f64> {
    use std::collections::HashSet;

    // Collect all node IDs
    let mut nodes: HashSet<String> = HashSet::new();
    for edge in edges {
        nodes.insert(edge.source_id.clone());
        nodes.insert(edge.target_id.clone());
    }

    if nodes.is_empty() {
        return HashMap::new();
    }

    let n = nodes.len() as f64;
    let base = (1.0 - damping) / n;

    // Build adjacency: source → list of targets
    // Exclude 'contradicts' edges — contradictions indicate conflict, not authority
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        if edge.relation != "contradicts" {
            outgoing
                .entry(edge.source_id.as_str())
                .or_default()
                .push(edge.target_id.as_str());
        }
    }

    // Initialize scores uniformly
    let mut scores: HashMap<String, f64> = nodes.iter().map(|id| (id.clone(), 1.0 / n)).collect();

    // 5 iterations of PageRank
    for _ in 0..5 {
        let mut new_scores: HashMap<String, f64> =
            nodes.iter().map(|id| (id.clone(), base)).collect();

        for (source, targets) in &outgoing {
            if targets.is_empty() {
                continue;
            }
            let source_score = scores.get(*source).copied().unwrap_or(0.0);
            let share = source_score / targets.len() as f64;
            for target in targets {
                if let Some(s) = new_scores.get_mut(*target) {
                    *s += damping * share;
                }
            }
        }

        scores = new_scores;
    }

    // Normalize to [0, 1]
    let max_score = scores.values().cloned().fold(0.0_f64, f64::max);
    if max_score > 0.0 {
        for score in scores.values_mut() {
            *score /= max_score;
        }
    }

    scores
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

fn normalized_rank_score(rank: usize, total: usize) -> f64 {
    if total <= 1 {
        return 1.0;
    }
    1.0 - ((rank.saturating_sub(1)) as f64 / (total.saturating_sub(1)) as f64)
}

fn blend_rrf_with_vector_signal(
    id: &str,
    rrf_score: f64,
    vec_scores: &HashMap<String, f64>,
    vec_ranks: Option<&HashMap<String, usize>>,
) -> f64 {
    let Some(cosine) = vec_scores.get(id).copied().map(normalize) else {
        return rrf_score;
    };
    let Some(rank_score) = vec_ranks
        .and_then(|ranks| ranks.get(id))
        .map(|rank| normalized_rank_score(*rank, vec_scores.len()))
    else {
        return rrf_score;
    };

    let improvement = (cosine - rank_score).max(0.0);
    rrf_score * (1.0 + 0.3 * improvement)
}

pub fn graph_relation_activation_weight(relation: &str) -> f64 {
    match relation {
        "supports" => 0.90,
        "elaborates" => 0.85,
        "causes" | "fixed_by" => 0.80,
        "reinforces" => 0.75,
        "follows" | "references" | "distilled_from" | "derived_from" => 0.70,
        "similar_to" | "related_to" | "merge_hint" => 0.55,
        "supersedes" => 0.40,
        "contradicts" | "rejected_because" => 0.30,
        _ => 0.50,
    }
}

pub fn graph_spreading_activation(
    seed_ids: &[String],
    edges: &[crate::types::MemoryEdge],
    max_hops: u32,
    decay: f64,
) -> HashMap<String, f64> {
    let capped_hops = max_hops.min(4);
    let seed_weights = seed_ids
        .iter()
        .map(|id| (id.clone(), 1.0))
        .collect::<HashMap<_, _>>();
    graph_spreading_activation_with_seed_weights(&seed_weights, edges, capped_hops, decay)
}

pub fn graph_spreading_activation_with_seed_weights(
    seed_weights: &HashMap<String, f64>,
    edges: &[crate::types::MemoryEdge],
    max_hops: u32,
    decay: f64,
) -> HashMap<String, f64> {
    if seed_weights.is_empty() || max_hops == 0 {
        return HashMap::new();
    }

    let seeds: HashSet<&String> = seed_weights.keys().collect();
    let mut activation: HashMap<String, f64> = seed_weights
        .iter()
        .filter_map(|(id, weight)| {
            let weight = if weight.is_finite() { *weight } else { 0.0 }.clamp(0.0, 1.0);
            (weight > 0.0).then(|| (id.clone(), weight))
        })
        .collect();
    if activation.is_empty() {
        return HashMap::new();
    }
    let mut frontier = activation.clone();

    for _ in 0..max_hops {
        if frontier.is_empty() {
            break;
        }
        let mut propagated_by_target = HashMap::<String, f64>::new();
        for edge in edges {
            for (source, target) in [
                (&edge.source_id, &edge.target_id),
                (&edge.target_id, &edge.source_id),
            ] {
                let Some(parent_activation) = frontier.get(source).copied() else {
                    continue;
                };
                if seeds.contains(target) {
                    continue;
                }
                let propagated = parent_activation
                    * edge.weight.clamp(0.0, 1.0)
                    * decay
                    * graph_relation_activation_weight(&edge.relation);
                if propagated <= 0.0 {
                    continue;
                }
                propagated_by_target
                    .entry(target.clone())
                    .and_modify(|acc| *acc = 1.0 - (1.0 - *acc) * (1.0 - propagated))
                    .or_insert(propagated);
            }
        }

        frontier = HashMap::new();
        for (id, propagated) in propagated_by_target {
            let propagated = propagated.clamp(0.0, 1.0);
            if propagated <= 0.0 {
                continue;
            }
            let current = activation.get(&id).copied().unwrap_or(0.0);
            // Converging graph paths should reinforce each other without letting
            // dense local clusters exceed a normalized activation ceiling.
            let combined = 1.0 - (1.0 - current) * (1.0 - propagated);
            if combined > current {
                activation.insert(id.clone(), combined);
                frontier.insert(id, propagated);
            }
        }
    }

    activation.retain(|id, _| !seeds.contains(id));
    activation
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
                decay_score_actr(e, ages)
            })
            .unwrap_or(0.0);

        let final_score = if weights.use_rrf {
            // Reciprocal Rank Fusion: rewards agreement across channels
            // without overtrusting raw score calibration differences.
            let rrf_k = 60.0;
            let vec_part = vec_ranks
                .as_ref()
                .and_then(|ranks| ranks.get(id))
                .map(|rank| 1.0 / (rrf_k + *rank as f64))
                .unwrap_or(0.0);
            let fts_part = fts_ranks
                .as_ref()
                .and_then(|ranks| ranks.get(id))
                .map(|rank| 1.0 / (rrf_k + *rank as f64))
                .unwrap_or(0.0);
            let symbolic_part = symbolic_ranks
                .as_ref()
                .and_then(|ranks| ranks.get(id))
                .map(|rank| 0.5 / (rrf_k + *rank as f64))
                .unwrap_or(0.0);
            let rrf_score = vec_part + fts_part + symbolic_part;
            let blended =
                blend_rrf_with_vector_signal(id, rrf_score, vec_scores, vec_ranks.as_ref());
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

/// Simple tokeniser for symbolic (bag-of-words) scoring.
/// - Latin/ASCII: splits on non-alphanumeric, filters tokens < 2 chars.
/// - CJK (Chinese/Japanese/Korean): emits each character as an individual token.
pub fn tokenize(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in lower.chars() {
        if is_cjk(ch) {
            // Flush any pending ASCII token
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            // Emit each CJK character as its own token
            tokens.push(ch.to_string());
        } else if ch.is_alphanumeric() {
            current.push(ch);
        } else {
            // Separator: flush pending ASCII token
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    // Flush trailing
    if current.len() >= 2 {
        tokens.push(current);
    }
    tokens
}

// Re-use is_cjk from noise module (single source of truth)
use crate::noise::is_cjk;
use regex::Regex;
use std::sync::OnceLock;

/// Shared compiled regex for A-share 6-digit stock codes.
static STOCK_CODE_RE: OnceLock<Regex> = OnceLock::new();
fn stock_code_re() -> &'static Regex {
    STOCK_CODE_RE.get_or_init(|| Regex::new(r"\b\d{6}\b").unwrap())
}

/// Compute a normalised token-recall score [0, 1].
/// Measures what fraction of query tokens appear in the entry's text/keywords/entities.
pub fn symbolic_score(
    query: &str,
    entry_text: &str,
    keywords: &[String],
    entities: &[String],
) -> f64 {
    let query_tokens: HashSet<String> = tokenize(query).into_iter().collect();
    if query_tokens.is_empty() {
        return 0.0;
    }

    let mut text_tokens: HashSet<String> = tokenize(entry_text).into_iter().collect();
    for kw in keywords {
        text_tokens.extend(tokenize(kw));
    }
    for ent in entities {
        let trimmed = ent.trim();
        if trimmed.is_empty() {
            continue;
        }
        text_tokens.extend(tokenize(trimmed));
        text_tokens.insert(trimmed.to_ascii_lowercase());
    }

    let overlap = query_tokens.intersection(&text_tokens).count();
    let union_size = query_tokens.union(&text_tokens).count().max(1);
    (overlap as f64) / (union_size as f64)
}

/// Extract A-share style 6-digit stock codes from a query.
pub fn extract_stock_codes(query: &str) -> Vec<String> {
    let re = stock_code_re();
    re.find_iter(query)
        .map(|m| m.as_str().to_string())
        .collect()
}

pub fn entry_has_stock_code(entry: &MemoryEntry, code: &str) -> bool {
    let code = code.trim();
    if code.is_empty() {
        return false;
    }
    entry.entities.iter().any(|e| e.trim() == code)
        || entry.keywords.iter().any(|k| k.contains(code))
        || entry.text.contains(code)
        || entry.summary.contains(code)
        || entry.path.contains(code)
}

/// Strong multiplier for exact ticker / trading-term precision matches.
///
/// Rationale: A-share 6-digit codes (e.g. "688981") are extremely common
/// numeric strings. Without boosting, FTS/symbolic channels dilute exact
/// matches across thousands of unrelated entries. The 12.0x factor ensures
/// an exact ticker match dominates hybrid ranking.
///
/// Rationale: A-share 6-digit codes (e.g. "688981") are extremely common
/// numeric strings. Without boosting, FTS/symbolic channels dilute exact
/// matches across thousands of unrelated entries. The 12.0x factor ensures
/// an exact ticker match dominates hybrid ranking.
///
/// When `use_rrf` is false (raw weighted-sum mode), the multiplier is
/// clamped to [1.0, 3.0] so it amplifies rather than overwhelms.
const TICKER_EXACT_MATCH_BOOST: f64 = 12.0;
const IRON_RULE_BOOST: f64 = 5.0;
const STOP_LOSS_BOOST: f64 = 4.0;
pub fn precision_query_multiplier(query: &str, entry: &MemoryEntry) -> f64 {
    for code in extract_stock_codes(query) {
        if entry_has_stock_code(entry, &code) {
            return TICKER_EXACT_MATCH_BOOST;
        }
    }

    let q = query.to_ascii_lowercase();
    let path = entry.path.to_ascii_lowercase();

    let mut mult: f64 = 1.0;
    let needs_bundle = ((q.contains("iron") && q.contains("rule")) || q.contains("iron_rules"))
        || q.contains("stop loss") || q.contains("stop-loss") || query.contains("止损");

    if !needs_bundle {
        // Fast path: query doesn't contain any precision terms, skip expensive bundle construction
        return mult;
    }

    let bundle = format!(
        "{} {} {} {} {}",
        entry.text.to_ascii_lowercase(),
        entry.summary.to_ascii_lowercase(),
        entry.keywords.join(" ").to_ascii_lowercase(),
        entry.entities.join(" ").to_ascii_lowercase(),
        entry.topic.to_ascii_lowercase(),
    );

    if ((q.contains("iron") && q.contains("rule")) || q.contains("iron_rules"))
        && (path.contains("iron_rule")
            || bundle.contains("iron rule")
            || bundle.contains("iron_rules")
            || bundle.contains("iron rules"))
    {
        mult = mult.max(IRON_RULE_BOOST);
    }
    if (q.contains("stop loss") || q.contains("stop-loss") || query.contains("止损"))
        && (bundle.contains("stop loss")
            || bundle.contains("止损")
            || path.contains("iron_rule")
            || path.contains("principles"))
    {
        mult = mult.max(STOP_LOSS_BOOST);
    }
    mult
}

/// Deterministic ticker/entity hints from memory text (no LLM).
pub fn heuristic_metadata_from_text(text: &str) -> (Vec<String>, Vec<String>) {
    let re = stock_code_re();
    let mut entities = Vec::new();
    let mut keywords = Vec::new();
    for m in re.find_iter(text) {
        let code = m.as_str().to_string();
        if !entities.iter().any(|e| e == &code) {
            entities.push(code.clone());
            keywords.push(format!("ticker:{code}"));
        }
    }
    (entities, keywords)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identity() {
        let v = vec![1.0_f32, 0.0, 0.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cosine_orthogonal() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-9);
    }

    #[test]
    fn symbolic_exact_match() {
        let score = symbolic_score("hello world", "hello world", &[], &[]);
        assert!(score > 0.9, "score={score}");
    }

    #[test]
    fn symbolic_uses_entities_for_stock_codes() {
        let entities = vec!["688981".to_string()];
        let score = symbolic_score("688981", "无关正文", &[], &entities);
        assert!(score >= 0.99, "score={score}");
    }

    #[test]
    fn precision_multiplier_for_exact_ticker() {
        use chrono::Utc;
        let mut entry = crate::types::MemoryEntry {
            id: "t".into(),
            path: "/trading/journal".into(),
            summary: "journal".into(),
            text: "trade note".into(),
            importance: 0.7,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec!["688981".into()],
            location: String::new(),
            source: "manual".into(),
            scope: "project".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: serde_json::json!({}),
            retention_policy: None,
            domain: None,
            vector: None,
        };
        assert!(precision_query_multiplier("688981 止损", &entry) >= 10.0);
        entry.entities.clear();
        assert!(precision_query_multiplier("688981 止损", &entry) <= 1.0);
    }

    #[test]
    fn decay_never_accessed() {
        use chrono::Duration;
        let mut entry = crate::types::MemoryEntry {
            id: "test".into(),
            path: "/".into(),
            summary: "".into(),
            text: "".into(),
            importance: 0.7,
            timestamp: (Utc::now() - Duration::days(60)).to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "".into(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: serde_json::Value::Object(Default::default()),
            vector: None,
            retention_policy: None,
            domain: None,
        };
        let s = decay_score(&entry);
        // 60-day old, no access → recency ~ exp(-0.693*2) ≈ 0.25, floor=0.7*0.3=0.21 → ~0.25
        assert!(s > 0.1 && s < 0.5, "unexpected decay={s}");

        // With importance floor
        entry.importance = 1.0;
        let s2 = decay_score(&entry);
        assert!(s2 >= 0.3, "importance floor violated: {s2}");
    }

    #[test]
    fn actr_access_history_uses_day_scale() {
        use chrono::Duration;
        let entry = crate::types::MemoryEntry {
            id: "test".into(),
            path: "/".into(),
            summary: "".into(),
            text: "".into(),
            importance: 0.7,
            timestamp: (Utc::now() - Duration::days(60)).to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "".into(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: serde_json::Value::Object(Default::default()),
            vector: None,
            retention_policy: None,
            domain: None,
        };

        let never = decay_score_actr(&entry, None);
        let old = decay_score_actr(&entry, Some(&[60.0 * 86_400.0]));
        let recent = decay_score_actr(&entry, Some(&[3_600.0, 7_200.0]));

        assert!(old >= never, "old={old}, never={never}");
        assert!(recent > old, "recent={recent}, old={old}");
    }

    #[test]
    fn rrf_blend_rewards_absolute_vector_similarity_without_penalizing_missing_vector() {
        let vec_scores = HashMap::from([
            ("a".to_string(), 0.99),
            ("b".to_string(), 0.98),
            ("c".to_string(), 0.97),
        ]);
        let ranks = rank_map(&vec_scores);
        let base = 0.02;

        let blended =
            blend_rrf_with_vector_signal(&"c".to_string(), base, &vec_scores, Some(&ranks));
        assert!(blended > base, "blended={blended}, base={base}");

        let missing =
            blend_rrf_with_vector_signal(&"x".to_string(), base, &vec_scores, Some(&ranks));
        assert_eq!(missing, base);
    }

    #[test]
    fn graph_spreading_activation_decays_by_hop_and_relation_type() {
        use crate::types::MemoryEdge;

        let seeds = vec!["a".to_string()];
        let edges = vec![
            MemoryEdge {
                source_id: "a".to_string(),
                target_id: "b".to_string(),
                relation: "supports".to_string(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
            MemoryEdge {
                source_id: "b".to_string(),
                target_id: "c".to_string(),
                relation: "causes".to_string(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
            MemoryEdge {
                source_id: "a".to_string(),
                target_id: "d".to_string(),
                relation: "contradicts".to_string(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        ];

        let activation = graph_spreading_activation(&seeds, &edges, 2, 0.5);
        assert!(!activation.contains_key("a"));
        assert!(activation["b"] > activation["c"]);
        assert!(activation["b"] > activation["d"]);
        assert!(activation["c"] > 0.0);
    }

    #[test]
    fn graph_spreading_activation_uses_weighted_seeds_and_converging_paths() {
        use crate::types::MemoryEdge;

        let mut seed_weights = HashMap::new();
        seed_weights.insert("strong".to_string(), 1.0);
        seed_weights.insert("weak".to_string(), 0.25);
        let edges = vec![
            MemoryEdge {
                source_id: "strong".to_string(),
                target_id: "shared".to_string(),
                relation: "supports".to_string(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
            MemoryEdge {
                source_id: "weak".to_string(),
                target_id: "shared".to_string(),
                relation: "supports".to_string(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
            MemoryEdge {
                source_id: "weak".to_string(),
                target_id: "weak-only".to_string(),
                relation: "supports".to_string(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        ];

        let activation =
            graph_spreading_activation_with_seed_weights(&seed_weights, &edges, 1, 0.5);
        assert!(!activation.contains_key("strong"));
        assert!(!activation.contains_key("weak"));
        assert!(activation["shared"] > activation["weak-only"]);
        assert!(activation["shared"] > 0.45);
    }
}
