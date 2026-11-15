//! Search rerank policy + the rerank execution that backs it.
//!
//! This module owns the rerank decision layer: it decides *whether* to rerank
//! (policy gate on score gap / exact-token / row count) and, when it decides
//! yes, calls the configured rerank provider via `server.llm` and merges the
//! model's order with a hybrid-score floor (pure math in `memcore::search`).
//!
//! History:
//! - Lived in `foundry_runtime_ops::recall`; relocated here to cut the
//!   `memory_search_ops -> foundry_runtime_ops` dependency edge (#876).
//! - Pure RRF + hybrid-floor blend then moved into `memcore` so
//!   downstream portable builds can reuse it without product deps.
//!
//! The LLM call follows the same direct-`server.llm` pattern as
//! `search_memory/rows.rs` (`server.llm.embed_voyage`); no trait seam is
//! introduced because `MemoryServer` and its `llm` field are `pub(crate)`.

use serde_json::{json, Value};

use super::search_helpers::search_score;
use super::search_memory::high_confidence_exact_token_row_indices;
use crate::tool_params::{SearchMemoryParams, MAX_SEARCH_CANDIDATES_PER_CHANNEL};
use crate::utils::stable_hash;
use crate::MemoryServer;

// Portable kernel surface: hybrid-floor RRF blend (no LLM, no Foundry).
use memcore::merge_rerank_order_with_hybrid_floor;

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

/// Describe the post-filter candidates actually offered to the adaptive gate.
/// This is not the size of the underlying vector or lexical candidate pools.
pub(crate) fn search_rerank_diagnostics(rows: &[Value], top_k: usize) -> Value {
    let score_gap = (rows.len() >= 3).then(|| search_score(&rows[0]) - search_score(&rows[2]));
    json!({
        "candidate_count": rows.len(),
        "requested_top_k": top_k,
        "top_three_score_gap": score_gap,
    })
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

    if rows.len() < 2 {
        rows.truncate(top_k);
        return (rows, SearchRerankPolicy::NotNeeded);
    }

    match rerank_gate_decision(
        &rows,
        &high_confidence_exact_token_row_indices(&rows, query),
    ) {
        RerankGate::SkipExactToken => {
            if let Some(obj) = rows.first_mut().and_then(serde_json::Value::as_object_mut) {
                obj.insert("rerank_policy".into(), json!("skipped_exact_token"));
            }
            rows.truncate(top_k);
            return (rows, SearchRerankPolicy::SkippedExactToken);
        }
        RerankGate::Throttled => {
            rows.truncate(top_k);
            return (rows, SearchRerankPolicy::ScoreGapTooWide);
        }
        RerankGate::Proceed => {}
    }

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
    (reranked, policy)
}

/// Pure rerank gate decision (#2305 finding 2). Kept free of I/O so the
/// skip/throttle/proceed contract is unit-testable against synthetic rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RerankGate {
    /// The first row is a high-confidence exact-token leader with no close
    /// high-confidence exact competitor: keep the skip.
    SkipExactToken,
    /// The top two scores are clearly apart: keep the cost/confidence throttle.
    Throttled,
    /// Ambiguous at the top (or a close exact-token competitor pair): the
    /// model should order the candidates.
    Proceed,
}

pub(crate) fn rerank_gate_decision(rows: &[Value], exact_hc_indices: &[usize]) -> RerankGate {
    // Ambiguity is the gap between the two largest scores on a sorted score
    // view — the order WITHIN top_k is caller-visible, so "every candidate
    // fits" is not "no need to rerank", but the display array itself may be
    // pack-reordered and is not score-sorted, so rows[0]/rows[1] positions
    // cannot measure it.
    let mut score_view: Vec<f64> = rows.iter().map(|row| search_score(row)).collect();
    score_view.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let ambiguous = score_view.len() >= 2 && score_view[0] - score_view[1] < 0.15;

    // Exact-token protection, keyed on the FIRST row (the pre-#2305
    // contract): a high-confidence exact-token leader keeps the skip. The
    // skip is released ONLY when another high-confidence exact-token
    // candidate sits within the ambiguity band OF THE LEADER — ambiguity
    // among non-exact rows must not lift the leader's protection, and a
    // lone exact candidate in the back must not swallow a useful rerank.
    if exact_hc_indices.first() == Some(&0) {
        let leader = search_score(&rows[0]);
        let exact_competitor_close = exact_hc_indices
            .iter()
            .skip(1)
            .any(|&index| (leader - search_score(&rows[index])).abs() < 0.15);
        if !exact_competitor_close {
            return RerankGate::SkipExactToken;
        }
    }

    if !ambiguous {
        return RerankGate::Throttled;
    }
    RerankGate::Proceed
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn row(final_score: f64) -> Value {
        json!({"id": final_score.to_string(), "score": {"final": final_score}})
    }

    #[test]
    fn exact_leader_with_distant_exact_tail_keeps_the_skip() {
        // #2305 review counterexample: ambiguity between the exact leader
        // (1.0) and a NON-exact row (0.98) must not release the skip when
        // the only other exact row is far away (0.40).
        let rows = [row(1.0), row(0.98), row(0.40)];
        assert_eq!(
            rerank_gate_decision(&rows, &[0, 2]),
            RerankGate::SkipExactToken
        );
    }

    #[test]
    fn lone_exact_candidate_in_the_back_does_not_swallow_rerank() {
        // #2305 review counterexample: only the LAST row is high-confidence
        // exact. The first row is not the exact leader, so the old rerank
        // path must proceed (previously this shape wrongly skipped and
        // dropped the exact row).
        let rows = [row(1.0), row(0.97), row(0.94)];
        assert_eq!(rerank_gate_decision(&rows, &[2]), RerankGate::Proceed);
    }

    #[test]
    fn close_exact_pair_releases_the_skip() {
        let rows = [row(0.571), row(0.571), row(0.2)];
        assert_eq!(rerank_gate_decision(&rows, &[0, 1]), RerankGate::Proceed);
    }

    #[test]
    fn clear_leader_keeps_the_throttle() {
        let rows = [row(1.0), row(0.6), row(0.55)];
        assert_eq!(rerank_gate_decision(&rows, &[]), RerankGate::Throttled);
    }

    #[test]
    fn near_tie_without_exact_rows_proceeds() {
        let rows = [row(0.9), row(0.88), row(0.5)];
        assert_eq!(rerank_gate_decision(&rows, &[]), RerankGate::Proceed);
    }
}

// ─── Rerank execution (relocated from the foundry recall module) ─────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RerankOutcome {
    Applied,
    Fallback,
    NotNeeded,
}

/// Run the configured rerank provider over `rows` and merge its ordering with a
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
    match server.llm.rerank(query, &docs, top_k).await {
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
            // Runtime provider errors keep graceful hybrid fallback, but the
            // response must carry a degraded marker (config errors fail at
            // LlmClient construction and never reach this path).
            tracing::warn!("[rerank] failed, falling back to hybrid ranking: {err}");
            let mut fallback_rows: Vec<Value> = rows.into_iter().take(top_k).collect();
            // Rides #926's recall_degradation plumbing (lexical_only wins if both apply).
            super::attach_rerank_fallback_degraded(&mut fallback_rows, &err);
            (fallback_rows, RerankOutcome::Fallback)
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

// Value accessors for document assembly (product JSON row shape). Same helpers
// still exist in foundry for prepend/wiki context; not shared yet.
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
