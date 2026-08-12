use super::*;

#[tokio::test]
async fn memory_briefing_keeps_issue_scoped_presence_collision_warnings() {
    let claimed_server = make_server();
    let briefing_server =
        crate::server_state::MemoryServer::new(claimed_server.global_db_path_buf(), None)
            .expect("second server handle");

    claimed_server.set_session_identity(Some("seat-with-claim".to_string()), None, None);
    crate::claims_ops::auto_register_or_heartbeat_claim(
        &claimed_server,
        &crate::claims_ops::ClaimHookInput {
            issue_ref: Some("kckylechen1/tachi#1688".to_string()),
            flow_id: None,
            dispatch_id: None,
            branch: None,
            declared_file_scope: None,
        },
    );

    briefing_server.set_session_identity(Some("briefing-seat".to_string()), None, None);
    let params: TachiMemoryParams = serde_json::from_value(serde_json::json!({
        "action": "briefing",
        "format": "json",
        "issue_ref": "kckylechen1/tachi#1688",
        "compact": true
    }))
    .expect("scoped briefing payload deserializes");
    let response = crate::facade_memory_ops::handle_tachi_memory(&briefing_server, params)
        .await
        .expect("scoped briefing succeeds");
    let response: serde_json::Value =
        serde_json::from_str(&response).expect("briefing response is JSON");
    let warnings = response["presence"]["warnings"]
        .as_array()
        .expect("same-issue presence warnings are projected");

    assert_eq!(warnings.len(), 1, "response: {response:#}");
    assert!(
        warnings[0].as_str().is_some_and(
            |warning| warning.contains("double-claim") && warning.contains("seat-with-claim")
        ),
        "response: {response:#}"
    );
}

#[tokio::test]
async fn tachi_memory_briefing_includes_health_wiki_and_kanban_sections() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "briefing".to_string(),
            issue_ref: None,
            format: Some("markdown".to_string()),
            query: Some("current work".to_string()),
            scope: None,
            top_k: 3,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            agent_role: None,
            text: None,
            title: None,
            summary: None,
            topic: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            project_explicit: false,
            domain: None,
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
            references: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
            content: None,
            ingest_type: "source".to_string(),
            source_url: None,
            auto_chunk: true,
            auto_summarize: true,
            auto_link: true,
            chunk_size_chars: 1200,
            chunk_overlap_chars: 120,
            conversation_id: None,
            turn_id: None,
            event_type: None,
            messages: Vec::new(),
            to: None,
            ttl_days: None,
            include_read: false,
            agent_id: None,
        },
    )
    .await
    .expect("briefing should succeed");

    assert!(body.starts_with("## Tachi briefing"));
    assert!(
        body.contains("Layer authority: [AUTHORITY: docs/specs > guide/SOP > wiki > memory/eval]")
    );
    assert!(body.contains("### Memories (this project)"));
    assert!(body.contains("[AUTHORITY: LOW-MEDIUM]"));
    assert!(body.contains("### Health snapshot"));
    assert!(!body.contains("merge_hints"));
    assert!(!body.contains("skill_quality"));
}
