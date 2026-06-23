use super::*;

#[tokio::test]
async fn tachi_search_memory_scope_excludes_wiki_rows() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("plain-memory-row");
            memory.path = "/facts/plain".to_string();
            memory.text = "UniqueBoundaryNeedle belongs in plain memory.".to_string();
            memory.summary = "Plain memory row".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut wiki = make_entry("wiki-row-should-not-appear");
            wiki.path = "/wiki/general/boundary".to_string();
            wiki.text = "UniqueBoundaryNeedle belongs in wiki.".to_string();
            wiki.summary = "Wiki row".to_string();
            wiki.domain = Some("wiki".to_string());
            wiki.metadata = json!({"wiki": true});
            store.upsert(&wiki).map_err(|e| e.to_string())
        })
        .expect("seed boundary entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueBoundaryNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(response.contains("plain-memory-row"));
    assert!(!response.contains("wiki-row-should-not-appear"));
}

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
            cache.source = memory_core::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
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
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        }))
        .await
        .expect("recall-cache scoped search");
    assert!(response.contains("foundry:recall-cache:boundary"));
}

#[tokio::test]
async fn tachi_search_excludes_sft_training_rows_by_default() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-memory-row");
            live.path = "/scratch/sigil/current-kimi-dispatch".to_string();
            live.text = "UniqueSftBoundaryNeedle live Kimi dispatch fix.".to_string();
            live.summary = "Live memory row".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut training = make_entry("sft-training-row");
            training.path = "/sft/v4/strict/engineering/1".to_string();
            training.text = "UniqueSftBoundaryNeedle historical training sample.".to_string();
            training.summary = "SFT training row".to_string();
            training.importance = 1.0;
            store.upsert(&training).map_err(|e| e.to_string())
        })
        .expect("seed sft boundary entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueSftBoundaryNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(response.contains("live-memory-row"));
    assert!(!response.contains("sft-training-row"));
}

#[tokio::test]
async fn tachi_search_scope_sft_opts_into_training_rows() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut live = make_entry("live-memory-row");
            live.path = "/scratch/sigil/current-kimi-dispatch".to_string();
            live.text = "UniqueSftScopeNeedle live Kimi dispatch fix.".to_string();
            live.summary = "Live memory row".to_string();
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut training = make_entry("sft-training-row");
            training.path = "/sft/v4/causal/engineering/1".to_string();
            training.text = "UniqueSftScopeNeedle historical training sample.".to_string();
            training.summary = "SFT training row".to_string();
            store.upsert(&training).map_err(|e| e.to_string())
        })
        .expect("seed sft scope entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueSftScopeNeedle".to_string(),
            scope: "sft".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("sft scoped search");

    assert!(response.contains("sft-training-row"));
    assert!(!response.contains("live-memory-row"));
}
