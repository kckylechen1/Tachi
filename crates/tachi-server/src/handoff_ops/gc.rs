use memcore::MemoryStore;

use super::HANDOFF_PATH;

pub(crate) fn gc_expired_handoff_memories(
    store: &mut MemoryStore,
    max_age_days: u64,
) -> Result<usize, String> {
    let cutoff = chrono::Utc::now()
        - chrono::Duration::days(std::cmp::min(max_age_days, i64::MAX as u64) as i64);

    let candidates = store
        .list_memories_by_category_and_path_prefix("handoff", &format!("{HANDOFF_PATH}%"))
        .map_err(|e| format!("query expired handoff memories failed: {e}"))?;

    let mut ids_to_delete = Vec::new();
    for row in candidates {
        let id = row.id;
        let timestamp = row.timestamp;
        let archived = row.archived;
        let metadata: serde_json::Value = serde_json::from_str(&row.metadata)
            .map_err(|e| format!("parse handoff metadata for '{id}' failed: {e}"))?;

        let status = metadata
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        if status != "acknowledged" && status != "promoted" && status != "superseded" && !archived {
            continue;
        }

        let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp)
            .map_err(|e| format!("parse handoff timestamp for '{id}' failed: {e}"))?
            .with_timezone(&chrono::Utc);
        if timestamp < cutoff {
            ids_to_delete.push(id);
        }
    }

    let mut deleted = 0usize;
    for id in ids_to_delete {
        if store
            .delete(&id)
            .map_err(|e| format!("delete expired handoff '{id}' failed: {e}"))?
        {
            deleted += 1;
        }
    }

    Ok(deleted)
}
