use super::*;

#[tokio::test]
async fn synthesize_agent_evolution_dry_run_loads_paths_and_memory_queries() {
    let server = make_server();
    let home = TempHomeGuard::new();
    let temp_dir = home
        .temp_home
        .join(".openclaw")
        .join(format!("tachi-foundry-input-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    let identity_path = temp_dir.join("IDENTITY.md");
    let eval_path = temp_dir.join("eval.md");
    std::fs::write(&identity_path, "# Identity\n\nMutable profile state").expect("write identity");
    std::fs::write(&eval_path, "Eval: tool routing drifted twice this week.").expect("write eval");

    let mut memory = make_entry("m_query");
    memory.path = "/openclaw/agent-yaya/tooluse".to_string();
    memory.topic = "tooluse".to_string();
    memory.summary = "Excel workflow succeeded via python and filesystem".to_string();
    memory.text =
        "The Excel workflow succeeded when the agent used python plus filesystem.".to_string();
    server
        .with_global_store(|store| store.upsert(&memory).map_err(|e| e.to_string()))
        .expect("seed query memory");

    let result = server
        .synthesize_agent_evolution(Parameters(SynthesizeAgentEvolutionParams {
            agent_id: "yaya".to_string(),
            display_name: Some("Yaya".to_string()),
            documents: Vec::new(),
            document_paths: vec![AgentEvolutionDocumentPathParams {
                kind: "identity".to_string(),
                path: identity_path.display().to_string(),
            }],
            evidence: Vec::new(),
            evidence_paths: vec![AgentEvolutionEvidencePathParams {
                kind: "eval".to_string(),
                path: eval_path.display().to_string(),
                title: Some("weekly eval".to_string()),
                source_ref: None,
                weight: 1.0,
            }],
            memory_queries: vec![AgentEvolutionMemoryQueryParams {
                query: "excel workflow".to_string(),
                title: Some("memory bundle".to_string()),
                path_prefix: Some("/openclaw/agent-yaya".to_string()),
                project: None,
                weight: 1.0,
                top_k: 3,
            }],
            goals: vec!["reduce routing drift".to_string()],
            dry_run: true,
        }))
        .await
        .expect("synthesize_agent_evolution dry_run should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], json!("dry_run"));
    assert_eq!(
        json["request"]["documents"]
            .as_array()
            .expect("documents array")
            .len(),
        1
    );
    assert_eq!(
        json["request"]["evidence"]
            .as_array()
            .expect("evidence array")
            .len(),
        2
    );
    assert!(json["request"]["documents"][0]["content"]
        .as_str()
        .unwrap_or("")
        .contains("Mutable profile state"));
    assert!(json["request"]["evidence"][1]["content"]
        .as_str()
        .unwrap_or("")
        .contains("Excel workflow succeeded"));

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn compact_session_memory_persists_rollup_and_signal_entries() {
    let (server, _temp_home) = make_server_with_temp_home();
    let result = server
        .compact_session_memory(Parameters(CompactSessionMemoryParams {
            agent_id: "main".to_string(),
            conversation_id: "conv-1".to_string(),
            window_id: "window-1".to_string(),
            compacted_text: "User prefers Tachi-managed memory and wants Excel-first workflows."
                .to_string(),
            salient_topics: vec!["memory".to_string(), "excel".to_string()],
            durable_signals: vec![
                "User prefers Tachi-managed memory.".to_string(),
                "Excel workflows should start with python plus filesystem.".to_string(),
            ],
            path_prefix: None,
            project: None,
            scope: "project".to_string(),
            importance: 0.7,
            queue_maintenance: false,
        }))
        .await
        .expect("compact_session_memory should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["status"], json!("completed"));
    assert_eq!(json["captured"].as_u64().unwrap_or(0), 3);
    assert!(json["section"]["block"]
        .as_str()
        .unwrap_or("")
        .contains("Durable Session Memory"));
}

#[tokio::test]
async fn list_agent_evolution_proposals_empty_result_accepts_zero_limit() {
    let server = make_server();

    let result = crate::foundry_ops::handle_list_agent_evolution_proposals(
        &server,
        ListAgentEvolutionProposalsParams {
            agent_id: "codex".to_string(),
            status: None,
            limit: 0,
        },
    )
    .await
    .expect("empty proposal list should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert_eq!(json["agent_id"], json!("codex"));
    assert_eq!(json["count"], json!(0));
    assert_eq!(json["proposals"].as_array().expect("array").len(), 0);
}

#[tokio::test]
async fn review_agent_evolution_proposal_rejects_invalid_status() {
    let server = make_server();

    let err = crate::foundry_ops::handle_review_agent_evolution_proposal(
        &server,
        ReviewAgentEvolutionProposalParams {
            proposal_id: "proposal-1".to_string(),
            status: "maybe".to_string(),
            note: None,
        },
    )
    .await
    .expect_err("invalid review status should fail");
    assert!(
        err.contains("Invalid review status"),
        "unexpected error: {err}"
    );
}
