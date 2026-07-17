use super::super::super::helpers::{dedup_strings, estimate_token_count};
use super::super::super::recall::run_compaction_model;
use crate::server_state::MemoryServer;
use crate::tool_params::CompactContextParams;
use serde_json::{json, Value};

/// #1099: `compact_context` never persists anything — it only drafts a
/// compacted text block. `persist=true` used to return a nominal-success
/// response (with two always-empty `captured_memory_ids`/`queued_job_ids`
/// fields) plus a `persist_requested_but_deferred` warning that most callers
/// never inspect. That shape lets a caller believe persistence happened when
/// nothing was ever written — the forbidden "nominal success with no
/// persistence" contract. Refuse loudly instead, before any compaction work
/// runs, and point at the real persistence path.
/// Public so the MCP facade (`tools/runtime_context_facade.rs`) can enforce
/// this refusal before any daemon forwarding is attempted — persist=true must
/// never reach a possibly-stale daemon, not just the in-process handler below.
pub(crate) const PERSIST_REFUSAL: &str = "compact_context does not persist memories and never has \
    (the response fields describing capture/queue ids were always empty placeholders). \
    persist=true is refused. Call compact_session_memory to actually persist a compacted \
    window, or omit persist / pass persist=false for a stateless compaction preview.";

pub(crate) async fn handle_compact_context(
    server: &MemoryServer,
    params: CompactContextParams,
) -> Result<String, String> {
    if params.persist {
        return Err(PERSIST_REFUSAL.to_string());
    }

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

    serde_json::to_string(&Value::Object(response))
        .map_err(|e| format!("Failed to serialize compact_context response: {e}"))
}
