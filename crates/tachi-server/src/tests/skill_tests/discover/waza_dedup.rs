use super::*;

#[test]
fn tachi_skill_discover_prefers_hub_waza_skill_over_host_duplicate() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original_home = std::env::var_os("HOME");
    let temp_home =
        crate::utils::test_fixture_path(format!("tachi-waza-dedup-test-{}", uuid::Uuid::new_v4()));
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
