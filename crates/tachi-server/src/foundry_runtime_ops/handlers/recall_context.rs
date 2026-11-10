use super::super::helpers::path_is_within_prefix;
use super::super::recall::{
    build_prepend_context, build_wiki_context, resolve_recall_scope, value_id, value_path,
    value_relevance, value_topic,
};
use crate::memory_search_ops::{rerank_rows_with_outcome, search_memory_rows, RerankOutcome};
use crate::server_state::MemoryServer;
use crate::tool_params::{RecallContextParams, SearchMemoryParams, WikiReadPlan};
use crate::utils::stable_hash;
use serde_json::{json, Value};
use std::collections::HashSet;

pub(crate) async fn handle_recall_context(
    server: &MemoryServer,
    params: RecallContextParams,
) -> Result<String, String> {
    if params.min_score.is_some_and(|value| !value.is_finite()) {
        return Err("min_score must be finite".to_string());
    }
    if params.query.trim().is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_query",
            "count": 0,
            "results": [],
            "prepend_context": "",
        }))
        .map_err(|e| format!("Failed to serialize recall_context response: {e}"));
    }

    let candidate_multiplier = params.candidate_multiplier.max(1);
    let candidate_top_k = params.top_k.max(1).saturating_mul(candidate_multiplier);
    let recall_scope =
        resolve_recall_scope(params.path_prefix.as_deref(), params.agent_id.as_deref());
    let mut parsed = Vec::<Value>::new();
    let mut seen_ids = HashSet::<String>::new();
    for search_prefix in &recall_scope.search_prefixes {
        let rows = search_memory_rows(
            server,
            SearchMemoryParams {
                query: params.query.clone(),
                query_vec: None,
                top_k: candidate_top_k,
                path_prefix: search_prefix.clone(),
                include_training: false,
                include_archived: false,
                candidates_per_channel: candidate_top_k.max(20),
                mmr_threshold: None,
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                context_symbols: Vec::new(),
                agent_role: params.agent_role.clone(),
                project: params.project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                conversation_scope: None,
                auto_conversation_scope: true,
                multi_evidence_pack: true,
                as_of: None,
                include_superseded: false,
                include_test: false,
                include_metadata: false,
                format: None,
            },
            false,
        )
        .await?;
        for row in rows {
            let id = value_id(&row);
            if !id.is_empty() && !seen_ids.insert(id) {
                continue;
            }
            parsed.push(row);
        }
    }

    let excluded_topics = params
        .exclude_topics
        .iter()
        .map(|topic| topic.trim().to_ascii_lowercase())
        .filter(|topic| !topic.is_empty())
        .collect::<HashSet<_>>();

    let min_score = params.min_score.unwrap_or(0.0);

    let filtered = parsed
        .into_iter()
        .filter(|row| {
            if recall_scope.allowed_prefixes.is_empty() {
                return true;
            }
            let path = value_path(row);
            !path.is_empty()
                && recall_scope
                    .allowed_prefixes
                    .iter()
                    .any(|prefix| path_is_within_prefix(&path, prefix))
        })
        .filter(|row| row.get("l0_rule").and_then(Value::as_bool) != Some(true))
        .filter(|row| {
            let topic = value_topic(row);
            topic.is_empty() || !excluded_topics.contains(&topic.to_ascii_lowercase())
        })
        .filter(|row| value_relevance(row) >= min_score)
        .collect::<Vec<_>>();

    let (reranked, rerank_outcome) =
        rerank_rows_with_outcome(server, &params.query, filtered, params.top_k.max(1)).await;
    if rerank_outcome == RerankOutcome::Fallback {
        tracing::warn!(
            "[recall_context] rerank fail-open: query_hash={} top_k={}",
            stable_hash(&params.query),
            params.top_k.max(1)
        );
    }
    let final_rows = reranked
        .into_iter()
        .filter(|row| value_relevance(row) >= min_score)
        .collect::<Vec<_>>();
    let prepend_context = build_prepend_context(&final_rows);

    // ── Wiki auto-search: enrich recall with wiki knowledge ─────────────
    let (wiki_rows, wiki_context) = if params.include_wiki && !params.query.trim().is_empty() {
        let wiki_top_k = params.wiki_top_k.max(1).min(10);
        let wiki_plan = WikiReadPlan::from_project(params.wiki_project.as_deref())?;
        let result = crate::wiki_ops::search_wiki_rows_for_plan(
            server,
            SearchMemoryParams {
                query: params.query.clone(),
                query_vec: None,
                top_k: wiki_top_k,
                path_prefix: Some("/wiki".to_string()),
                include_training: false,
                include_archived: false,
                candidates_per_channel: (wiki_top_k * 3).max(20),
                mmr_threshold: Some(0.85),
                graph_expand_hops: 1,
                graph_relation_filter: None,
                weights: None,
                context_symbols: Vec::new(),
                agent_role: None,
                project: params.wiki_project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                conversation_scope: None,
                auto_conversation_scope: true,
                multi_evidence_pack: true,
                as_of: None,
                include_superseded: false,
                include_test: false,
                include_metadata: false,
                format: None,
            },
            &wiki_plan,
            None,
            false,
        )
        .await?;
        if result.rows.is_empty() {
            (vec![], String::new())
        } else {
            let ctx = build_wiki_context(&result.rows);
            (result.rows, ctx)
        }
    } else {
        (vec![], String::new())
    };

    // Combine prepend_context blocks
    let combined_context = if wiki_context.is_empty() {
        prepend_context
    } else {
        format!("{}\n{}", prepend_context, wiki_context)
    };

    serde_json::to_string(&json!({
        "status": "completed",
        "count": final_rows.len(),
        "results": final_rows,
        "prepend_context": combined_context,
        "path_prefixes": recall_scope.search_prefixes,
        "allowed_prefixes": recall_scope.allowed_prefixes,
        "warning": recall_scope.warning,
        "wiki_count": wiki_rows.len(),
        "wiki_results": wiki_rows,
    }))
    .map_err(|e| format!("Failed to serialize recall_context response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recall_params(query: &str, min_score: Option<f64>) -> RecallContextParams {
        let mut params: RecallContextParams = serde_json::from_value(json!({
            "query": query,
            "path_prefix": "/facts/finite-recall",
            "include_wiki": false,
        }))
        .unwrap();
        params.min_score = min_score;
        params
    }

    #[tokio::test]
    async fn recall_context_rejects_typed_nonfinite_min_score() {
        let server = crate::tests::make_server();
        for query in ["FiniteRecallNeedle", ""] {
            for min_score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let error = handle_recall_context(&server, recall_params(query, Some(min_score)))
                    .await
                    .expect_err("typed callers must not bypass finite-number validation");
                assert_eq!(error, "min_score must be finite");
            }
        }
    }

    #[tokio::test]
    async fn recall_context_preserves_zero_min_score_and_empty_query() {
        let server = crate::tests::make_server();
        let mut entry = crate::tests::make_entry("finite-recall-memory");
        entry.path = "/facts/finite-recall".to_string();
        entry.text = "FiniteRecallNeedle stored memory".to_string();
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|error| error.to_string()))
            .unwrap();

        let response =
            handle_recall_context(&server, recall_params("FiniteRecallNeedle", Some(0.0)))
                .await
                .unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["status"], "completed");
        assert!(response["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == entry.id));

        for min_score in [None, Some(0.0)] {
            let response = handle_recall_context(&server, recall_params(" ", min_score))
                .await
                .unwrap();
            let response: Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["status"], "skipped");
            assert_eq!(response["reason"], "empty_query");
        }
    }
}
