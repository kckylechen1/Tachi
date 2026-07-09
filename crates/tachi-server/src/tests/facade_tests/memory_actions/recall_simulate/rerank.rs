use super::*;

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
    assert_eq!(
        parsed["variants"][0]["flip_report"]["hit_to_miss"],
        json!(0)
    );
    assert_eq!(
        parsed["variants"][0]["flip_report"]["hit_retained"],
        json!(1)
    );
    assert_eq!(
        parsed["cases"][0]["baseline"]["returned_ids"][0],
        json!("recall-sim-rerank-alpha")
    );
}
