use super::*;

#[tokio::test]
async fn recommend_toolchain_infers_host_tools_and_skills() {
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
        .expect("seed capability registry");

    let result = server
        .recommend_toolchain(Parameters(RecommendToolchainParams {
            query: "build an excel spreadsheet from csv exports".to_string(),
            host: Some("codex".to_string()),
            skill_limit: 3,
            capability_limit: 3,
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
    assert_eq!(json["skills"][0]["id"], "skill:excel-automation");
}
