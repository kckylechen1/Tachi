use memcore::MemoryEntry;
use serde_json::json;

use crate::server_state::{DbScope, MemoryServer};
use crate::shared_defs::slim_entry;
use crate::tool_params::SyncMemoriesParams;

pub(crate) async fn handle_sync_memories(
    server: &MemoryServer,
    params: SyncMemoriesParams,
) -> Result<String, String> {
    let path_prefix = params.path_prefix.as_deref().unwrap_or("/");
    let limit = params.limit.min(500);

    let mut all_entries: Vec<(MemoryEntry, DbScope)> = Vec::new();

    // #925 follow-up: recency-first so sync surfaces newest changes under limit
    // (path-ordered list_by_path can drop recent rows before client-side sort).
    let global_entries = server.with_global_store(|store| {
        store
            .list_by_path_recent(path_prefix, limit, false)
            .map_err(|e| format!("Failed to list global memories: {}", e))
    })?;
    all_entries.extend(global_entries.into_iter().map(|e| (e, DbScope::Global)));

    if server.has_project_db() {
        let project_entries = server.with_project_store(|store| {
            store
                .list_by_path_recent(path_prefix, limit, false)
                .map_err(|e| format!("Failed to list project memories: {}", e))
        })?;
        all_entries.extend(project_entries.into_iter().map(|e| (e, DbScope::Project)));
    }

    all_entries.sort_by(|a, b| b.0.timestamp.cmp(&a.0.timestamp));
    all_entries.truncate(limit);

    if all_entries.is_empty() {
        return serde_json::to_string(&json!({
            "agent_id": params.agent_id,
            "new_count": 0,
            "changed_count": 0,
            "entries": [],
        }))
        .map_err(|e| format!("Failed to serialize: {e}"));
    }

    let memory_ids: Vec<String> = all_entries.iter().map(|(e, _)| e.id.clone()).collect();

    let known_revisions = server.with_global_store(|store| {
        store
            .get_agent_known_revisions(&params.agent_id, &memory_ids)
            .map_err(|e| format!("Failed to get known revisions: {}", e))
    })?;

    let known_revisions = if server.has_project_db() {
        let mut project_known = server.with_project_store(|store| {
            store
                .get_agent_known_revisions(&params.agent_id, &memory_ids)
                .map_err(|e| format!("Failed to get project known revisions: {}", e))
        })?;
        for (id, rev) in known_revisions {
            project_known.entry(id).or_insert(rev);
        }
        project_known
    } else {
        known_revisions
    };

    let mut diff_entries: Vec<serde_json::Value> = Vec::new();
    let mut sync_updates: Vec<(String, i64)> = Vec::new();
    let mut new_count = 0u64;
    let mut changed_count = 0u64;

    for (entry, db_scope) in &all_entries {
        let current_rev = entry.revision;
        match known_revisions.get(&entry.id) {
            None => {
                let mut obj = match slim_entry(entry, *db_scope) {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                obj.insert("diff_type".into(), json!("new"));
                diff_entries.push(serde_json::Value::Object(obj));
                sync_updates.push((entry.id.clone(), current_rev));
                new_count += 1;
            }
            Some(&known_rev) if current_rev > known_rev => {
                let mut obj = match slim_entry(entry, *db_scope) {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                obj.insert("diff_type".into(), json!("changed"));
                obj.insert("prev_revision".into(), json!(known_rev));
                diff_entries.push(serde_json::Value::Object(obj));
                sync_updates.push((entry.id.clone(), current_rev));
                changed_count += 1;
            }
            _ => {
                sync_updates.push((entry.id.clone(), current_rev));
            }
        }
    }

    if !sync_updates.is_empty() {
        server
            .with_global_store(|store| {
                store
                    .update_agent_known_state(&params.agent_id, &sync_updates)
                    .map_err(|e| format!("Failed to update agent state: {}", e))
            })
            .map_err(|e| {
                format!(
                    "sync_memories failed to persist agent state for '{}': {}",
                    params.agent_id, e
                )
            })?;
    }

    serde_json::to_string(&json!({
        "agent_id": params.agent_id,
        "new_count": new_count,
        "changed_count": changed_count,
        "entries": diff_entries,
    }))
    .map_err(|e| format!("Failed to serialize: {e}"))
}
