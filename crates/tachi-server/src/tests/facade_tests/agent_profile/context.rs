use super::*;

#[tokio::test]
async fn profile_context_suppresses_user_model_for_subagents() {
    let server = make_server();
    let mut params = profile_params("context");
    params.session_kind = Some("subagent".to_string());
    params.include_private_user = true;
    params.documents = vec![TachiProfileDocumentParams {
        kind: "user".to_string(),
        path: Some("USER.md".to_string()),
        content: "- Kyle prefers private context to stay in main sessions.\n".to_string(),
    }];

    let body = crate::agent_profile_ops::handle_tachi_profile(&server, params)
        .await
        .expect("profile context");
    let parsed: Value = serde_json::from_str(&body).expect("profile JSON");
    let context = parsed["prepend_context"].as_str().expect("context");
    assert!(context.contains("User Model"));
    assert!(context.contains("suppressed for this session kind"));
    assert!(!context.contains("private context to stay in main sessions"));
}

#[tokio::test]
async fn profile_context_includes_projected_continuity_read_model() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut pattern = make_entry("profile-continuity-pattern");
            pattern.path = "/user/patterns/agent_os/continuity-first".to_string();
            pattern.summary = "Continuity-first project management".to_string();
            pattern.text =
                "Use projected continuity before picking the next agent action.".to_string();
            pattern.metadata = json!({
                "projection_kind": "pattern",
                "projection_key": "continuity-first",
                "counters": {"seen": 3, "hit": 1, "miss": 0}
            });
            store.upsert(&pattern).map_err(|e| e.to_string())
        })
        .expect("seed projected pattern");

    let mut params = profile_params("context");
    params.include_continuity = true;
    params.documents = vec![TachiProfileDocumentParams {
        kind: "agents".to_string(),
        path: Some("AGENTS.md".to_string()),
        content: "- Run verification before claiming completion.\n".to_string(),
    }];

    let body = crate::agent_profile_ops::handle_tachi_profile(&server, params)
        .await
        .expect("profile context");
    let parsed: Value = serde_json::from_str(&body).expect("profile JSON");
    let context = parsed["prepend_context"].as_str().expect("context");
    assert!(context.contains("Continuity Read Model"));
    assert!(context.contains("Continuity-first project management"));
    assert!(context.contains("affect, when present, is tone/reminder only"));
}

#[tokio::test]
async fn profile_context_truncates_multibyte_text_on_char_boundary() {
    let server = make_server();
    let mut params = profile_params("context");
    params.max_chars = 120;
    params.documents = vec![TachiProfileDocumentParams {
        kind: "agents".to_string(),
        path: None,
        content: "- 这个配置会统一不同 agent 的说话方式、工具边界、记忆策略和验证习惯。\n"
            .repeat(8),
    }];

    let body = crate::agent_profile_ops::handle_tachi_profile(&server, params)
        .await
        .expect("profile context");
    let parsed: Value = serde_json::from_str(&body).expect("profile JSON");
    let context = parsed["prepend_context"].as_str().expect("context");
    assert!(context.len() <= 120);
    assert!(context.contains("truncated by tachi_profile max_chars"));
}
