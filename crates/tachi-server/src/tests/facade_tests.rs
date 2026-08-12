use super::{
    ensure_test_env, make_entry, make_server, make_server_with_temp_home, seed_wiki_project_entries,
};
use crate::tool_params::{
    InitProjectDbParams, TachiDomainAdapterParams, TachiEventParams, TachiMemoryParams,
    TachiSearchParams, TachiTaskParams, TachiTuneParams,
};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

fn tachi_memory_params(action: &str) -> TachiMemoryParams {
    TachiMemoryParams {
        action: action.to_string(),
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
        metadata: None,
        emit_continuity: false,
        files: Vec::new(),
        references: Vec::new(),
        flow_id: None,
        event: None,
        state: None,
        project: None,
        project_explicit: false,
        domain: None,
        compact: false,
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
    }
}

fn tachi_tune_params(action: &str) -> TachiTuneParams {
    TachiTuneParams {
        action: action.parse().expect("valid tachi_tune action"),
        format: Some("markdown".to_string()),
        task: None,
        execution_level: None,
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
        risk: None,
        limit: None,
        state_filter: None,
        proposal_id: None,
        review_status: None,
        notes: None,
        confirm: false,
        top_k: 6,
        metadata: None,
        text: None,
        enable_rerank: false,
        scope: None,
        path_prefix: None,
        project: None,
        domain: None,
        file_context: None,
        error_context: None,
        include_archived: false,
        include_training: false,
        force: false,
        as_of: None,
    }
}

async fn handle_tachi_tune_for_test(
    server: &crate::MemoryServer,
    params: TachiTuneParams,
) -> Result<String, String> {
    server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
    crate::tune_ops::handle_tachi_tune(server, params).await
}

fn session_claim_snapshot(server: &crate::MemoryServer) -> (Vec<memcore::SessionClaim>, i64) {
    server
        .with_global_store_read(|store| {
            let claims = memcore::list_claims(store.connection(), None)
                .map_err(|error| error.to_string())?;
            let total_changes = store
                .connection()
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .map_err(|error| error.to_string())?;
            Ok((claims, total_changes))
        })
        .expect("snapshot session-claim rows")
}

#[tokio::test]
async fn retired_memory_claim_release_are_rejected_without_mutating_workclaim_rows() {
    let server = make_server();
    crate::claims_ops::admit_agent_connection(&server, Some("agent.c2a1".to_string()), true)
        .expect("admit seed identity");
    server
        .with_global_store(|store| {
            memcore::insert_work_claim(
                store.connection_mut(),
                &memcore::NewWorkClaim {
                    claim_id: "c2a1-canonical-workclaim".to_string(),
                    agent_identity_id: "agent.c2a1".to_string(),
                    session_client: Some("work-claim:c2a1-canonical-workclaim".to_string()),
                    issue_ref: Some("kckylechen1/tachi#1688".to_string()),
                    flow_id: Some("flow-c2a1".to_string()),
                    dispatch_id: Some("dispatch-c2a1".to_string()),
                    branch: "leaf/1688-memory-claim-release".to_string(),
                    worktree_path: "/tmp/tachi-c2a1".to_string(),
                    declared_file_scope: r#"["crates/tachi-server/src/facade_memory_ops/mod.rs"]"#
                        .to_string(),
                    role: "implementer".to_string(),
                    mode: memcore::WorkClaimMode::ReadOnly,
                    expected_head: "03b6b600".to_string(),
                    lease_expires_at: "2030-01-01T00:00:00Z".to_string(),
                    created_at: "2026-08-12T00:00:00Z".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
            memcore::insert_claim(
                store.connection(),
                &memcore::NewSessionClaim {
                    claim_id: "c2a1-historical-presence".to_string(),
                    session_client: Some("legacy-c2a1".to_string()),
                    issue_ref: Some("kckylechen1/tachi#1688".to_string()),
                    flow_id: Some("flow-c2a1".to_string()),
                    dispatch_id: Some("dispatch-c2a1".to_string()),
                    branch: "legacy/c2a1".to_string(),
                    declared_file_scope: Some(
                        r#"["crates/tachi-server/src/claims_ops.rs"]"#.to_string(),
                    ),
                    created_at: "2026-08-11T00:00:00Z".to_string(),
                },
            )
            .map_err(|error| error.to_string())
        })
        .expect("seed canonical and historical claim rows");

    let before = session_claim_snapshot(&server);
    for action in ["claim", "release"] {
        let mut params = tachi_memory_params(action);
        params.format = Some("json".to_string());
        params.flow_id = Some("flow-c2a1".to_string());
        let error = crate::facade_memory_ops::handle_tachi_memory(&server, params)
            .await
            .expect_err("retired Memory claim/release must be rejected");
        assert!(error.contains("tachi_task"), "{action} guidance: {error}");
        assert_eq!(
            session_claim_snapshot(&server),
            before,
            "retired Memory action={action} must not mutate canonical or historical claim rows"
        );
    }
}

mod briefing;
mod cli_daemon;
mod domain_adapter;
mod event;
mod fold_757;
mod memory_actions;
mod memory_search;
mod receipt_golden;
mod status;
