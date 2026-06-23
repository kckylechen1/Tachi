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
async fn search_memory_keeps_exact_token_top_when_rerank_enabled() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut alpha = make_entry("recall-probe-alpha-20260607");
            alpha.path = "/scratch/tachi/recall-probe-alpha-20260607".to_string();
            alpha.summary = "Alpha recall probe".to_string();
            alpha.text =
                "RECALL_PROBE_ALPHA_20260607 clean-cli bridge dry-run force-delete subcommands"
                    .to_string();
            alpha.keywords = vec![
                "recall-probe".to_string(),
                "clean-cli".to_string(),
                "dry-run".to_string(),
            ];
            store.upsert(&alpha).map_err(|e| e.to_string())?;

            let mut beta = make_entry("recall-probe-beta-20260607");
            beta.path = "/scratch/tachi/recall-probe-beta-20260607".to_string();
            beta.summary = "Beta recall probe".to_string();
            beta.text =
                "RECALL_PROBE_BETA_20260607 cleanup defaults preview before deletion".to_string();
            beta.keywords = vec!["recall-probe".to_string(), "cleanup".to_string()];
            store.upsert(&beta).map_err(|e| e.to_string())?;

            let mut delta = make_entry("recall-probe-delta-20260607");
            delta.path = "/scratch/tachi/recall-probe-delta-20260607".to_string();
            delta.summary = "Delta recall probe".to_string();
            delta.text =
                "RECALL_PROBE_DELTA_20260607 profile routing requested_profile tool_profile"
                    .to_string();
            delta.keywords = vec!["recall-probe".to_string(), "profile".to_string()];
            store.upsert(&delta).map_err(|e| e.to_string())
        })
        .expect("seed recall probe entries");

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "RECALL_PROBE_ALPHA_20260607".to_string(),
            query_vec: None,
            top_k: 1,
            path_prefix: None,
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
            enable_rerank: true,
            as_of: None,
            include_metadata: false,
        }))
        .await
        .expect("exact probe search should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");

    assert_eq!(rows.len(), 1, "rerank gate should still honor top_k");
    assert_eq!(rows[0]["id"], json!("recall-probe-alpha-20260607"));
    assert_eq!(rows[0]["match_type"], json!("exact_token"));
    assert_eq!(rows[0]["rerank_policy"], json!("skipped_exact_token"));
}

#[tokio::test]
async fn tachi_search_surfaces_referenced_files() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("row-with-files");
            memory.path = "/facts/spec-pointer".to_string();
            memory.text = "UniqueFilesNeedle references a spec.".to_string();
            memory.summary = "Row with referenced files".to_string();
            memory.metadata = json!({
                "files": ["docs/SPEC.md", "crates/memory-server/src/lib.rs"]
            });
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut bare = make_entry("row-without-files");
            bare.path = "/facts/no-pointer".to_string();
            bare.text = "UniqueFilesNeedle without any files.".to_string();
            bare.summary = "Row without files".to_string();
            store.upsert(&bare).map_err(|e| e.to_string())
        })
        .expect("seed referenced-files entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueFilesNeedle".to_string(),
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

    assert!(
        response.contains("📎"),
        "expected referenced-files marker in: {response}"
    );
    assert!(
        response.contains("docs/SPEC.md"),
        "expected referenced file path in: {response}"
    );
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

#[tokio::test]
async fn find_similar_memory_excludes_sft_training_rows_by_default() {
    let server = make_server();
    if !server.global_vec_available {
        return;
    }
    let mut query_vec = vec![0.0; 1024];
    query_vec[0] = 1.0;

    server
        .with_global_store(|store| {
            let mut live = make_entry("similar-live-memory-row");
            live.path = "/scratch/sigil/current-similar".to_string();
            live.text = "Similar vector live memory.".to_string();
            live.summary = "Similar live memory".to_string();
            live.vector = Some(query_vec.clone());
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut training = make_entry("similar-sft-training-row");
            training.path = "/scratch/sigil/training-marked".to_string();
            training.text = "Similar vector historical training sample.".to_string();
            training.summary = "Similar training memory".to_string();
            training.source = "sft_seed".to_string();
            training.metadata = json!({"training_sample": true});
            training.vector = Some(query_vec.clone());
            store.upsert(&training).map_err(|e| e.to_string())
        })
        .expect("seed similar sft boundary entries");

    let response = server
        .find_similar_memory(Parameters(FindSimilarMemoryParams {
            query_vec: query_vec.clone(),
            top_k: 5,
            path_prefix: None,
            include_archived: false,
            include_training: false,
            candidates_per_channel: 20,
        }))
        .await
        .expect("find similar should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("similar JSON");
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("similar-live-memory-row")),
        "live row should remain visible: {rows:#?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row["id"] == json!("similar-sft-training-row")),
        "training row leaked through default find_similar_memory: {rows:#?}"
    );

    let response = server
        .find_similar_memory(Parameters(FindSimilarMemoryParams {
            query_vec,
            top_k: 5,
            path_prefix: None,
            include_archived: false,
            include_training: true,
            candidates_per_channel: 20,
        }))
        .await
        .expect("find similar training opt-in should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("similar JSON");
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("similar-sft-training-row")),
        "training opt-in should surface training row: {rows:#?}"
    );
}

#[tokio::test]
async fn find_similar_memory_excludes_recall_cache_rows_by_default() {
    let server = make_server();
    if !server.global_vec_available {
        return;
    }
    let mut query_vec = vec![0.0; 1024];
    query_vec[0] = 1.0;

    server
        .with_global_store(|store| {
            let mut live = make_entry("similar-recall-live-row");
            live.path = "/scratch/sigil/current-similar".to_string();
            live.text = "Similar vector live memory.".to_string();
            live.summary = "Similar live memory".to_string();
            live.vector = Some(query_vec.clone());
            store.upsert(&live).map_err(|e| e.to_string())?;

            let mut cache = make_entry("foundry:recall-cache:similar");
            cache.path = "/scratch/sigil/recall-cache/similar".to_string();
            cache.text = "Similar vector recall cache.".to_string();
            cache.summary = "Similar recall cache".to_string();
            cache.topic = "recall_rerank_cache".to_string();
            cache.source = memory_core::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
            cache.metadata = json!({"cache_key": memory_core::FOUNDRY_RECALL_CACHE_SOURCE});
            cache.vector = Some(query_vec.clone());
            store.upsert(&cache).map_err(|e| e.to_string())
        })
        .expect("seed similar recall-cache boundary entries");

    let response = server
        .find_similar_memory(Parameters(FindSimilarMemoryParams {
            query_vec: query_vec.clone(),
            top_k: 5,
            path_prefix: None,
            include_archived: false,
            include_training: false,
            candidates_per_channel: 20,
        }))
        .await
        .expect("find similar should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("similar JSON");
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("similar-recall-live-row")),
        "live row should remain visible: {rows:#?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row["id"] == json!("foundry:recall-cache:similar")),
        "recall-cache row leaked through default find_similar_memory: {rows:#?}"
    );

    let response = server
        .find_similar_memory(Parameters(FindSimilarMemoryParams {
            query_vec,
            top_k: 5,
            path_prefix: Some("/scratch/sigil/recall-cache".to_string()),
            include_archived: false,
            include_training: false,
            candidates_per_channel: 20,
        }))
        .await
        .expect("find similar recall-cache scope should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("similar JSON");
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("foundry:recall-cache:similar")),
        "recall-cache scope should surface cache row: {rows:#?}"
    );
}

#[tokio::test]
async fn search_memory_boosts_guide_rows_by_context() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut guide = make_entry("guide-context-boosted");
            guide.path = "/guide/fix_pattern/rust-build".to_string();
            guide.category = "guide".to_string();
            guide.text = "UniqueGuideContextNeedle operational fix pattern.".to_string();
            guide.summary = "Guide row".to_string();
            guide.metadata = json!({
                "guide": true,
                "guide_type": "fix_pattern",
                "file_patterns": ["crates/memory-server/src/*.rs"],
                "error_patterns": ["linker error"]
            });
            store.upsert(&guide).map_err(|e| e.to_string())?;

            let mut memory = make_entry("plain-context-result");
            memory.path = "/facts/plain-context".to_string();
            memory.text = "UniqueGuideContextNeedle plain memory row.".to_string();
            memory.summary = "Plain row".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())
        })
        .expect("seed guide context entries");

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "UniqueGuideContextNeedle".to_string(),
            query_vec: None,
            top_k: 2,
            path_prefix: None,
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
            file_context: Some("crates/memory-server/src/tools.rs".to_string()),
            error_context: Some("linker error: could not find native static library".to_string()),
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        }))
        .await
        .expect("search memory with guide context");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");
    assert_eq!(rows[0]["id"], json!("guide-context-boosted"));
}
