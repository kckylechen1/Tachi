use super::*;

// ─── PR6: hub_quick_add safety boundary ─────────────────────────────────────

#[tokio::test]
async fn hub_quick_add_skill_auto_approve_is_noop_already_enabled() {
    let server = make_server();
    let body = crate::hub_ops::handle_hub_quick_add(
        &server,
        crate::tool_params::HubQuickAddParams {
            id: "skill:pr6-test".to_string(),
            cap_type: "skill".to_string(),
            name: "pr6 test skill".to_string(),
            description: "A trivial skill for the PR6 quick_add test.".to_string(),
            definition: serde_json::json!({"prompt": "echo {{x}}"}).to_string(),
            version: 1,
            scope: "global".to_string(),
            auto_approve: true,
        },
    )
    .await
    .expect("quick_add");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        v["auto_approve"], json!("already_enabled"),
        "skills are governance-approved at register time; auto_approve must be a no-op. body: {body}"
    );
    assert!(v.get("review").is_none(), "no review step should run");
}

#[tokio::test]
async fn hub_quick_add_refuses_to_auto_approve_untrusted_stdio_mcp() {
    let server = make_server();
    let definition = serde_json::json!({
        "transport": "stdio",
        "command": "/tmp/definitely-not-on-allowlist",
        "args": []
    })
    .to_string();
    let body = crate::hub_ops::handle_hub_quick_add(
        &server,
        crate::tool_params::HubQuickAddParams {
            id: "mcp:pr6-untrusted".to_string(),
            cap_type: "mcp".to_string(),
            name: "untrusted mcp".to_string(),
            description: String::new(),
            definition,
            version: 1,
            scope: "global".to_string(),
            auto_approve: true,
        },
    )
    .await
    .expect("quick_add");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        v["auto_approve"], json!("refused_untrusted"),
        "untrusted stdio MCP must NOT be auto-approved even when explicitly requested. body: {body}"
    );
    // The register step must still report the cap as pending+disabled.
    assert_eq!(v["register"]["enabled"], json!(false));
    assert_eq!(v["register"]["review_status"], json!("pending"));
    assert_eq!(v["register"]["auto_approval_eligible"], json!(false));
    // No review step should have run.
    assert!(
        v.get("review").is_none(),
        "untrusted path must not invoke review"
    );
    // A safety warning should be present in the response (warnings are
    // appended via `append_warning` which concatenates into a single "warning"
    // string field, not an array).
    let warning = v.get("warning").and_then(|w| w.as_str()).unwrap_or("");
    assert!(
        warning.contains("trusted allowlist"),
        "expected an allowlist warning, got warning={warning:?}, body: {body}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn hub_quick_add_applies_review_for_trusted_stdio_mcp() {
    // This test spawns `npx -y @modelcontextprotocol/server-everything` for
    // real (the trusted-allowlist→discovery→enable path is the whole point).
    // Interpreters such as npx are no longer auto-approved for MCP; use a
    // container runtime (docker) as the trusted stdio command. Discovery may
    // still fail locally, in which case review.rs marks it unhealthy/disabled.
    let _home_guard = acquire_real_home_lock();
    let server = make_server();
    let definition = serde_json::json!({
        "transport": "stdio",
        "command": "docker",
        "args": ["--version"]
    })
    .to_string();
    let body = crate::hub_ops::handle_hub_quick_add(
        &server,
        crate::tool_params::HubQuickAddParams {
            id: "mcp:pr6-trusted".to_string(),
            cap_type: "mcp".to_string(),
            name: "trusted mcp".to_string(),
            description: String::new(),
            definition,
            version: 1,
            scope: "global".to_string(),
            auto_approve: true,
        },
    )
    .await
    .expect("quick_add");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        v["register"]["auto_approval_eligible"],
        json!(true),
        "body: {body}"
    );
    assert_eq!(v["auto_approve"], json!("applied"), "body: {body}");
    // The review sub-response must reflect the approval transition. Discovery
    // can still fail in local/CI environments, in which case review disables
    // the MCP capability and reports it unhealthy.
    assert_eq!(v["review"]["review_status"], json!("approved"));
    if v["review"]["health_status"] == json!("healthy") {
        assert_eq!(v["review"]["enabled"], json!(true), "body: {body}");
    } else {
        assert_eq!(v["review"]["enabled"], json!(false), "body: {body}");
    }
}
