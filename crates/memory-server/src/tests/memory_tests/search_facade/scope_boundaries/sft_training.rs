use super::*;

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
            context_symbols: Vec::new(),
            agent_role: None,
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
