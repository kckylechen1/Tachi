use crate::server_state::{HandoffMemo, MemoryServer};
use chrono::Utc;
use memory_core::{MemoryEntry, MemoryStore};
use serde_json::json;

use super::memo::memo_from_entry;
use super::{HANDOFF_DB_LIMIT, HANDOFF_PATH};

/// Pending cross-project handoff memos in global DB (session briefing feed).
pub(crate) fn list_pending_handoffs_for_briefing(
    server: &MemoryServer,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.max(1).min(12);
    let entries = server.with_global_store_read(pending_handoff_entries)?;
    let rows = entries
        .into_iter()
        .take(limit)
        .map(|entry| {
            let memo = memo_from_entry(&entry);
            serde_json::json!({
                "id": entry.id,
                "path": entry.path,
                "from_agent": memo.from_agent,
                "target_agent": memo.target_agent,
                "summary": memo.summary,
                "next_steps": memo.next_steps,
                "created_at": memo.created_at,
                "kind": "handoff",
            })
        })
        .collect();
    Ok(rows)
}

pub(super) fn pending_handoff_entries(store: &mut MemoryStore) -> Result<Vec<MemoryEntry>, String> {
    let mut entries = store
        .list_by_path(HANDOFF_PATH, HANDOFF_DB_LIMIT, false)
        .map_err(|e| format!("Failed to list handoff memories: {e}"))?;
    entries.retain(|entry| entry.category == "handoff" && !memo_from_entry(entry).acknowledged);
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(entries)
}

pub(super) fn upsert_acknowledged_entry(
    store: &mut MemoryStore,
    mut entry: MemoryEntry,
    agent_id: Option<&str>,
) -> Result<(), String> {
    let acknowledged_at = Utc::now().to_rfc3339();
    let mut memo = memo_from_entry(&entry);
    memo.acknowledged = true;

    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "handoff metadata must be an object".to_string())?;
    metadata.insert("handoff".into(), json!(memo));
    metadata.insert("status".into(), json!("acknowledged"));
    metadata.insert("acknowledged".into(), json!(true));
    metadata.insert("acknowledged_at".into(), json!(acknowledged_at));
    if let Some(agent_id) = agent_id.filter(|value| !value.trim().is_empty()) {
        metadata.insert("acknowledged_by".into(), json!(agent_id));
    }

    entry.vector = None;
    store
        .upsert(&entry)
        .map_err(|e| format!("Failed to acknowledge handoff memory: {e}"))
}

pub(super) fn supersede_pending_handoffs(
    store: &mut MemoryStore,
    memo: &HandoffMemo,
    entry: &MemoryEntry,
) -> Result<(), String> {
    let pending = pending_handoff_entries(store)?;
    for old_entry in pending {
        let old_memo = memo_from_entry(&old_entry);
        if old_memo.from_agent == memo.from_agent && old_memo.target_agent == memo.target_agent {
            let mut old_entry_mut = old_entry.clone();
            old_entry_mut.archived = true;
            old_entry_mut.vector = None;
            if let Some(obj) = old_entry_mut.metadata.as_object_mut() {
                obj.insert(
                    "status".to_string(),
                    serde_json::Value::String("superseded".to_string()),
                );
            }
            store.upsert(&old_entry_mut).map_err(|e| format!("{e}"))?;
            store
                .supersede_memory(&old_entry.id, &entry.id)
                .map_err(|e| format!("{e}"))?;
        }
    }
    Ok(())
}
