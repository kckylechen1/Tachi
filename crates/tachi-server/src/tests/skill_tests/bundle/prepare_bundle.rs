use super::*;

#[tokio::test]
async fn prepare_capability_bundle_returns_primary_skill_and_section() {
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
            store
                .pack_register(&Pack {
                    id: "obra/superexcel".to_string(),
                    name: "SuperExcel".to_string(),
                    source: "github:obra/superexcel".to_string(),
                    version: "1.0.0".to_string(),
                    description: "Excel and spreadsheet automation pack".to_string(),
                    skill_count: 3,
                    enabled: true,
                    local_path: "/tmp/superexcel".to_string(),
                    metadata: json!({
                        "tags": ["excel", "spreadsheet", "csv"]
                    })
                    .to_string(),
                    installed_at: Utc::now().to_rfc3339(),
                    updated_at: Utc::now().to_rfc3339(),
                })
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed bundle registry");

    let result = server
        .prepare_capability_bundle(Parameters(PrepareCapabilityBundleParams {
            query: "build an excel spreadsheet from csv exports".to_string(),
            host: Some("codex".to_string()),
            skill_limit: 3,
            capability_limit: 3,
            pack_limit: 3,
            include_section: true,
        }))
        .await
        .expect("prepare_capability_bundle should succeed");
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
