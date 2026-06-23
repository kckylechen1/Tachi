use super::*;

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
