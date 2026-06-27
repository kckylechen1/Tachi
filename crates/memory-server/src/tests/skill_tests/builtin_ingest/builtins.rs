use super::*;

#[tokio::test]
async fn server_seeds_builtin_capabilities_and_mcp_policies() {
    let server = make_server();

    let trajectory = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:trajectory-distiller")
                .map_err(|e| e.to_string())
        })
        .expect("lookup trajectory builtin");
    let coding = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:coding-architecture-decision")
                .map_err(|e| e.to_string())
        })
        .expect("lookup coding builtin");
    let trading = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:trading-position-snapshot")
                .map_err(|e| e.to_string())
        })
        .expect("lookup trading builtin");
    let mcp = server
        .with_global_store_read(|store| store.hub_get("mcp:web-search").map_err(|e| e.to_string()))
        .expect("lookup mcp builtin");
    let zread = server
        .with_global_store_read(|store| store.hub_get("mcp:zread").map_err(|e| e.to_string()))
        .expect("lookup zread builtin");
    let vision = server
        .with_global_store_read(|store| store.hub_get("mcp:vision").map_err(|e| e.to_string()))
        .expect("lookup vision builtin");
    let superpowers_execute = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:superpowers-executing-plans")
                .map_err(|e| e.to_string())
        })
        .expect("lookup superpowers executing builtin");
    let subagent_driven = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:superpowers-subagent-driven-development")
                .map_err(|e| e.to_string())
        })
        .expect("lookup subagent-driven builtin");
    let verification = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:superpowers-verification-before-completion")
                .map_err(|e| e.to_string())
        })
        .expect("lookup verification builtin");
    let waza_check = server
        .with_global_store_read(|store| {
            store.hub_get("skill:waza-check").map_err(|e| e.to_string())
        })
        .expect("lookup waza check builtin");

    let trajectory = trajectory.expect("trajectory-distiller builtin should exist");
    let coding = coding.expect("coding builtin should exist");
    let trading = trading.expect("trading builtin should exist");
    let mcp = mcp.expect("mcp builtin should exist");
    let zread = zread.expect("zread builtin should exist");
    let vision = vision.expect("vision builtin should exist");
    let superpowers_execute =
        superpowers_execute.expect("superpowers executing builtin should exist");
    let subagent_driven = subagent_driven.expect("subagent-driven builtin should exist");
    let verification = verification.expect("verification builtin should exist");
    let waza_check = waza_check.expect("waza check builtin should exist");

    let trajectory_def: Value =
        serde_json::from_str(&trajectory.definition).expect("trajectory definition json");
    let coding_def: Value =
        serde_json::from_str(&coding.definition).expect("coding definition json");
    let trading_def: Value =
        serde_json::from_str(&trading.definition).expect("trading definition json");
    let mcp_def: Value = serde_json::from_str(&mcp.definition).expect("mcp definition json");
    let zread_def: Value = serde_json::from_str(&zread.definition).expect("zread definition json");
    let vision_def: Value =
        serde_json::from_str(&vision.definition).expect("vision definition json");
    let superpowers_execute_def: Value = serde_json::from_str(&superpowers_execute.definition)
        .expect("superpowers executing definition json");
    let subagent_driven_def: Value =
        serde_json::from_str(&subagent_driven.definition).expect("subagent-driven definition json");
    let verification_def: Value =
        serde_json::from_str(&verification.definition).expect("verification definition json");
    let waza_check_def: Value =
        serde_json::from_str(&waza_check.definition).expect("waza check definition json");

    assert_eq!(trajectory_def["retention_policy"], "permanent");
    assert_eq!(coding_def["retention_policy"], "permanent");
    assert_eq!(trading_def["retention_policy"], "ephemeral");
    assert_eq!(superpowers_execute_def["retention_policy"], "permanent");
    assert_eq!(
        superpowers_execute_def["policy"]["visibility"],
        "discoverable"
    );
    assert!(superpowers_execute_def["content"]
        .as_str()
        .is_some_and(|content| content.contains("Executing Plans")));
    assert!(subagent_driven_def["content"]
        .as_str()
        .is_some_and(|content| content.contains("Subagent-Driven Development")));
    assert!(verification_def["content"]
        .as_str()
        .is_some_and(|content| content.contains("Verification Before Completion")));
    assert_eq!(waza_check_def["retention_policy"], "permanent");
    assert_eq!(waza_check_def["policy"]["visibility"], "discoverable");
    assert!(waza_check_def["content"]
        .as_str()
        .is_some_and(|content| content.contains("Review Before You Ship")));
    assert_eq!(mcp_def["auto_ingest"], true);
    assert!(mcp_def.get("auth_header").is_none());
    assert_eq!(mcp_def["auth"]["type"], "bearer");
    assert_eq!(
        mcp_def["auth"]["token"],
        "ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY"
    );
    assert_eq!(
        mcp_def["url"],
        "https://open.bigmodel.cn/api/mcp/web_search_prime/mcp"
    );
    assert_eq!(
        zread_def["url"],
        "https://open.bigmodel.cn/api/mcp/zread/mcp"
    );
    assert_eq!(vision_def["transport"], "stdio");
    assert_eq!(vision_def["command"], "npx");
    assert_eq!(vision_def["args"][0], "-y");
    assert_eq!(vision_def["args"][1], "@z_ai/mcp-server@latest");
    assert_eq!(
        vision_def["env"]["Z_AI_API_KEY"],
        "${vault:ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY}"
    );
    assert_eq!(vision_def["env"]["Z_AI_MODE"], "ZAI");

    let policy = server
        .with_global_store_read(|store| {
            store
                .get_sandbox_policy("mcp:web-search")
                .map_err(|e| e.to_string())
        })
        .expect("lookup builtin sandbox policy");
    assert!(policy.is_some(), "builtin MCP should seed sandbox policy");
}
