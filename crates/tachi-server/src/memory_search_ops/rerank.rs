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
use super::search_memory::has_high_confidence_exact_token_top;
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
