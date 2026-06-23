use memory_core::MemoryStore;

use super::HANDOFF_PATH;

pub(crate) fn gc_expired_handoff_memories(
    store: &mut MemoryStore,
    max_age_days: u64,
) -> Result<usize, String> {
    let cutoff = chrono::Utc::now()
        - chrono::Duration::days(std::cmp::min(max_age_days, i64::MAX as u64) as i64);

    let mut stmt = store
        .connection()
        .prepare(
            "SELECT id, timestamp, metadata, archived
             FROM memories
             WHERE category = ?1 AND path LIKE ?2",
        )
        .map_err(|e| format!("prepare handoff GC query failed: {e}"))?;
    let rows = stmt
        .query_map(("handoff", format!("{HANDOFF_PATH}%")), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .map_err(|e| format!("query expired handoff memories failed: {e}"))?;

    let mut ids_to_delete = Vec::new();
    for row in rows {
        let (id, timestamp, metadata_json, archived) =
            row.map_err(|e| format!("read expired handoff candidate failed: {e}"))?;
        let metadata: serde_json::Value = serde_json::from_str(&metadata_json)
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
    drop(stmt);

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
