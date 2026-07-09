use super::super::super::helpers::{
    build_section_artifact, dedup_strings, estimate_token_count, SectionArtifactInput,
};
use super::super::super::recall::run_compaction_model;
use crate::server_state::MemoryServer;
use crate::tool_params::CompactRollupParams;
use serde_json::json;

pub(crate) async fn handle_compact_rollup(
    server: &MemoryServer,
    params: CompactRollupParams,
) -> Result<String, String> {
    let items = params
        .items
        .iter()
        .filter(|item| !item.compacted_text.trim().is_empty())
        .collect::<Vec<_>>();
    let current_summary = params.current_summary.as_deref().unwrap_or("").trim();

    if items.is_empty() && current_summary.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_rollup",
            "rollup_id": params.rollup_id,
            "compacted_text": "",
            "estimated_tokens": 0,
            "salient_topics": [],
            "durable_signals": [],
        }))
        .map_err(|e| format!("Failed to serialize compact_rollup response: {e}"));
    }

    let payload = json!({
        "agent_id": params.agent_id,
        "conversation_id": params.conversation_id,
        "rollup_id": params.rollup_id,
        "target_tokens": params.target_tokens.max(32),
        "current_summary": params.current_summary,
        "path_prefix": params.path_prefix,
        "project": params.project,
        "items": items
            .iter()
            .map(|item| json!({
                "item_id": item.item_id,
                "window_id": item.window_id,
                "compacted_text": item.compacted_text,
                "salient_topics": item.salient_topics,
                "durable_signals": item.durable_signals,
            }))
            .collect::<Vec<_>>(),
    });
    let draft = match run_compaction_model(
        server,
        crate::prompts::COMPACT_ROLLUP_PROMPT,
        &payload,
        params.max_output_tokens,
    )
    .await
    {
        Ok(draft) => draft,
        Err(err) => {
            return serde_json::to_string(&json!({
                "status": "failed",
                "reason": "llm_compaction_failed",
                "error": err,
                "agent_id": params.agent_id,
                "conversation_id": params.conversation_id,
                "rollup_id": params.rollup_id,
                "compacted_text": "",
                "estimated_tokens": 0,
                "target_tokens": params.target_tokens.max(32),
                "salient_topics": [],
                "durable_signals": [],
                "source_item_count": items.len(),
                "section": null,
            }))
            .map_err(|e| format!("Failed to serialize compact_rollup response: {e}"));
        }
    };
    let compacted_text = draft.compacted_text.trim().to_string();
    let estimated_tokens = estimate_token_count(&compacted_text);
    let source_refs = items
        .iter()
        .filter_map(|item| item.window_id.clone().or_else(|| item.item_id.clone()))
        .collect::<Vec<_>>();
    let section = if params.build_section && !compacted_text.is_empty() {
        Some(build_section_artifact(SectionArtifactInput {
            layer: "session",
            kind: "compact_rollup",
            title: Some("Session Rollup"),
            content: &compacted_text,
            items: &draft.durable_signals,
            cache_boundary: "session",
            source_refs: &source_refs,
            target_tokens: Some(params.target_tokens),
        }))
    } else {
        None
    };

    serde_json::to_string(&json!({
        "status": if compacted_text.is_empty() { "skipped" } else { "completed" },
        "agent_id": params.agent_id,
        "conversation_id": params.conversation_id,
        "rollup_id": params.rollup_id,
        "compacted_text": compacted_text,
        "estimated_tokens": estimated_tokens,
        "target_tokens": params.target_tokens.max(32),
        "salient_topics": dedup_strings(draft.salient_topics),
        "durable_signals": dedup_strings(draft.durable_signals),
        "source_item_count": items.len(),
        "section": section,
    }))
    .map_err(|e| format!("Failed to serialize compact_rollup response: {e}"))
}
