use super::*;

#[tokio::test]
async fn tachi_memory_recall_simulate_reports_hit_metrics_without_access_mutation() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut alpha = make_entry("recall-sim-alpha");
            alpha.path = "/scratch/tachi/recall-sim-alpha".to_string();
            alpha.summary = "Recall simulate alpha".to_string();
            alpha.text =
                "RECALL_SIM_ALPHA_NEEDLE_20260626 clean-cli dry-run force delete".to_string();
            alpha.keywords = vec!["recall-sim".to_string(), "clean-cli".to_string()];
            store.upsert(&alpha).map_err(|e| e.to_string())?;

            let mut beta = make_entry("recall-sim-beta");
            beta.path = "/scratch/tachi/recall-sim-beta".to_string();
            beta.summary = "Recall simulate beta".to_string();
            beta.text = "RECALL_SIM_BETA_NEEDLE_20260626 unrelated router audit".to_string();
            beta.keywords = vec!["recall-sim".to_string(), "router".to_string()];
            store.upsert(&beta).map_err(|e| e.to_string())
        })
        .expect("seed recall simulation entries");

    let mut params = tachi_memory_params("recall_simulate");
    params.format = Some("json".to_string());
    params.scope = Some("memory".to_string());
    params.top_k = 1;
    params.metadata = Some(json!({
        "cases": [
            {
                "name": "alpha-hit",
                "query": "RECALL_SIM_ALPHA_NEEDLE_20260626",
                "expected_id": "recall-sim-alpha"
            },
            {
                "name": "beta-miss",
                "query": "RECALL_SIM_ALPHA_NEEDLE_20260626",
                "expected_id": "recall-sim-beta"
            }
        ]
    }));

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("recall_simulate should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("recall_simulate JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["action"], json!("recall_simulate"));
    assert_eq!(parsed["case_count"], json!(2));
    assert_eq!(parsed["metrics"]["hit_count"], json!(1));
    assert_eq!(parsed["metrics"]["miss_count"], json!(1));
    assert_eq!(parsed["metrics"]["recall_at_k"], json!(0.5));
    assert_eq!(parsed["metrics"]["mrr"], json!(0.5));
    assert_eq!(parsed["cases"][0]["hit"], json!(true));
    assert_eq!(parsed["cases"][0]["rank"], json!(1));
    assert_eq!(
        parsed["cases"][0]["returned_ids"][0],
        json!("recall-sim-alpha")
    );
    assert_eq!(parsed["cases"][1]["hit"], json!(false));

    let db_path = server.global_db_path_buf();
    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    let access_count: i64 = conn
        .query_row(
            "SELECT access_count FROM memories WHERE id='recall-sim-alpha'",
            [],
            |row| row.get(0),
        )
        .expect("read access_count");
    assert_eq!(access_count, 0, "recall_simulate should not record access");
}

#[tokio::test]
async fn tachi_memory_recall_simulate_accepts_text_json_cases() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut entry = make_entry("recall-sim-text-json");
            entry.path = "/scratch/tachi/recall-sim-text-json".to_string();
            entry.summary = "Recall simulate text JSON".to_string();
            entry.text = "RECALL_SIM_TEXT_JSON_NEEDLE_20260626".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed recall simulation text JSON entry");

    let mut params = tachi_memory_params("recall_simulate");
    params.format = Some("json".to_string());
    params.text = Some(
        json!({
            "eval_cases": [
                {
                    "query": "RECALL_SIM_TEXT_JSON_NEEDLE_20260626",
                    "expected_ids": ["recall-sim-text-json"]
                }
            ]
        })
        .to_string(),
    );

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("recall_simulate should accept text JSON");
    let parsed: Value = serde_json::from_str(&body).expect("recall_simulate JSON");

    assert_eq!(parsed["metrics"]["hit_count"], json!(1));
    assert_eq!(parsed["cases"][0]["rank"], json!(1));
}

#[tokio::test]
async fn tachi_memory_recall_simulate_compares_recall_config_variants() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut all_terms = make_entry("recall-sim-all-terms");
            all_terms.path = "/scratch/tachi/recall-sim-all-terms".to_string();
            all_terms.summary = "Recall simulate all terms".to_string();
            all_terms.text = "cleanup cli safe deployment note".to_string();
            all_terms.keywords = vec!["cleanup".to_string(), "cli".to_string(), "safe".to_string()];
            store.upsert(&all_terms).map_err(|e| e.to_string())?;

            let mut partial = make_entry("recall-sim-partial-term");
            partial.path = "/scratch/tachi/recall-sim-partial-term".to_string();
            partial.summary = "Recall simulate partial term".to_string();
            partial.text = "cleanup preview deletes stale artifacts".to_string();
            partial.keywords = vec!["cleanup".to_string()];
            store.upsert(&partial).map_err(|e| e.to_string())
        })
        .expect("seed recall simulation variant entries");

    let mut params = tachi_memory_params("recall_simulate");
    params.format = Some("json".to_string());
    params.scope = Some("memory".to_string());
    params.top_k = 3;
    params.metadata = Some(json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup cli safe",
                "expected_id": "recall-sim-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.3",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.3,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    }));

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("recall_simulate variants should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("recall_simulate JSON");

    let variants = parsed["variants"].as_array().expect("variants array");
    assert_eq!(variants.len(), 2);
    assert_eq!(variants[0]["name"], json!("current"));
    assert_eq!(variants[1]["name"], json!("or-fallback-0.3"));
    assert_eq!(
        variants[1]["config"]["or_fallback_fts_score_factor"],
        json!(0.3)
    );
    assert_eq!(variants[1]["config"]["or_fallback_fts_max_terms"], json!(4));
    assert!(variants[1]["cases"][0]["returned_ids"]
        .as_array()
        .expect("returned ids")
        .iter()
        .any(|id| id == "recall-sim-partial-term"));
}

#[tokio::test]
async fn tachi_memory_recall_simulate_keeps_exact_token_top_when_rerank_enabled() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut alpha = make_entry("recall-sim-rerank-alpha");
            alpha.path = "/scratch/tachi/recall-sim-rerank-alpha".to_string();
            alpha.summary = "Recall simulate rerank alpha".to_string();
            alpha.text = "RECALL_SIM_RERANK_ALPHA_20260626 clean-cli bridge dry-run force-delete"
                .to_string();
            alpha.keywords = vec![
                "recall-sim".to_string(),
                "clean-cli".to_string(),
                "dry-run".to_string(),
            ];
            store.upsert(&alpha).map_err(|e| e.to_string())?;

            let mut beta = make_entry("recall-sim-rerank-beta");
            beta.path = "/scratch/tachi/recall-sim-rerank-beta".to_string();
            beta.summary = "Recall simulate rerank beta".to_string();
            beta.text = "RECALL_SIM_RERANK_BETA_20260626 cleanup defaults preview".to_string();
            beta.keywords = vec!["recall-sim".to_string(), "cleanup".to_string()];
            store.upsert(&beta).map_err(|e| e.to_string())?;

            let mut delta = make_entry("recall-sim-rerank-delta");
            delta.path = "/scratch/tachi/recall-sim-rerank-delta".to_string();
            delta.summary = "Recall simulate rerank delta".to_string();
            delta.text =
                "RECALL_SIM_RERANK_DELTA_20260626 profile routing requested_profile".to_string();
            delta.keywords = vec!["recall-sim".to_string(), "profile".to_string()];
            store.upsert(&delta).map_err(|e| e.to_string())
        })
        .expect("seed rerank recall simulation entries");

    let mut params = tachi_memory_params("recall_simulate");
    params.format = Some("json".to_string());
    params.scope = Some("memory".to_string());
    params.top_k = 1;
    params.enable_rerank = true;
    params.metadata = Some(json!({
        "cases": [
            {
                "name": "exact-alpha",
                "query": "RECALL_SIM_RERANK_ALPHA_20260626",
                "expected_id": "recall-sim-rerank-alpha"
            }
        ]
    }));

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("recall_simulate should replay rerank-enabled searches");
    let parsed: Value = serde_json::from_str(&body).expect("recall_simulate JSON");

    assert_eq!(parsed["rerank"]["enabled"], json!(true));
    assert_eq!(parsed["metrics"]["hit_count"], json!(1));
    assert_eq!(
        parsed["cases"][0]["returned_ids"][0],
        json!("recall-sim-rerank-alpha")
    );
    assert_eq!(
        parsed["cases"][0]["rerank"]["policy"],
        json!("skipped_exact_token")
    );
    assert_eq!(
        parsed["cases"][0]["returned"][0]["match_type"],
        json!("exact_token")
    );
    assert_eq!(
        parsed["cases"][0]["returned"][0]["rerank_policy"],
        json!("skipped_exact_token")
    );
    assert_eq!(
        parsed["variants"][0]["rerank"]["policy_counts"]["skipped_exact_token"],
        json!(1)
    );
}
