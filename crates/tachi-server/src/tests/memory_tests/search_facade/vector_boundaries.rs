use super::*;

fn assert_access_metadata_untouched(
    entry: &memcore::MemoryEntry,
    history_count: i64,
    label: &str,
) {
    assert_eq!(
        entry.access_count, 0,
        "{label} should not bump access_count"
    );
    assert_eq!(
        entry.recall_count, 0,
        "{label} should not bump recall_count"
    );
    assert!(
        entry.last_access.is_none(),
        "{label} should not set last_access"
    );
    assert_eq!(history_count, 0, "{label} should not append access_history");
}

fn read_access_snapshot(
    store: &memcore::MemoryStore,
    id: &str,
) -> (memcore::MemoryEntry, i64) {
    let entry = store
        .get(id)
        .expect("read entry")
        .expect("entry should exist");
    let history_count = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            [id],
            |row| row.get::<_, i64>(0),
        )
        .expect("read access_history count");
    (entry, history_count)
}

#[tokio::test]
async fn find_similar_memory_excludes_sft_training_rows_by_default() {
    let server = make_server();
    if !server.global_vec_available() {
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
            project: None,
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
            project: None,
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
async fn find_similar_memory_does_not_record_access_in_global_or_project_db() {
    let server = make_server();
    if !server.global_vec_available() {
        return;
    }

    let root = std::env::temp_dir().join(format!("tachi-similar-access-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");
    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("activate project db");

    let mut query_vec = vec![0.0; 1024];
    query_vec[0] = 1.0;

    server
        .with_global_store(|store| {
            let mut entry = make_entry("similar-global-access-row");
            entry.path = "/scratch/sigil/global-similar-access".to_string();
            entry.text = "Global similar access probe.".to_string();
            entry.summary = "Global similar access probe".to_string();
            entry.vector = Some(query_vec.clone());
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed global similar row");
    server
        .with_project_store(|store| {
            let mut entry = make_entry("similar-project-access-row");
            entry.path = "/scratch/sigil/project-similar-access".to_string();
            entry.text = "Project similar access probe.".to_string();
            entry.summary = "Project similar access probe".to_string();
            entry.vector = Some(query_vec.clone());
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed project similar row");

    let response = server
        .find_similar_memory(Parameters(FindSimilarMemoryParams {
            query_vec,
            top_k: 5,
            path_prefix: None,
            project: None,
            include_archived: false,
            include_training: false,
            candidates_per_channel: 20,
        }))
        .await
        .expect("find similar should succeed");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&response).expect("similar JSON");
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("similar-global-access-row")),
        "global row should be visible: {rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row["id"] == json!("similar-project-access-row")),
        "project row should be visible: {rows:#?}"
    );

    server
        .with_global_store_read(|store| {
            let (entry, history_count) = read_access_snapshot(store, "similar-global-access-row");
            assert_access_metadata_untouched(&entry, history_count, "global find_similar read");
            Ok(())
        })
        .expect("global access snapshot");
    server
        .with_project_store_read(|store| {
            let (entry, history_count) = read_access_snapshot(store, "similar-project-access-row");
            assert_access_metadata_untouched(&entry, history_count, "project find_similar read");
            Ok(())
        })
        .expect("project access snapshot");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn find_similar_memory_excludes_recall_cache_rows_by_default() {
    let server = make_server();
    if !server.global_vec_available() {
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
            cache.source = memcore::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
            cache.metadata = json!({"cache_key": memcore::FOUNDRY_RECALL_CACHE_SOURCE});
            cache.vector = Some(query_vec.clone());
            store.upsert(&cache).map_err(|e| e.to_string())
        })
        .expect("seed similar recall-cache boundary entries");

    let response = server
        .find_similar_memory(Parameters(FindSimilarMemoryParams {
            query_vec: query_vec.clone(),
            top_k: 5,
            path_prefix: None,
            project: None,
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
            project: None,
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
