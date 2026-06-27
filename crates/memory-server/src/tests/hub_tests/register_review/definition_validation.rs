use super::*;

#[tokio::test]
async fn hub_register_skill_rejects_invalid_definition_json() {
    let server = make_server();

    let params = HubRegisterParams {
        id: "skill:bad-json".to_string(),
        cap_type: "skill".to_string(),
        name: "bad-json".to_string(),
        description: "invalid skill json".to_string(),
        definition: "{\"prompt\":\"line\nbreak\"}".to_string(),
        version: 1,
        scope: "global".to_string(),
    };

    let err = server
        .hub_register(Parameters(params))
        .await
        .expect_err("invalid skill definition JSON should be rejected");
    assert!(
        err.contains("invalid skill definition JSON"),
        "unexpected error: {err}"
    );

    let missing = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:bad-json")
                .map_err(|e| format!("hub get: {e}"))
        })
        .expect("hub get should succeed");
    assert!(missing.is_none(), "invalid skill should not be persisted");
}

#[tokio::test]
async fn hub_register_rejects_oversized_definition_before_persisting() {
    let server = make_server();

    let params = HubRegisterParams {
        id: "skill:oversized-definition".to_string(),
        cap_type: "skill".to_string(),
        name: "oversized definition".to_string(),
        description: "definition should be size capped".to_string(),
        definition: format!(
            "{{\"prompt\":\"{}\",\"inputSchema\":{{\"type\":\"object\"}}}}",
            "x".repeat(300 * 1024)
        ),
        version: 1,
        scope: "global".to_string(),
    };

    let err = server
        .hub_register(Parameters(params))
        .await
        .expect_err("oversized definition should be rejected");
    assert!(
        err.contains("definition is too large"),
        "unexpected error: {err}"
    );

    let missing = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:oversized-definition")
                .map_err(|e| format!("hub get: {e}"))
        })
        .expect("hub get should succeed");
    assert!(
        missing.is_none(),
        "oversized definition should not be persisted"
    );
}
