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
            pack_limit: None,
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

#[tokio::test]
async fn tachi_skill_discover_defaults_to_callable_approved_skills() {
    let server = make_server();

    let mut pending = make_skill_capability(
        "skill:debug-pending",
        "debug-pending",
        "Debug workflow that still needs review",
        "standard",
    );
    pending.review_status = "pending".to_string();
    let mut unhealthy = make_skill_capability(
        "skill:debug-unhealthy",
        "debug-unhealthy",
        "Debug workflow with failing health",
        "standard",
    );
    unhealthy.health_status = "unhealthy".to_string();

    server
        .with_global_store(|store| {
            store
                .hub_register(&make_skill_capability(
                    "skill:debug-approved",
                    "debug-approved",
                    "Approved debug workflow",
                    "standard",
                ))
                .map_err(|e| format!("register approved failed: {e}"))?;
            store
                .hub_register(&pending)
                .map_err(|e| format!("register pending failed: {e}"))?;
            store
                .hub_register(&unhealthy)
                .map_err(|e| format!("register unhealthy failed: {e}"))?;
            Ok::<_, String>(())
        })
        .expect("failed to register skills");

    let response = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "discover".to_string(),
            query: Some("debug workflow".to_string()),
            cap_type: None,
            enabled_only: None,
            limit: Some(10),
            skill_id: None,
            args: None,
            profile: None,
            host: None,
            skill_limit: None,
            capability_limit: None,
            pack_limit: None,
            include_section: None,
        }))
        .await
        .expect("tachi_skill discover should succeed");

    let json: Value = serde_json::from_str(&response).expect("skill discover response json");
    let ids = json["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|item| item.get("id").and_then(|id| id.as_str()))
        .collect::<Vec<_>>();

    assert!(ids.contains(&"skill:debug-approved"), "{json}");
    assert!(!ids.contains(&"skill:debug-pending"), "{json}");
    assert!(!ids.contains(&"skill:debug-unhealthy"), "{json}");
    assert!(json["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| { item.get("callable").and_then(|value| value.as_bool()) == Some(true) }));
    let approved = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.get("id").and_then(|id| id.as_str()) == Some("skill:debug-approved"))
        .expect("approved skill result should be present");
    assert_eq!(
        approved.get("source").and_then(|value| value.as_str()),
        Some("local_approved_cache")
    );
}

#[test]
fn tachi_skill_discover_prefers_hub_waza_skill_over_host_duplicate() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original_home = std::env::var_os("HOME");
    let temp_home =
        std::env::temp_dir().join(format!("tachi-waza-dedup-test-{}", uuid::Uuid::new_v4()));
    let skill_dir = temp_home.join(".agents/skills/check");
    std::fs::create_dir_all(&skill_dir).expect("create host check skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: check\ndescription: Review Before You Ship duplicate host skill\n---\n# Check\n",
    )
    .expect("write host check skill");
    std::env::set_var("HOME", &temp_home);

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let response = runtime
        .block_on(async {
            let server = make_server();
            server
                .tachi_skill(Parameters(TachiSkillParams {
                    action: "discover".to_string(),
                    query: Some("check review ship".to_string()),
                    cap_type: None,
                    enabled_only: Some(true),
                    limit: Some(10),
                    skill_id: None,
                    args: None,
                    profile: None,
                    host: None,
                    skill_limit: None,
                    capability_limit: None,
                    pack_limit: None,
                    include_section: None,
                }))
                .await
        })
        .expect("tachi_skill discover should succeed");

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_dir_all(&temp_home);

    let json: Value = serde_json::from_str(&response).expect("skill discover response json");
    let ids = json["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|item| item.get("id").and_then(|id| id.as_str()))
        .collect::<Vec<_>>();
    assert!(
        ids.contains(&"skill:waza-check"),
        "expected Hub Waza skill in discover results: {json}"
    );
    assert!(
        !ids.contains(&"host-skill:check"),
        "host duplicate should be suppressed when Hub Waza skill exists: {json}"
    );
}
