use super::*;

mod ask;
mod recall_simulate;
mod save;

#[tokio::test]
async fn tachi_memory_recall_proposals_review_and_apply_config_env() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed config.env");

    server
        .with_global_store(|store| {
            let mut all_terms = make_entry("recall-proposal-all-terms");
            all_terms.path = "/scratch/tachi/recall-proposal-all-terms".to_string();
            all_terms.summary = "Recall proposal all terms".to_string();
            all_terms.text = "cleanup preview safe deployment note".to_string();
            all_terms.keywords = vec![
                "cleanup".to_string(),
                "preview".to_string(),
                "safe".to_string(),
            ];
            store.upsert(&all_terms).map_err(|e| e.to_string())?;

            let mut partial = make_entry("recall-proposal-partial-term");
            partial.path = "/scratch/tachi/recall-proposal-partial-term".to_string();
            partial.summary = "Recall proposal partial term".to_string();
            partial.text = "cleanup preview deletes stale artifacts".to_string();
            partial.keywords = Vec::new();
            store.upsert(&partial).map_err(|e| e.to_string())
        })
        .expect("seed recall proposal entries");

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-proposal-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    }));

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("proposal response JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .expect("proposal list")
        .iter()
        .find(|proposal| proposal["variant"] == json!("or-fallback-0.6"))
        .unwrap_or_else(|| panic!("expected recall proposal in response: {parsed}"));
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    assert_eq!(proposal["kind"], json!("recall_config"));
    assert_eq!(
        proposal["config_env"]["TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR"],
        json!("0.6")
    );
    assert_eq!(
        proposal["config_env"]["TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS"],
        json!("4")
    );

    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    review.notes = Some("fixture approval".to_string());
    let review_body = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("review should succeed");
    let review_json: Value = serde_json::from_str(&review_body).expect("review JSON");
    assert_eq!(review_json["proposal"]["status"], json!("approved"));

    let mut missing_confirm = tachi_memory_params("apply_recall_proposals");
    missing_confirm.proposal_id = Some(proposal_id.clone());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, missing_confirm)
        .await
        .expect_err("apply should require confirm=true");
    assert!(err.contains("confirm=true"));

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    let apply_body = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("apply should succeed");
    let apply_json: Value = serde_json::from_str(&apply_body).expect("apply JSON");
    assert_eq!(apply_json["restart_required"], json!(true));
    assert!(apply_json["updated_keys"]
        .as_array()
        .expect("updated keys")
        .contains(&json!("TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR")));

    let config_body = std::fs::read_to_string(&config_env).expect("read config.env");
    assert!(config_body.contains("VOYAGE_API_KEY=vault:VOYAGE_API_KEY"));
    assert!(config_body.contains("TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6"));
    assert!(config_body.contains("TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4"));
}

#[tokio::test]
async fn tachi_memory_readiness_can_return_operational_json() {
    let server = make_server();
    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "readiness",
        "format": "json"
    }))
    .expect("params deserialize");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("readiness json should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("readiness response should be JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert!(parsed["runtime"].is_object());
    assert!(parsed["health"].is_object());
    assert!(parsed["tools"].is_array());
    assert!(parsed["tool_visibility_summary"].is_object());
    assert!(parsed["suggestions"].is_array());
    assert!(parsed["vector_health"].is_object());
    assert!(parsed["readiness_warnings"].is_array());

    let tools = server.tachi_tools().await.expect("tachi_tools");
    let tools_count = tools
        .lines()
        .find_map(|line| line.strip_prefix("count: "))
        .expect("tachi_tools count line")
        .parse::<u64>()
        .expect("numeric tachi_tools count");
    assert_eq!(
        parsed["tool_visibility_summary"]["visible_count"],
        json!(tools_count),
        "readiness visible_count should match tachi_tools output"
    );
}

#[tokio::test]
async fn tachi_memory_alerts_and_compact_briefing_report_same_wiki_counts() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut entry = make_entry("briefing-alerts-wiki-orphan");
            entry.path = "/wiki/test/briefing-alerts-orphan".to_string();
            entry.summary = "Briefing alerts wiki orphan".to_string();
            entry.text =
                "BriefingAlertsWikiCountNeedle should be counted by wiki hygiene.".to_string();
            entry.domain = Some("wiki".to_string());
            entry.metadata = json!({"wiki": true});
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed wiki hygiene row");

    let mut briefing_params = tachi_memory_params("briefing");
    briefing_params.format = Some("json".to_string());
    briefing_params.query = Some("BriefingAlertsWikiCountNeedle".to_string());
    briefing_params.compact = true;
    let briefing_body = crate::facade_memory_ops::handle_tachi_memory(&server, briefing_params)
        .await
        .expect("briefing should succeed");
    let briefing_json: Value = serde_json::from_str(&briefing_body).expect("briefing JSON");

    let mut alerts_params = tachi_memory_params("alerts");
    alerts_params.format = Some("json".to_string());
    let alerts_body = crate::facade_memory_ops::handle_tachi_memory(&server, alerts_params)
        .await
        .expect("alerts should succeed");
    let alerts_json: Value = serde_json::from_str(&alerts_body).expect("alerts JSON");

    assert_eq!(
        briefing_json["health"]["wiki"], alerts_json["wiki_counts"],
        "alerts and compact briefing should report the same wiki hygiene counts"
    );
    assert!(
        alerts_json["wiki_counts"]["orphans"].as_u64().unwrap_or(0) >= 1,
        "fixture should produce a visible orphan count: {alerts_json}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_memory_progress_writes_append_only_jsonl() {
    let (server, temp_home) = make_server_with_temp_home();
    let run_root = temp_home.temp_home.join("runs");
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "progress".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            scope: None,
            top_k: 6,
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
            text: Some("step completed with api_key=test-secret-value-1234567890".to_string()),
            title: Some("Progress step".to_string()),
            summary: Some("one step done".to_string()),
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
            flow_id: Some("flow_progress_test".to_string()),
            event: Some("validation".to_string()),
            state: Some("running".to_string()),
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
        },
    )
    .await
    .expect("progress should record");

    assert!(body.starts_with("## Tachi progress"));
    assert!(body.contains("status: recorded"));
    assert!(body.contains("secret_redactions: 1"));
    let log = std::fs::read_to_string(run_root.join("flow_progress_test/progress.jsonl"))
        .expect("progress jsonl");
    assert!(log.contains("validation"));
    assert!(!log.contains("test-secret-value-1234567890"));
    if let Some(original) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", original);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
