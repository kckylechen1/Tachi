use super::*;

// #1690 C3 slice A: the standalone `recommend_toolchain` route is retired with
// the "second model brain". The behaviors it guarded — host-tool inference for
// a query and ranked skill matching — survive in the kept
// `handle_prepare_capability_bundle` (reachable through the canonical
// `tachi_skill(action='bundle')` facade), so this test re-anchors onto that
// live surface: `bundle.host_tools` and `bundle.primary_skill`.

fn bundle_params(query: &str, host: &str) -> TachiSkillParams {
    TachiSkillParams {
        action: "bundle".to_string(),
        query: Some(query.to_string()),
        cap_type: None,
        enabled_only: None,
        limit: None,
        skill_id: None,
        args: None,
        profile: None,
        host: Some(host.to_string()),
        skill_limit: Some(3),
        capability_limit: Some(3),
        include_section: Some(true),
    }
}

#[tokio::test]
async fn tachi_skill_bundle_infers_host_tools_and_primary_skill() {
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
        .tachi_skill(Parameters(bundle_params(
            "build an excel spreadsheet from csv exports",
            "codex",
        )))
        .await
        .expect("tachi_skill bundle should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let host_tools = json["bundle"]["host_tools"]
        .as_array()
        .expect("host_tools array")
        .iter()
        .filter_map(|row| row.as_str())
        .collect::<Vec<_>>();
    assert!(host_tools.contains(&"python"));
    assert!(host_tools.contains(&"filesystem"));
    assert_eq!(
        json["bundle"]["primary_skill"]["id"],
        json!("skill:excel-automation")
    );
}
