use super::normalize::normalize_card_status;
use super::*;

pub(crate) fn gc_expired_kanban_cards(
    store: &mut MemoryStore,
    max_age_days: u64,
) -> Result<usize, String> {
    let cutoff = chrono::Utc::now()
        - chrono::Duration::days(std::cmp::min(max_age_days, i64::MAX as u64) as i64);
    let mut stmt = store
        .connection()
        .prepare(
            "SELECT id, path, category, timestamp, metadata
             FROM memories
             WHERE path LIKE ?1",
        )
        .map_err(|e| format!("prepare kanban GC query failed: {e}"))?;
    let rows = stmt
        .query_map((format!("{KANBAN_PATH_PREFIX}%"),), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| format!("query expired kanban cards failed: {e}"))?;

    let mut ids_to_delete = Vec::new();
    for row in rows {
        let (id, path, category, timestamp, metadata_json) =
            row.map_err(|e| format!("read expired kanban card candidate failed: {e}"))?;
        let metadata: serde_json::Value = serde_json::from_str(&metadata_json)
            .map_err(|e| format!("parse kanban card metadata for '{id}' failed: {e}"))?;

        // A card is reapable when EITHER:
        //   (a) it is a `category=kanban` card in a terminal status
        //       (resolved/expired), or
        //   (b) it is a dispatch ("board") card under /kanban/tasks/ that is
        //       stuck in a non-terminal a2a_state from a long-dead run.
        let reapable = if category == KANBAN_CATEGORY {
            matches!(
                metadata
                    .get("status")
                    .and_then(|value| value.as_str())
                    .and_then(normalize_card_status)
                    .as_deref(),
                Some("resolved") | Some("expired")
            )
        } else if path.starts_with(KANBAN_DISPATCH_PATH_PREFIX) {
            metadata
                .get("a2a_state")
                .and_then(|value| value.as_str())
                .map(|state| KANBAN_DISPATCH_NON_TERMINAL_STATES.contains(&state))
                .unwrap_or(false)
        } else {
            false
        };
        if !reapable {
            continue;
        }

        let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp)
            .map_err(|e| format!("parse kanban card timestamp for '{id}' failed: {e}"))?
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
            .map_err(|e| format!("delete expired kanban card '{id}' failed: {e}"))?
        {
            deleted += 1;
        }
    }

    Ok(deleted)
}
