use super::*;

#[tokio::test]
async fn tachi_memory_search_defaults_to_json_and_keeps_markdown_escape_hatch() {
    let server = make_server();

    let mut json_params = tachi_memory_params("search");
    json_params.format = None;
    json_params.query = Some("facade default json no matches".to_string());
    let json_body = crate::facade_memory_ops::handle_tachi_memory(&server, json_params)
        .await
        .expect("default search should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("default search JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["query"], json!("facade default json no matches"));

    let mut markdown_params = tachi_memory_params("search");
    markdown_params.query = Some("facade markdown output".to_string());
    let markdown = crate::facade_memory_ops::handle_tachi_memory(&server, markdown_params)
        .await
        .expect("markdown search should succeed");
    assert!(markdown.starts_with("## Tachi search:"), "{markdown}");
}

#[tokio::test]
async fn tachi_memory_search_records_user_access_history() {
    let server = make_server();
    let entry_id = format!("explicit-recall-access-{}", uuid::Uuid::new_v4());
    let mut entry = make_entry(&entry_id);
    entry.summary = "explicit recall access sentinel".to_string();
    entry.text = "explicit recall access sentinel should record access history".to_string();
    entry.keywords = vec!["explicit".to_string(), "recall".to_string()];
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("seed: {e}")))
        .expect("seed recall entry");

    let mut params = tachi_memory_params("search");
    params.format = None;
    params.scope = Some("memory".to_string());
    params.query = Some("explicit recall access sentinel".to_string());
    params.top_k = 3;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("search should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("search JSON");
    let memory_rows = parsed["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .and_then(|section| section["rows"].as_array())
        .expect("memory rows");
    assert!(
        memory_rows.iter().any(|row| row["id"] == json!(entry_id)),
        "seeded row should be recalled: {memory_rows:?}"
    );

    let post = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&entry_id, false)
                .map_err(|e| format!("get: {e}"))
        })
        .expect("post-read")
        .expect("seeded entry exists");
    assert_eq!(
        post.access_count, 1,
        "user-facing search should bump access_count once"
    );
    assert!(
        post.last_access.is_some(),
        "user-facing search should set last_access"
    );
}

#[tokio::test]
async fn tachi_memory_search_caps_large_top_k() {
    let server = make_server();
    server
        .with_global_store(|store| {
            for idx in 0..(crate::MAX_FACADE_TOP_K + 25) {
                let mut entry = make_entry(&format!("facade-clamp-{idx}"));
                entry.path = format!("/facade/clamp/{idx}");
                entry.summary = format!("facade clamp sentinel {idx}");
                entry.text = format!("facade clamp sentinel searchable row {idx}");
                entry.keywords = vec!["facade".to_string(), "clamp".to_string()];
                store
                    .upsert(&entry)
                    .map_err(|e| format!("seed clamp row: {e}"))?;
            }
            Ok(())
        })
        .expect("seed clamp memories");

    let mut params = tachi_memory_params("search");
    params.format = None;
    params.query = Some("facade clamp sentinel".to_string());
    params.top_k = 10_000;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("search should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("search JSON");
    let memory_rows = parsed["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .and_then(|section| section["rows"].as_array())
        .expect("memory rows");

    assert_eq!(memory_rows.len(), crate::MAX_FACADE_TOP_K);
}

#[tokio::test]
async fn direct_tachi_search_caps_large_top_k() {
    let server = make_server();
    server
        .with_global_store(|store| {
            for idx in 0..(crate::MAX_FACADE_TOP_K + 25) {
                let mut entry = make_entry(&format!("direct-search-clamp-{idx}"));
                entry.path = format!("/facade/direct-clamp/{idx}");
                entry.summary = format!("direct facade clamp sentinel {idx}");
                entry.text = format!("direct facade clamp sentinel searchable row {idx}");
                entry.keywords = vec!["direct".to_string(), "facade".to_string()];
                store
                    .upsert(&entry)
                    .map_err(|e| format!("seed direct clamp row: {e}"))?;
            }
            Ok(())
        })
        .expect("seed direct clamp memories");

    let params = TachiSearchParams {
        query: "direct facade clamp sentinel".to_string(),
        scope: "memory".to_string(),
        top_k: 10_000,
        path_prefix: None,
        project: None,
        domain: None,
        file_context: None,
        error_context: None,
        context_symbols: Vec::new(),
        category: None,
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
    };

    let (sections, _, _) =
        crate::facade_search_ops::collect_tachi_search_sections(&server, &params).await;
    let memory_rows = sections
        .iter()
        .find(|(name, _)| name == "Memory")
        .and_then(|(_, rows)| rows.as_array())
        .expect("memory rows");

    assert_eq!(memory_rows.len(), crate::MAX_FACADE_TOP_K);
}
