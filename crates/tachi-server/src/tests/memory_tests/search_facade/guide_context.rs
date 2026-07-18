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
                "file_patterns": ["crates/tachi-server/src/*.rs"],
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
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: None,
            file_context: Some("crates/tachi-server/src/tools.rs".to_string()),
            error_context: Some("linker error: could not find native static library".to_string()),
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            // tachi#1201 k3: search_memory now defaults to markdown; this
            // test parses the response as JSON, so opt in explicitly.
            format: Some("json".to_string()),
        }))
        .await
        .expect("search memory with guide context");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");
    assert_eq!(rows[0]["id"], json!("guide-context-boosted"));
}

#[tokio::test]
async fn search_memory_reports_recall_quality_when_vectors_are_sparse() {
    let server = make_server();
    let mut query_vec = vec![0.0; 1024];
    query_vec[0] = 1.0;
    server
        .with_global_store(|store| {
            for idx in 0..3 {
                let mut memory = make_entry(&format!("sparse-recall-row-{idx}"));
                memory.path = format!("/facts/sparse-recall/{idx}");
                memory.text = format!("SparseRecallQualityNeedle memory row {idx}.");
                memory.summary = format!("Sparse recall row {idx}");
                store.upsert(&memory).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed sparse recall entries");

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "SparseRecallQualityNeedle".to_string(),
            query_vec: Some(query_vec),
            top_k: 3,
            path_prefix: None,
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
            // tachi#1201 k3: search_memory now defaults to markdown; this
            // test parses the response as JSON, so opt in explicitly.
            format: Some("json".to_string()),
        }))
        .await
        .expect("search should succeed");

    let rows: Vec<Value> = serde_json::from_str(&response).expect("search JSON");
    let recall_quality = rows
        .iter()
        .find_map(|row| row.get("recall_quality"))
        .expect("sparse vector coverage should be surfaced on search rows");
    assert_eq!(recall_quality["status"], json!("degraded"));
    assert_eq!(recall_quality["vector_missing"], json!(3));
}

#[tokio::test]
async fn search_memory_context_symbols_participate_in_plain_query_recall() {
    let server = make_server();

    server
        .with_global_store(|store| {
            let mut entry = make_entry("context-symbol-hit");
            entry.path = "/context-symbols".to_string();
            entry.summary = "Plain continuity issue".to_string();
            entry.text =
                "tachi-server regression notes without the user-facing query term".to_string();
            entry.entities = vec!["tachi-server".to_string()];
            store.upsert(&entry).map_err(|e| format!("seed entry: {e}"))
        })
        .expect("seed context symbol memory");

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "release blocker".to_string(),
            query_vec: None,
            top_k: 3,
            path_prefix: Some("/context-symbols".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: vec!["tachi-server".to_string()],
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            // tachi#1201 k3: search_memory now defaults to markdown; this
            // test parses the response as JSON, so opt in explicitly.
            format: Some("json".to_string()),
        }))
        .await
        .expect("search memory with context symbols");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("search JSON");

    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("context-symbol-hit")),
        "context symbol should help recall the seeded row, got {rows:?}"
    );
}
