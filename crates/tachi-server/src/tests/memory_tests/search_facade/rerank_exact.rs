use super::*;

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
            context_symbols: Vec::new(),
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
