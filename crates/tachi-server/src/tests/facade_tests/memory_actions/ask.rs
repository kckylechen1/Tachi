use super::*;

#[tokio::test]
async fn tachi_memory_ask_returns_evidence_contract() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "ask".to_string(),
            format: Some("markdown".to_string()),
            query: Some("what did we implement".to_string()),
            scope: None,
            top_k: 3,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            agent_role: None,
            text: None,
            title: None,
            summary: None,
            topic: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: None,
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
            content: None,
            ingest_type: "source".to_string(),
            source_url: None,
            auto_chunk: true,
            auto_summarize: true,
            auto_link: true,
            chunk_size_chars: 1200,
            chunk_overlap_chars: 120,
            conversation_id: None,
            turn_id: None,
            event_type: None,
            messages: Vec::new(),
        },
    )
    .await
    .expect("ask should succeed");

    assert!(body.starts_with("## Tachi ask"));
    assert!(body.contains("status: completed"));
    assert!(body.contains("evidence:"));
    // evidence count may be 0 in CI without embedding API; verify format only
    assert!(body.contains("hit(s)"));
}

#[tokio::test]
async fn tachi_memory_ask_can_return_compact_json() {
    let server = make_server();
    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "ask",
        "format": "json",
        "query": "what did we implement",
        "top_k": 3
    }))
    .expect("params deserialize");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("ask json should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("ask response should be JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["query"], json!("what did we implement"));
    assert!(parsed["evidence"].is_array());
    assert!(parsed["thinking"].is_object());
}

#[tokio::test]
async fn tachi_memory_ask_keeps_controlled_probe_evidence_aligned_with_search() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut alpha = make_entry("ask-parity-alpha");
            alpha.path = "/scratch/tachi/ask-parity-alpha".to_string();
            alpha.summary = "Ask parity alpha".to_string();
            alpha.text =
                "RECALL_PROBE_ALPHA_ASK_20260607 clean-cli bridge dry-run force-delete behavior"
                    .to_string();
            alpha.keywords = vec!["recall-probe".to_string(), "clean-cli".to_string()];
            store.upsert(&alpha).map_err(|e| e.to_string())?;

            for idx in 0..12 {
                let mut distractor = make_entry(&format!("ask-parity-distractor-{idx}"));
                distractor.path = format!("/scratch/tachi/ask-parity-distractor-{idx}");
                distractor.summary = format!("Ask parity distractor {idx}");
                distractor.text =
                    format!("RECALL_PROBE_BETA_ASK_20260607 clean-cli bridge candidate {idx}");
                distractor.keywords = vec!["recall-probe".to_string(), "clean-cli".to_string()];
                store.upsert(&distractor).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed ask/search parity entries");

    let mut search_params = tachi_memory_params("search");
    search_params.format = Some("json".to_string());
    search_params.query = Some("RECALL_PROBE_ALPHA_ASK_20260607".to_string());
    search_params.scope = Some("memory".to_string());
    search_params.top_k = 3;
    let search_body = crate::facade_memory_ops::handle_tachi_memory(&server, search_params)
        .await
        .expect("search should succeed");
    let search_json: Value = serde_json::from_str(&search_body).expect("search JSON");
    let search_ids = search_json["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .flat_map(|section| {
            section["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row["id"].as_str())
        })
        .collect::<Vec<_>>();

    let mut ask_params = tachi_memory_params("ask");
    ask_params.format = Some("json".to_string());
    ask_params.query = Some("RECALL_PROBE_ALPHA_ASK_20260607".to_string());
    ask_params.scope = Some("memory".to_string());
    ask_params.top_k = 3;
    ask_params.enable_rerank = false;
    let ask_body = crate::facade_memory_ops::handle_tachi_memory(&server, ask_params)
        .await
        .expect("ask should succeed");
    let ask_json: Value = serde_json::from_str(&ask_body).expect("ask JSON");
    let evidence = ask_json["evidence"].as_array().expect("ask evidence");
    let ask_ids = evidence
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(search_ids.first().copied(), Some("ask-parity-alpha"));
    assert_eq!(ask_ids.first().copied(), Some("ask-parity-alpha"));
    assert!(
        ask_ids
            .iter()
            .take(3)
            .any(|id| search_ids.iter().take(3).any(|search_id| search_id == id)),
        "ask evidence should overlap controlled search evidence: search={search_ids:?} ask={ask_ids:?}"
    );
    assert!(
        evidence
            .first()
            .is_some_and(|row| row.get("rerank_policy").is_none()),
        "ask should not force rerank when enable_rerank=false: {ask_json}"
    );
}
