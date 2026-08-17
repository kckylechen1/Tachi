use super::*;

// #1690 C3 slice A: the standalone `prepare_capability_bundle` backcompat route
// is retired; the handler it forwarded to SURVIVES (tachi_skill bundle/loadout
// still call it, their deletion is a later slice). This test re-anchors onto the
// canonical `tachi_skill(action='bundle')` facade surface.

#[tokio::test]
async fn tachi_skill_bundle_returns_primary_skill_and_section() {
    let server = make_server();
    let excel = make_skill_capability(
        "skill:excel-automation",
        "excel-automation",
        "Build spreadsheet workflows and Excel reports from CSV data.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&excel).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed bundle registry");

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
    let host_tools = json["bundle"]["host_tools"]
        .as_array()
        .expect("host_tools array")
        .iter()
        .filter_map(|row| row.as_str())
        .collect::<Vec<_>>();
    assert!(host_tools.contains(&"python"));
    assert!(json["bundle"]["section"]["block"]
        .as_str()
        .unwrap_or("")
        .contains("Capability Bundle"));
}
