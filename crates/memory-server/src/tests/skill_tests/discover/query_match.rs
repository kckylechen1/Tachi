use super::*;

#[tokio::test]
async fn tachi_skill_discover_matches_tokenized_query_and_compacts_output() {
    let server = make_server();

    server
        .with_global_store(|store| {
            store
                .hub_register(&make_skill_capability(
                    "skill:tachi-tool-guide",
                    "tachi-tool-guide",
                    "Guide for choosing Tachi facade tools after tool surface consolidation.",
                    "standard",
                ))
                .map_err(|e| format!("register failed: {e}"))
        })
        .expect("failed to register skill");

    let response = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "discover".to_string(),
            query: Some("tachi tool guide".to_string()),
            cap_type: None,
            enabled_only: Some(true),
            limit: Some(5),
            skill_id: None,
            args: None,
            profile: None,
            host: None,
            skill_limit: None,
            capability_limit: None,
            include_section: None,
        }))
        .await
        .expect("tachi_skill discover should succeed");

    let json: Value = serde_json::from_str(&response).expect("skill discover response json");
    let results = json["results"]
        .as_array()
        .expect("skill discover should return results");
    assert!(
        results
            .iter()
            .any(|item| item["id"] == json!("skill:tachi-tool-guide")),
        "expected tokenized query to find skill:tachi-tool-guide, got: {json}"
    );
    assert!(
        results.iter().all(|item| item.get("definition").is_none()),
        "skill discover facade should not return full definitions: {json}"
    );
    assert_eq!(
        json["source"],
        json!("local_approved_cache+host_skill_dirs")
    );
    assert_eq!(
        json["search_backend"],
        json!("hub_search+local_skill_index")
    );
}
