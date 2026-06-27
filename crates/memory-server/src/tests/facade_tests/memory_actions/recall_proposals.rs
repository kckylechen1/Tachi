use super::*;

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
