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

mod briefing;
mod cli_daemon;
mod domain_adapter;
mod event;
mod fold_757;
mod memory_actions;
mod memory_search;
mod receipt_golden;
mod status;
