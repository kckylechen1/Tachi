use super::*;

#[tokio::test]
async fn recommend_capability_skips_hidden_capabilities_by_default() {
    let server = make_server();
    let hidden = make_skill_capability(
        "skill:hidden-playbook",
        "hidden-playbook",
        "Handle sensitive internal incident playbooks.",
        "hidden",
    );
    let visible = make_skill_capability(
        "skill:incident-playbook",
        "incident-playbook",
        "Handle incident response playbooks.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&hidden).map_err(|e| e.to_string())?;
            store.hub_register(&visible).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register capabilities");

    let result = server
        .recommend_capability(Parameters(RecommendCapabilityParams {
            query: "incident playbook".to_string(),
            host: None,
            cap_type: Some("skill".to_string()),
            limit: 5,
            include_hidden: false,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_capability should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let ids = json["recommendations"]
        .as_array()
        .expect("recommendations array")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"skill:incident-playbook"));
    assert!(!ids.contains(&"skill:hidden-playbook"));

    let result = server
        .recommend_capability(Parameters(RecommendCapabilityParams {
            query: "incident playbook".to_string(),
            host: None,
            cap_type: Some("skill".to_string()),
            limit: 5,
            include_hidden: true,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_capability include_hidden should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    let ids = json["recommendations"]
        .as_array()
        .expect("recommendations array")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"skill:hidden-playbook"));
}

#[tokio::test]
async fn recommend_capability_limit_zero_normalizes_to_one() {
    let server = make_server();
    let first = make_skill_capability(
        "skill:incident-first",
        "incident-first",
        "Handle incident response runbooks and playbooks.",
        "listed",
    );
    let second = make_skill_capability(
        "skill:incident-second",
        "incident-second",
        "Handle incident retrospectives and follow-up actions.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&first).map_err(|e| e.to_string())?;
            store.hub_register(&second).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register capabilities");

    let result = server
        .recommend_capability(Parameters(RecommendCapabilityParams {
            query: "incident response".to_string(),
            host: None,
            cap_type: Some("skill".to_string()),
            limit: 0,
            include_hidden: false,
            include_uncallable: false,
        }))
        .await
        .expect("recommend_capability should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["count"], json!(1));
    assert_eq!(json["recommendations"].as_array().expect("array").len(), 1);
}
