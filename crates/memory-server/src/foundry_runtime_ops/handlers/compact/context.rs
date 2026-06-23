use super::super::super::helpers::{dedup_strings, estimate_token_count};
use super::super::super::recall::run_compaction_model;
use crate::server_state::MemoryServer;
use crate::tool_params::CompactContextParams;
use serde_json::{json, Value};

pub(crate) async fn handle_compact_context(
    server: &MemoryServer,
    params: CompactContextParams,
) -> Result<String, String> {
    let combined_text = params
        .messages
        .iter()
        .map(|message| format!("{}: {}", message.role.trim(), message.content.trim()))
        .collect::<Vec<_>>()
        .join("\n");

    if combined_text.trim().is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_messages",
            "compacted_text": "",
            "estimated_tokens": 0,
            "captured_memory_ids": [],
            "queued_job_ids": [],
        }))
        .map_err(|e| format!("Failed to serialize compact_context response: {e}"));
    }

    let payload = json!({
        "agent_id": params.agent_id,
        "conversation_id": params.conversation_id,
        "window_id": params.window_id,
        "trigger": params.trigger,
        "target_tokens": params.target_tokens.max(32),
        "current_summary": params.current_summary,
        "path_prefix": params.path_prefix,
        "project": params.project,
        "messages": params.messages,
    });
    let draft = match run_compaction_model(
        server,
        crate::prompts::COMPACT_CONTEXT_PROMPT,
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
                "trigger": params.trigger,
                "conversation_id": params.conversation_id,
                "window_id": params.window_id,
                "compacted_text": "",
                "estimated_tokens": 0,
                "target_tokens": params.target_tokens.max(32),
                "salient_topics": [],
                "durable_signals": [],
                "captured_memory_ids": [],
                "queued_job_ids": [],
            }))
            .map_err(|e| format!("Failed to serialize compact_context response: {e}"));
        }
    };
    let compacted_text = draft.compacted_text.trim().to_string();
    let estimated_tokens = estimate_token_count(&compacted_text);

    let status = if compacted_text.is_empty() {
        "skipped"
    } else {
        "completed"
    };

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!(status));
    response.insert("trigger".into(), json!(params.trigger));
    response.insert("conversation_id".into(), json!(params.conversation_id));
    response.insert("window_id".into(), json!(params.window_id));
    response.insert("compacted_text".into(), json!(compacted_text));
    response.insert("estimated_tokens".into(), json!(estimated_tokens));
    response.insert("target_tokens".into(), json!(params.target_tokens.max(32)));
    response.insert(
        "salient_topics".into(),
        json!(dedup_strings(draft.salient_topics)),
    );
    response.insert(
        "durable_signals".into(),
        json!(dedup_strings(draft.durable_signals)),
    );
    response.insert("captured_memory_ids".into(), json!(Vec::<String>::new()));
    response.insert("queued_job_ids".into(), json!(Vec::<String>::new()));
    if params.persist {
        response.insert("warning".into(), json!("persist_requested_but_deferred"));
    }

    serde_json::to_string(&Value::Object(response))
        .map_err(|e| format!("Failed to serialize compact_context response: {e}"))
}
