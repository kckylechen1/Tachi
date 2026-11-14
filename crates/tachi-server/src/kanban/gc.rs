use super::*;

/// Reap unpinned, unreferenced terminal Kanban cards after their terminal
/// retention interval. Selection and deletion share one write transaction.
///
/// #1413 concern 1 — cache invalidation is intentionally NOT done here: this
/// function runs INSIDE a caller's `with_global_store` / `with_project_store`
/// closure and only borrows a `&mut MemoryStore` (no `&MemoryServer`). Calling
/// the shared recall-cache invalidator from here would re-enter
/// `with_global_store` (recursing on the non-reentrant `global_rw_gate` when
/// the caller holds the global store, or nesting the global gate inside the
/// project gate otherwise). The post-commit cache bust is the operator GC
/// caller's job, issued AFTER this returns and the store lock is released.
pub(crate) fn gc_expired_kanban_cards(
    store: &mut MemoryStore,
    max_age_days: u64,
) -> Result<usize, String> {
    let as_of = chrono::Utc::now().to_rfc3339();

    store
        .gc_expired_kanban_cards(&as_of, max_age_days)
        .map_err(|error| format!("safe kanban GC failed: {error}"))
}
