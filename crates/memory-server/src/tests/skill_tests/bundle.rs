use super::*;

#[tokio::test]
async fn recommend_toolchain_infers_host_tools_and_projected_packs() {
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
            store
                .projection_upsert(&AgentProjection {
                    agent: "codex".to_string(),
                    pack_id: "obra/superexcel".to_string(),
                    enabled: true,
                    projected_path: "/tmp/codex/superexcel".to_string(),
                    skill_count: 3,
                    synced_at: Utc::now().to_rfc3339(),
                })
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed capability registry");

    let result = server
        .recommend_toolchain(Parameters(RecommendToolchainParams {
            query: "build an excel spreadsheet from csv exports".to_string(),
            host: Some("codex".to_string()),
            skill_limit: 3,
            capability_limit: 3,
            pack_limit: 3,
        }))
        .await
        .expect("recommend_toolchain should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let host_tools = json["host_tools"]
        .as_array()
        .expect("host_tools array")
        .iter()
        .filter_map(|row| row.as_str())
        .collect::<Vec<_>>();
    assert!(host_tools.contains(&"python"));
    assert!(host_tools.contains(&"filesystem"));
    assert_eq!(json["packs"][0]["id"], "obra/superexcel");
    assert_eq!(json["packs"][0]["projected_to_host"], true);
    assert_eq!(json["skills"][0]["id"], "skill:excel-automation");
}

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
            pack_limit: Some(1),
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

#[tokio::test]
async fn tachi_skill_bundle_requires_query() {
    let server = make_server();

    let err = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "bundle".to_string(),
            query: None,
            cap_type: None,
            enabled_only: None,
            limit: None,
            skill_id: None,
            args: None,
            profile: None,
            host: Some("codex".to_string()),
            skill_limit: None,
            capability_limit: None,
            pack_limit: None,
            include_section: None,
        }))
        .await
        .expect_err("missing bundle query should fail");

    assert!(
        err.contains("query is required when action='bundle'"),
        "{err}"
    );
}
