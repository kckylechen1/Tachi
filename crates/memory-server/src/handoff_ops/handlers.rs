use crate::server_state::{HandoffMemo, MemoryServer};
use crate::tool_params::{HandoffCheckParams, HandoffLeaveParams};
use chrono::Utc;
use memory_core::MemoryEntry;
use serde_json::json;

use super::identity::resolve_from_agent;
use super::memo::{memo_from_entry, memo_matches_agent, memo_to_memory_entry};
use super::pending::{
    pending_handoff_entries, supersede_pending_handoffs, upsert_acknowledged_entry,
};
use super::HANDOFF_MEMORY_LIMIT;

pub(crate) async fn handle_handoff_leave(
    server: &MemoryServer,
    params: HandoffLeaveParams,
) -> Result<String, String> {
    let from_agent = resolve_from_agent(server);

    let memo = HandoffMemo {
        id: uuid::Uuid::new_v4().to_string(),
        from_agent: from_agent.clone(),
        target_agent: params.target_agent,
        summary: params.summary,
        next_steps: params.next_steps,
        context: params.context,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    };

    let memo_id = memo.id.clone();
    let memo_json = serde_json::to_string(&memo).map_err(|e| format!("serialize: {e}"))?;
    let entry = memo_to_memory_entry(server, &memo);

    server.with_global_store(|store| {
        supersede_pending_handoffs(store, &memo, &entry)?;
        store.upsert(&entry).map_err(|e| format!("{e}"))
    })?;

    let mut memos = server.agent_runtime_write();
    memos.handoff_memos.push(memo);

    if memos.handoff_memos.len() > HANDOFF_MEMORY_LIMIT {
        let drain_count = memos.handoff_memos.len() - HANDOFF_MEMORY_LIMIT;
        memos.handoff_memos.drain(..drain_count);
    }

    serde_json::to_string(&json!({
        "status": "memo_left",
        "memo_id": memo_id,
        "from_agent": from_agent,
        "memo": memo_json,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_handoff_check(
    server: &MemoryServer,
    params: HandoffCheckParams,
) -> Result<String, String> {
    let agent_id = params.agent_id.as_deref();
    let entries = server.with_global_store_read(pending_handoff_entries)?;
    let matching_entries: Vec<MemoryEntry> = entries
        .into_iter()
        .filter(|entry| memo_matches_agent(&memo_from_entry(entry), agent_id))
        .collect();
    let matching: Vec<HandoffMemo> = matching_entries.iter().map(memo_from_entry).collect();

    let result = serde_json::to_string(&json!({
        "pending_memos": matching.len(),
        "memos": matching,
    }))
    .map_err(|e| format!("serialize: {e}"))?;

    if params.acknowledge {
        for entry in matching_entries {
            server.with_global_store(|store| upsert_acknowledged_entry(store, entry, agent_id))?;
        }

        let mut rt = server.agent_runtime_write();
        for memo in rt.handoff_memos.iter_mut() {
            if memo_matches_agent(memo, agent_id) {
                memo.acknowledged = true;
            }
        }
    }

    Ok(result)
}
