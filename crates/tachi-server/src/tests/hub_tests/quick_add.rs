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
async fn hub_register_and_quick_add_refuse_retired_mcp_and_virtual_cap_types() {
    let server = make_server();
    for cap_type in ["mcp", "virtual"] {
        let definition = serde_json::json!({
            "transport": "stdio",
            "command": "docker",
            "args": ["--version"]
        })
        .to_string();
        let register_err = crate::hub_ops::handle_hub_register(
            &server,
            crate::tool_params::HubRegisterParams {
                id: format!("{cap_type}:retired"),
                cap_type: cap_type.to_string(),
                name: format!("retired {cap_type}"),
                description: String::new(),
                definition: definition.clone(),
                version: 1,
                scope: "global".to_string(),
            },
        )
        .await
        .expect_err("retired cap_type must be refused at register");
        assert!(
            register_err.contains("retired"),
            "{cap_type}: register error must name the retirement: {register_err}"
        );
        let quick_add_err = crate::hub_ops::handle_hub_quick_add(
            &server,
            crate::tool_params::HubQuickAddParams {
                id: format!("{cap_type}:retired"),
                cap_type: cap_type.to_string(),
                name: format!("retired {cap_type}"),
                description: String::new(),
                definition,
                version: 1,
                scope: "global".to_string(),
                auto_approve: true,
            },
        )
        .await
        .expect_err("retired cap_type must be refused at quick_add");
        assert!(
            quick_add_err.contains("retired"),
            "{cap_type}: quick_add error must name the retirement: {quick_add_err}"
        );
    }
}
