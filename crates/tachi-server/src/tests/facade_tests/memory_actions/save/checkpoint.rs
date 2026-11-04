use super::*;

#[tokio::test]
async fn tachi_memory_checkpoint_saves_agent_checkpoint() {
    let (server, _project_db) = make_server_with_project_fixture("facade-checkpoint");

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            conversation_scope: None,
            auto_conversation_scope: true,
            multi_evidence_pack: true,
            action: "checkpoint".to_string(),
            issue_ref: None,
            format: Some("markdown".to_string()),
            query: None,
            scope: Some("project".to_string()),
            content_scope: None,
            storage_scope: None,
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
            include_superseded: false,
            synthesize: false,
            model: None,
            agent_role: None,
            text: Some("Implemented status diagnostics; next run cargo test.".to_string()),
            title: Some("Status diagnostics".to_string()),
            summary: Some("Status diagnostics checkpoint".to_string()),
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
            supersedes: Vec::new(),
            project: None,
            project_explicit: false,
            domain: Some("engineering".to_string()),
            metadata: None,
            compact: false,
            files: Vec::new(),
            references: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
        },
    )
    .await
    .expect("checkpoint should save");

    assert!(body.contains("Saved ->"));
    assert!(body.contains("status: saved"));
    assert!(body.contains("id: `"));
    assert!(
        !body.contains("Summary:"),
        "default checkpoint receipt should not echo input summary"
    );
    let id = body
        .split_once("id: `")
        .expect("checkpoint receipt ID")
        .1
        .split_once('`')
        .expect("closed checkpoint receipt ID")
        .0;
    assert!(server
        .with_project_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("read project checkpoint")
        .is_some());
    assert!(server
        .with_global_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("inspect global checkpoint absence")
        .is_none());
}
