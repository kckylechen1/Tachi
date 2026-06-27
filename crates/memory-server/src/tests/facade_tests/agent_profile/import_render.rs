use super::*;

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
