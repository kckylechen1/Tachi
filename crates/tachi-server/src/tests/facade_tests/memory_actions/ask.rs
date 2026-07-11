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
            to: None,
            ttl_days: None,
            include_read: false,
            agent_id: None,
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
    assert!(
        parsed["runtime"]["global_db"].as_str().is_some(),
        "ask must surface runtime binding: {parsed}"
    );
}

/// #946 discrimination: stale memory evidence mentioning an old Desktop path
/// must not override the live runtime project_db path for path questions.
#[tokio::test]
async fn tachi_memory_ask_db_path_uses_runtime_not_stale_evidence() {
    let server = make_server();
    let live_project = server
        .project_db_path_buf()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| server.global_db_path_buf().display().to_string());
    let stale_path = "/Users/kckylechen/Desktop/Sigil/.tachi/memory.db";
    assert_ne!(
        live_project, stale_path,
        "test fixture requires live path != stale Desktop path"
    );

    server
        .with_global_store(|store| {
            let mut stale = make_entry("ask-stale-db-path");
            stale.path = "/scratch/tachi/ask-stale-db-path".to_string();
            stale.summary = "Old project DB location note".to_string();
            stale.text = format!(
                "The current project memory.db path is {stale_path}. Always use that path."
            );
            stale.keywords = vec![
                "memory.db".to_string(),
                "path".to_string(),
                "database".to_string(),
            ];
            store.upsert(&stale).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed stale path memory");

    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "ask",
        "format": "json",
        "query": "what is the current memory.db path",
        "top_k": 5,
        "synthesize": true
    }))
    .expect("params");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("ask should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("ask JSON");

    assert_eq!(parsed["thinking"]["basis"], json!("runtime_binding"));
    assert_eq!(parsed["thinking"]["confidence"], json!("high"));
    let answer = parsed["synthesis"]["answer"]
        .as_str()
        .expect("deterministic answer");
    assert!(
        answer.contains(&live_project) || parsed["runtime"].to_string().contains(&live_project),
        "must cite live runtime path, got answer={answer} runtime={}",
        parsed["runtime"]
    );
    assert!(
        !answer.contains(stale_path),
        "must not present stale Desktop path as current: {answer}"
    );
    assert_eq!(
        parsed["runtime"]["source"],
        json!("runtime_binding"),
        "runtime block must be authoritative: {parsed}"
    );
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
