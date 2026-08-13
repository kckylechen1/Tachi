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
    let as_of = chrono::Utc::now().to_rfc3339();

    let candidates = store
        .list_memories_by_path_prefix(&format!("{KANBAN_PATH_PREFIX}%"))
        .map_err(|e| format!("query expired kanban cards failed: {e}"))?;

    let mut ids_to_delete = Vec::new();
    for row in candidates {
        let id = row.id;
        let path = row.path;
        let category = row.category;
        let timestamp = row.timestamp;
        if memcore::db::is_kanban_gc_candidate(
            &category,
            &path,
            &row.metadata,
            &timestamp,
            &as_of,
            max_age_days,
        )
        .map_err(|error| format!("evaluate kanban GC candidate '{id}' failed: {error}"))?
        {
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
