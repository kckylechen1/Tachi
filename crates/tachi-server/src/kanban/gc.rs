use super::normalize::normalize_card_status;
use super::*;

/// Reap terminal/expired kanban cards and stale dispatch ("board") cards
/// older than `max_age_days`.
///
/// #1413 concern 1 — cache invalidation is intentionally NOT done here: this
/// function runs INSIDE a caller's `with_global_store` / `with_project_store`
/// closure and only borrows a `&mut MemoryStore` (no `&MemoryServer`). Calling
/// the shared recall-cache invalidator from here would re-enter
/// `with_global_store` (recursing on the non-reentrant `global_rw_gate` when
/// the caller holds the global store, or nesting the global gate inside the
/// project gate otherwise). The post-commit cache bust is the caller's job,
/// issued AFTER this returns and the store lock is released — see
/// `handle_memory_gc` in `memory_ops.rs`.
pub(crate) fn gc_expired_kanban_cards(
    store: &mut MemoryStore,
    max_age_days: u64,
) -> Result<usize, String> {
    let cutoff = chrono::Utc::now()
        - chrono::Duration::days(std::cmp::min(max_age_days, i64::MAX as u64) as i64);

    let candidates = store
        .list_memories_by_path_prefix(&format!("{KANBAN_PATH_PREFIX}%"))
        .map_err(|e| format!("query expired kanban cards failed: {e}"))?;

    let mut ids_to_delete = Vec::new();
    for row in candidates {
        let id = row.id;
        let path = row.path;
        let category = row.category;
        let timestamp = row.timestamp;
        let metadata: serde_json::Value = serde_json::from_str(&row.metadata)
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
                .map(|state| {
                    KANBAN_DISPATCH_NON_TERMINAL_STATES.contains(&state)
                        && !(state == "TASK_STATE_INPUT_REQUIRED"
                            && metadata
                                .get("closure_kind")
                                .and_then(|value| value.as_str())
                                == Some("partial"))
                })
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
