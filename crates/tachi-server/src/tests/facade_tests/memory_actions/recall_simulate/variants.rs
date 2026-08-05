use super::*;

#[tokio::test]
async fn tachi_tune_recall_simulate_compares_recall_config_variants() {
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

    let mut params = tachi_tune_params("recall_simulate");
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

    let body = handle_tachi_tune_for_test(&server, params)
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
