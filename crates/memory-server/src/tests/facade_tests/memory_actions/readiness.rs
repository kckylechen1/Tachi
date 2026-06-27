use super::*;

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
