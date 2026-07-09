//! Search rerank policy + the rerank execution that backs it.
//!
//! This module owns the rerank decision layer: it decides *whether* to rerank
//! (policy gate on score gap / exact-token / row count) and, when it decides
//! yes, calls the Voyage rerank model via `server.llm` and merges the model's
//! order with a hybrid-score floor. This logic previously lived in
//! `foundry_runtime_ops::recall`; it was relocated here to cut the
//! `memory_search_ops -> foundry_runtime_ops` dependency edge (#833 rerank
//! seam). `foundry_runtime_ops` now calls *back into* this module instead.
//!
//! The LLM call follows the same direct-`server.llm` pattern as
//! `search_memory/rows.rs` (`server.llm.embed_voyage`); no trait seam is
//! introduced because `MemoryServer` and its `llm` field are `pub(crate)`.

use serde_json::{json, Value};

use super::search_helpers::search_score;
use super::search_memory::has_high_confidence_exact_token_top;
use crate::tool_params::{SearchMemoryParams, MAX_SEARCH_CANDIDATES_PER_CHANNEL};
use crate::utils::stable_hash;
use crate::MemoryServer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchRerankPolicy {
    Disabled,
    NotNeeded,
    SkippedExactToken,
    Applied,
    Fallback,
    ScoreGapTooWide,
}

impl SearchRerankPolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NotNeeded => "not_needed",
            Self::SkippedExactToken => "skipped_exact_token",
            Self::Applied => "applied",
            Self::Fallback => "fallback",
            Self::ScoreGapTooWide => "score_gap_too_wide",
        }
    }
}

pub(crate) fn expand_search_params_for_rerank(params: &mut SearchMemoryParams, final_top_k: usize) {
    if !params.enable_rerank {
        return;
    }
    params.top_k = final_top_k.saturating_mul(3);
    params.candidates_per_channel = params
        .candidates_per_channel
        .max(params.top_k)
        .min(MAX_SEARCH_CANDIDATES_PER_CHANNEL);
}

pub(crate) async fn apply_search_rerank_policy(
    server: &MemoryServer,
    query: &str,
    mut rows: Vec<Value>,
    top_k: usize,
    enable_rerank: bool,
) -> (Vec<Value>, SearchRerankPolicy) {
    if !enable_rerank {
        rows.truncate(top_k);
        return (rows, SearchRerankPolicy::Disabled);
    }

    if rows.len() <= top_k {
        rows.truncate(top_k);
        return (rows, SearchRerankPolicy::NotNeeded);
    }

    if has_high_confidence_exact_token_top(&rows, query) {
        if let Some(obj) = rows.first_mut().and_then(serde_json::Value::as_object_mut) {
            obj.insert("rerank_policy".into(), json!("skipped_exact_token"));
        }
        rows.truncate(top_k);
        return (rows, SearchRerankPolicy::SkippedExactToken);
    }

    if rows.len() >= 3 && search_score(&rows[0]) - search_score(&rows[2]) < 0.15 {
        let (reranked, outcome) = rerank_rows_with_outcome(server, query, rows, top_k).await;
        let policy = match outcome {
            RerankOutcome::Applied => SearchRerankPolicy::Applied,
            RerankOutcome::Fallback => {
                eprintln!(
                    "[search_memory] rerank fail-open: query_hash={} top_k={}",
                    stable_hash(query),
                    top_k
                );
                SearchRerankPolicy::Fallback
            }
            RerankOutcome::NotNeeded => SearchRerankPolicy::NotNeeded,
        };
        return (reranked, policy);
    }

    rows.truncate(top_k);
    (rows, SearchRerankPolicy::ScoreGapTooWide)
}

// ─── Rerank execution (relocated from the foundry recall module) ─────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RerankOutcome {
    Applied,
    Fallback,
    NotNeeded,
}

/// Run the Voyage rerank model over `rows` and merge its ordering with a
/// hybrid-score floor so the top-K always contains the strongest lexical
/// candidates. Returns the reranked rows (truncated to `top_k`) and whether
/// the model's order was applied, fell back, or was not needed.
pub(crate) async fn rerank_rows_with_outcome(
    server: &MemoryServer,
    query: &str,
    rows: Vec<Value>,
    top_k: usize,
) -> (Vec<Value>, RerankOutcome) {
    if rows.len() <= 1 {
        return (
            rows.into_iter().take(top_k).collect(),
            RerankOutcome::NotNeeded,
        );
    }

    let docs = rows.iter().map(build_rerank_document).collect::<Vec<_>>();
    match server.llm.rerank_voyage(query, &docs, top_k).await {
        Ok(order) => {
            let out = merge_rerank_order_with_hybrid_floor(&rows, &order, top_k);
            let outcome = if out.is_empty() {
                RerankOutcome::Fallback
            } else {
                RerankOutcome::Applied
            };
            let rows = if out.is_empty() {
                rows.into_iter().take(top_k).collect()
            } else {
                out
            };
            (rows, outcome)
        }
        Err(err) => {
            tracing::warn!("[rerank] failed, falling back to hybrid ranking: {err}");
            (
                rows.into_iter().take(top_k).collect(),
                RerankOutcome::Fallback,
            )
        }
    }
}

fn build_rerank_document(row: &Value) -> String {
    let text = value_text(row);
    let topic = value_topic(row);
    let keywords = value_string_array(row, "keywords");
    let doc = [text, topic, keywords.join(", ")]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    // Voyage (and similar) reject empty documents with HTTP 400; fall back to a
    // non-empty placeholder so one blank row cannot poison the whole batch (#876).
    if doc.trim().is_empty() {
        "(empty memory entry)".to_string()
    } else {
        doc
    }
}

/// Provisional: minimum fraction of `top_k` hybrid-head items guaranteed to
/// survive rerank. Lower = more promotion room for tail items; raise to protect
/// more head items. Named threshold per AGENTS.md; tunable.
const HYBRID_HEAD_FRACTION: f64 = 0.5;

/// Stamp the relevance fields so the value rendered into agent context matches
/// the actual sort key. `relevance`/`score.final` reflect `blend_score` (the key
/// the output is ordered by); `rerank_score` keeps the raw provider signal for
/// observability when present.
fn apply_blend_relevance(mut row: Value, blend_score: f64, rerank_score: Option<f64>) -> Value {
    let blend = round3(blend_score);
    if let Value::Object(map) = &mut row {
        map.insert("relevance".into(), json!(blend));
        if let Some(raw) = rerank_score {
            map.insert("rerank_score".into(), json!(round3(raw)));
        }
        if let Some(Value::Object(score_map)) = map.get_mut("score") {
            score_map.insert("final".into(), json!(blend));
        }
    }
    row
}

fn merge_rerank_order_with_hybrid_floor(
    rows: &[Value],
    order: &[(usize, f64)],
    top_k: usize,
) -> Vec<Value> {
    let output_len = top_k.min(rows.len());
    if output_len == 0 {
        return Vec::new();
    }

    // rerank_by_index: index -> (rerank_rank, provider_score). Keep first
    // occurrence per index; rerank_rank is the 1-based enumerate position.
    let mut rerank_by_index: std::collections::HashMap<usize, (usize, f64)> =
        std::collections::HashMap::new();
    for (rerank_rank, &(index, score)) in order.iter().enumerate() {
        if index < rows.len() {
            rerank_by_index
                .entry(index)
                .or_insert((rerank_rank + 1, score));
        }
    }
    let missing_rerank_rank = rows.len() + 1;

    // blend_score = reciprocal rank fusion of hybrid rank and rerank rank.
    let blend_score = |index: usize| -> f64 {
        let original_rank = index + 1;
        let rerank_rank = rerank_by_index
            .get(&index)
            .map(|(rank, _)| *rank)
            .unwrap_or(missing_rerank_rank);
        (1.0 / original_rank as f64) + (1.0 / rerank_rank as f64)
    };

    // Seatbelt: a guaranteed prefix of the hybrid head always survives, but its
    // position may shift down if tail items out-score it. head_floor is strictly
    // <= output_len, leaving room for tail promotions.
    let head_floor = ((output_len as f64) * HYBRID_HEAD_FRACTION).round() as usize;
    let head_floor = head_floor.min(output_len);

    // Guaranteed head indices 0..head_floor.
    let mut selected: Vec<usize> = (0..head_floor).collect();

    // Remaining slots filled by the top blend-scoring indices from the pool
    // head_floor..rows.len() (tail + any non-guaranteed head).
    let remaining = output_len.saturating_sub(head_floor);
    if remaining > 0 {
        let mut pool: Vec<usize> = (head_floor..rows.len()).collect();
        pool.sort_by(|&a, &b| {
            blend_score(b)
                .partial_cmp(&blend_score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.cmp(&b))
        });
        for index in pool.into_iter().take(remaining) {
            selected.push(index);
        }
    }

    // Order the selected set by blend_score descending (stable tiebreak: index
    // ascending) for the final output sequence.
    selected.sort_by(|&a, &b| {
        blend_score(b)
            .partial_cmp(&blend_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(&b))
    });

    selected
        .into_iter()
        .map(|index| {
            let rerank_score = rerank_by_index.get(&index).map(|(_, score)| *score);
            apply_blend_relevance(rows[index].clone(), blend_score(index), rerank_score)
        })
        .collect()
}

// ─── value accessors (local copies; originals stay in foundry for shared use) ───

fn value_text(row: &Value) -> String {
    row.get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn value_topic(row: &Value) -> String {
    row.get("topic")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn value_string_array(row: &Value, key: &str) -> Vec<String> {
    row.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── merge_rerank_order_with_hybrid_floor (relocated from foundry tests) ──

    #[test]
    fn rerank_merge_blends_rerank_order_with_hybrid_rank() {
        let rows = (0..6)
            .map(|idx| {
                json!({
                    "id": format!("hybrid-{idx}"),
                    "text": format!("candidate {idx}"),
                    "relevance": 1.0 - (idx as f64 * 0.01),
                    "score": { "final": 1.0 - (idx as f64 * 0.01) }
                })
            })
            .collect::<Vec<_>>();

        let merged = merge_rerank_order_with_hybrid_floor(
            &rows,
            &[(5, 0.99), (4, 0.98), (2, 0.97), (1, 0.96)],
            3,
        );
        let ids = merged
            .iter()
            .map(|row| row["id"].as_str().expect("row id"))
            .collect::<Vec<_>>();

        // head_floor for top_k=3 is 2 -> hybrid-0 and hybrid-1 are guaranteed by
        // the seatbelt; the remaining slot promotes the best tail item (hybrid-5,
        // rerank_rank 1, blend 1.167). Final order is blend_score descending.
        assert_eq!(ids, vec!["hybrid-5", "hybrid-0", "hybrid-1"]);
        // relevance now equals the blend sort key (reciprocal-rank-fusion), not the
        // raw provider signal, so [Score=X.XX] in agent context matches the rank.
        assert_eq!(merged[0]["relevance"], json!(1.167)); // 1/6 + 1/1
        assert_eq!(merged[1]["relevance"], json!(1.143)); // 1/1 + 1/7
        assert_eq!(merged[2]["relevance"], json!(0.75)); //  1/2 + 1/4
                                                         // rerank_score keeps the raw provider signal where the model scored the row.
        assert_eq!(merged[0]["rerank_score"], json!(0.99));
        assert!(merged[1].get("rerank_score").is_none());
        assert_eq!(merged[2]["rerank_score"], json!(0.96));
        assert!(
            ids.contains(&"hybrid-0"),
            "hybrid head should remain available as the no-evict seatbelt"
        );
    }

    #[test]
    fn rerank_merge_allows_tail_promotion_without_dropping_the_hybrid_head() {
        let rows = (0..6)
            .map(|idx| {
                json!({
                    "id": format!("hybrid-{idx}"),
                    "text": format!("candidate {idx}"),
                    "relevance": 1.0 - (idx as f64 * 0.01),
                    "score": { "final": 1.0 - (idx as f64 * 0.01) }
                })
            })
            .collect::<Vec<_>>();

        let merged = merge_rerank_order_with_hybrid_floor(
            &rows,
            &[(5, 0.99), (4, 0.98), (2, 0.97), (1, 0.96)],
            3,
        );
        let ids = merged
            .iter()
            .map(|row| row["id"].as_str().expect("row id"))
            .collect::<Vec<_>>();

        assert!(
            ids.contains(&"hybrid-5"),
            "expanded rerank candidates must be able to promote into top_k: {ids:?}"
        );
        assert!(
            ids.contains(&"hybrid-0"),
            "the hybrid head is the no-evict seatbelt for prior top-k hits: {ids:?}"
        );
    }

    #[test]
    fn rerank_merge_never_evicts_hybrid_head_beyond_head_floor() {
        let rows = (0..6)
            .map(|idx| {
                json!({
                    "id": format!("hybrid-{idx}"),
                    "text": format!("candidate {idx}"),
                    "relevance": 1.0 - (idx as f64 * 0.01),
                    "score": { "final": 1.0 - (idx as f64 * 0.01) }
                })
            })
            .collect::<Vec<_>>();
        // rerank strongly prefers the TAIL — model ranks 5,4,3,2,1,0
        let order = vec![
            (5, 0.99_f64),
            (4, 0.98),
            (3, 0.97),
            (2, 0.96),
            (1, 0.95),
            (0, 0.94),
        ];
        let merged = merge_rerank_order_with_hybrid_floor(&rows, &order, 3);
        let ids: Vec<&str> = merged.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(merged.len(), 3, "output must be exactly top_k: {ids:?}");
        // head_floor for top_k=3 is 2 → hybrid-0 and hybrid-1 MUST survive
        assert!(
            ids.contains(&"hybrid-0"),
            "hybrid rank-1 must survive (head floor): {ids:?}"
        );
        assert!(
            ids.contains(&"hybrid-1"),
            "hybrid rank-2 must survive (head floor): {ids:?}"
        );
    }

    #[test]
    fn rerank_merge_relevance_matches_blend_sort_order() {
        let rows = (0..4)
            .map(|idx| {
                json!({
                    "id": format!("hybrid-{idx}"),
                    "text": format!("candidate {idx}"),
                    "relevance": 1.0 - (idx as f64 * 0.1),
                    "score": { "final": 1.0 - (idx as f64 * 0.1) }
                })
            })
            .collect::<Vec<_>>();
        let order = vec![(3, 0.99_f64)]; // only tail item 3 gets a rerank score
        let merged = merge_rerank_order_with_hybrid_floor(&rows, &order, 2);
        assert_eq!(merged.len(), 2);
        // relevance must be non-increasing down the output (matches sort order)
        let rels: Vec<f64> = merged
            .iter()
            .map(|r| r["relevance"].as_f64().unwrap())
            .collect();
        for w in rels.windows(2) {
            assert!(
                w[0] >= w[1] - 1e-9,
                "relevance must be non-increasing down the output: {rels:?}"
            );
        }
        // promoted tail item hybrid-3 keeps its raw provider score in rerank_score
        let promo = merged.iter().find(|r| r["id"].as_str() == Some("hybrid-3"));
        assert!(
            promo.is_some(),
            "hybrid-3 should promote: {:?}",
            merged.iter().map(|r| r["id"].as_str()).collect::<Vec<_>>()
        );
        if let Some(p) = promo {
            assert_eq!(p["rerank_score"].as_f64().unwrap(), 0.99);
        }
    }

    // ── apply_blend_relevance ──

    #[test]
    fn apply_blend_relevance_stamps_score_fields() {
        let row = json!({"id": "x", "score": {"final": 0.1}});
        let scored = apply_blend_relevance(row, 0.9876, Some(0.8));
        assert_eq!(scored["relevance"], json!(0.988), "blend rounded to 3dp");
        assert_eq!(scored["score"]["final"], json!(0.988), "final = blend");
        assert_eq!(
            scored["rerank_score"],
            json!(0.8),
            "raw provider score kept"
        );
    }

    #[test]
    fn apply_blend_relevance_without_rerank_score_omits_field() {
        let row = json!({"id": "y"});
        let scored = apply_blend_relevance(row, 0.5, None);
        assert_eq!(scored["relevance"], json!(0.5));
        assert!(
            scored.get("rerank_score").is_none(),
            "no rerank_score when None"
        );
    }
}
