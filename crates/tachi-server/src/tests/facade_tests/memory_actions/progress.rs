use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_memory_progress_writes_append_only_jsonl() {
    let (server, temp_home) = make_server_with_temp_home();
    let run_root = temp_home.temp_home.join("runs");
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "progress".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            scope: None,
            top_k: 6,
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
            text: Some("step completed with api_key=test-secret-value-1234567890".to_string()),
            title: Some("Progress step".to_string()),
            summary: Some("one step done".to_string()),
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
            flow_id: Some("flow_progress_test".to_string()),
            event: Some("validation".to_string()),
            state: Some("running".to_string()),
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
            to: None,
            ttl_days: None,
            include_read: false,
            agent_id: None,
        },
    )
    .await
    .expect("progress should record");

    assert!(body.starts_with("## Tachi progress"));
    assert!(body.contains("status: recorded"));
    assert!(body.contains("secret_redactions: 1"));
    let log = std::fs::read_to_string(run_root.join("flow_progress_test/progress.jsonl"))
        .expect("progress jsonl");
    assert!(log.contains("validation"));
    assert!(!log.contains("test-secret-value-1234567890"));
    if let Some(original) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", original);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
