use super::*;

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
