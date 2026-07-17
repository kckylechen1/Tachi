use super::*;

#[tokio::test]
async fn tachi_search_excludes_recall_cache_rows_by_default() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("recall-live-memory-row");
            live.path = "/scratch/sigil/current".to_string();
            live.text = "UniqueRecallCacheNeedle belongs in live memory.".to_string();
            live.summary = "Live memory row".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut cache = make_entry("foundry:recall-cache:boundary");
            cache.path = "/scratch/sigil/recall-cache/boundary".to_string();
            cache.text = "UniqueRecallCacheNeedle belongs in recall cache.".to_string();
            cache.summary = "Recall cache row".to_string();
            cache.topic = "recall_rerank_cache".to_string();
            cache.source = memcore::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
            cache.metadata = json!({"recall_rerank_cache": true});
            store.upsert(&cache).map_err(|e| e.to_string())
        })
        .expect("seed recall-cache boundary entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueRecallCacheNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            agent_role: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(response.contains("recall-live-memory-row"));
    assert!(!response.contains("foundry:recall-cache:boundary"));

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "UniqueRecallCacheNeedle".to_string(),
            query_vec: None,
            top_k: 5,
            path_prefix: Some("/scratch/sigil/recall-cache".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            // Assertion below is a substring `.contains()` check on the
            // response, unaffected by the search_memory markdown/JSON
            // default (tachi#1201 k3); left unset intentionally.
            format: None,
        }))
        .await
        .expect("recall-cache scoped search");
    assert!(response.contains("foundry:recall-cache:boundary"));
}
