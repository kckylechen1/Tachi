use super::*;

#[tokio::test]
async fn tachi_tune_recall_simulate_accepts_text_json_cases() {
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

    let mut params = tachi_tune_params("recall_simulate");
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

    let body = handle_tachi_tune_for_test(&server, params)
        .await
        .expect("recall_simulate should accept text JSON");
    let parsed: Value = serde_json::from_str(&body).expect("recall_simulate JSON");

    assert_eq!(parsed["metrics"]["hit_count"], json!(1));
    assert_eq!(parsed["cases"][0]["rank"], json!(1));
}
