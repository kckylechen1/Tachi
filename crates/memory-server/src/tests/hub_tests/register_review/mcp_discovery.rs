use super::*;

#[tokio::test]
async fn hub_register_defers_mcp_discovery_until_review() {
    let server = make_server();
    let params = HubRegisterParams {
        id: "mcp:discovery-fails".to_string(),
        cap_type: "mcp".to_string(),
        name: "discovery-fails".to_string(),
        description: "test discovery failure".to_string(),
        definition: json!({
            "transport": "stdio",
            "command": "/tmp/not-on-allowlist",
            "args": [],
        })
        .to_string(),
        version: 1,
        scope: "global".to_string(),
    };

    let response = server
        .hub_register(Parameters(params))
        .await
        .expect("hub_register should return response");
    let data: serde_json::Value =
        serde_json::from_str(&response).expect("hub_register response should be JSON");

    assert_eq!(data.get("enabled"), Some(&json!(false)));
    assert_eq!(data.get("review_status"), Some(&json!("pending")));
    assert_eq!(data.get("discovery"), Some(&json!("deferred")));

    let cap = server
        .get_capability("mcp:discovery-fails")
        .expect("capability should be persisted");
    assert!(!cap.enabled, "capability should stay disabled until review");

    let def: serde_json::Value =
        serde_json::from_str(&cap.definition).expect("stored definition should be valid JSON");
    assert!(
        def.get("discovery_status").is_none(),
        "registration should not persist discovery results before approval"
    );

    assert!(
        !lock_or_recover(&server.tool_discovery.proxy_tools, "proxy_tools")
            .contains_key("discovery-fails"),
        "pending capability should not cache proxy tools"
    );
}
