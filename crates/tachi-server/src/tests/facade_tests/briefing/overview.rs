use super::*;

#[tokio::test]
async fn tachi_memory_briefing_includes_health_wiki_and_kanban_sections() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "briefing".to_string(),
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
            domain: None,
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
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
            issue_ref: None,
            branch: None,
            declared_file_scope: Vec::new(),
            claim_id: None,
            dispatch_id: None,
            release_reason: None,
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
