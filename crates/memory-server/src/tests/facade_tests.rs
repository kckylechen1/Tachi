use super::{
    ensure_test_env, make_entry, make_server, make_server_with_temp_home, seed_wiki_project_entries,
};
use crate::tool_params::{
    InitProjectDbParams, TachiEventParams, TachiMemoryParams, TachiSearchParams, TachiSkillParams,
    TachiTaskParams,
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
        flow_id: None,
        event: None,
        state: None,
        project: None,
        domain: None,
        compact: false,
    }
}

mod briefing;
mod cli_daemon;
mod event;
mod memory_actions;
mod memory_search;
mod schema;
mod status;
