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
        let (reranked, outcome) =
            crate::foundry_runtime_ops::rerank_rows_with_outcome(server, query, rows, top_k).await;
        let policy = match outcome {
            crate::foundry_runtime_ops::RerankOutcome::Applied => SearchRerankPolicy::Applied,
            crate::foundry_runtime_ops::RerankOutcome::Fallback => {
                eprintln!(
                    "[search_memory] rerank fail-open: query_hash={} top_k={}",
                    stable_hash(query),
                    top_k
                );
                SearchRerankPolicy::Fallback
            }
            crate::foundry_runtime_ops::RerankOutcome::NotNeeded => SearchRerankPolicy::NotNeeded,
        };
        return (reranked, policy);
    }

    rows.truncate(top_k);
    (rows, SearchRerankPolicy::ScoreGapTooWide)
}
