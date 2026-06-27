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
