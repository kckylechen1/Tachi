use super::*;

#[tokio::test]
async fn tachi_skill_bundle_wraps_capability_bundle_under_skill_facade() {
    let server = make_server();
    let excel = make_skill_capability(
        "skill:excel-automation",
        "excel-automation",
        "Build spreadsheet workflows and Excel reports from CSV data.",
        "listed",
    );

    server
        .with_global_store(|store| store.hub_register(&excel).map_err(|e| e.to_string()))
        .expect("seed skill registry");

    let result = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "bundle".to_string(),
            query: Some("build an excel spreadsheet from csv exports".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: None,
            skill_id: None,
            args: None,
            profile: None,
            host: Some("codex".to_string()),
            skill_limit: Some(3),
            capability_limit: Some(3),
            include_section: Some(true),
        }))
        .await
        .expect("tachi_skill bundle should succeed");

    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(
        json["bundle"]["primary_skill"]["id"],
        json!("skill:excel-automation")
    );
    assert!(json["bundle"]["section"]["block"]
        .as_str()
        .unwrap_or("")
        .contains("Capability Bundle"));
}
