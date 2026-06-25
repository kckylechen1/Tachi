use super::*;

fn profile_params(action: &str) -> TachiProfileParams {
    TachiProfileParams {
        action: action.to_string(),
        agent_id: Some("codex".to_string()),
        display_name: Some("Codex".to_string()),
        target: None,
        targets: Vec::new(),
        documents: Vec::new(),
        document_paths: Vec::new(),
        pack: None,
        project: None,
        role: None,
        session_kind: None,
        include_private_user: false,
        include_continuity: false,
        max_chars: 4_000,
        dry_run: true,
    }
}

#[tokio::test]
async fn profile_import_builds_pack_from_agent_docs() {
    let server = make_server();
    let mut params = profile_params("import");
    params.documents = vec![
        TachiProfileDocumentParams {
            kind: "identity".to_string(),
            path: Some("IDENTITY.md".to_string()),
            content: "- Name: Codex\n- Vibe: direct engineering agent\n".to_string(),
        },
        TachiProfileDocumentParams {
            kind: "agents".to_string(),
            path: Some("AGENTS.md".to_string()),
            content: r#"
# Agent Rules

- For non-trivial work, call tachi_memory(action="briefing").
- Run verification before claiming completion.
- Keep progress concise and concrete.
"#
            .to_string(),
        },
    ];

    let body = crate::agent_profile_ops::handle_tachi_profile(&server, params)
        .await
        .expect("profile import");
    let parsed: Value = serde_json::from_str(&body).expect("profile JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["pack"]["identity"]["name"], json!("Codex"));
    assert_eq!(parsed["pack"]["memory_policy"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["pack"]["quality_bar"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn profile_render_returns_agent_markdown_targets_without_writing() {
    let server = make_server();
    let mut params = profile_params("render");
    params.targets = vec![
        "codex_agents".to_string(),
        "claude_md".to_string(),
        "gemini_md".to_string(),
        "cursor_mdc".to_string(),
    ];
    params.documents = vec![TachiProfileDocumentParams {
        kind: "agents".to_string(),
        path: Some("AGENTS.md".to_string()),
        content: "- Keep answers short.\n- Run tests before claiming completion.\n".to_string(),
    }];

    let body = crate::agent_profile_ops::handle_tachi_profile(&server, params)
        .await
        .expect("profile render");
    let parsed: Value = serde_json::from_str(&body).expect("profile JSON");
    let docs = parsed["documents"].as_array().expect("documents");
    assert_eq!(docs.len(), 4);
    let filenames = docs
        .iter()
        .filter_map(|doc| doc["filename"].as_str())
        .collect::<Vec<_>>();
    assert!(filenames.contains(&"AGENTS.md"));
    assert!(filenames.contains(&"CLAUDE.md"));
    assert!(filenames.contains(&"GEMINI.md"));
    assert!(filenames.contains(&"tachi-profile.mdc"));
    for doc in docs {
        assert_eq!(doc["dry_run"], json!(true));
        assert!(doc["content"].as_str().unwrap().contains("Tachi MCP"));
        assert!(doc["content"]
            .as_str()
            .unwrap()
            .contains("AgentProfilePack"));
    }
}

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
async fn profile_rejects_non_dry_run_calls() {
    let server = make_server();
    let mut params = profile_params("render");
    params.dry_run = false;
    params.documents = vec![TachiProfileDocumentParams {
        kind: "agents".to_string(),
        path: None,
        content: "- Keep answers short.\n".to_string(),
    }];

    let err = crate::agent_profile_ops::handle_tachi_profile(&server, params)
        .await
        .expect_err("non-dry-run profile calls are not supported");
    assert!(err.contains("dry_run=false is not supported"));
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
