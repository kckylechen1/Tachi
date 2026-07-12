use super::*;

#[tokio::test]
async fn tachi_memory_briefing_defaults_to_named_wiki_project_hits() {
    let mut entry = make_entry("briefing-default-wiki-hit");
    entry.path = "/wiki/agent/tachi/briefing-default".to_string();
    entry.summary = "Briefing default wiki hit".to_string();
    entry.text =
        "BriefingDefaultWikiNeedle should appear in default briefing wiki rows.".to_string();
    entry.entities = vec!["BriefingDefaultWikiNeedle".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "briefing".to_string(),
            format: Some("json".to_string()),
            query: Some("BriefingDefaultWikiNeedle".to_string()),
            scope: None,
            top_k: 5,
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

    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");
    assert!(
        parsed["wiki"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row["id"] == json!("briefing-default-wiki-hit")
                    || row["path"] == json!("/wiki/agent/tachi/briefing-default")
            })),
        "expected default briefing wiki rows to include project:wiki hit, got: {parsed}"
    );
}
