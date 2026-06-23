use crate::tool_params::SearchMemoryParams;
use crate::MemoryServer;
use memory_core::MemoryStore;

fn search_store(
    store: &mut MemoryStore,
    params: &SearchMemoryParams,
    record_access: bool,
) -> Result<Vec<memory_core::SearchResult>, String> {
    let mut opts = params.to_search_options(store.vec_available);
    opts.record_access = record_access;
    store
        .search(&params.query, Some(opts))
        .map_err(|e| e.to_string())
}

pub(super) fn with_named_project_search(
    server: &MemoryServer,
    project_name: &str,
    params: &SearchMemoryParams,
    record_access: bool,
    context: impl Into<String>,
) -> Result<Vec<memory_core::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(store, params, record_access).map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_named_project_store_read(project_name, action)
    }
}

pub(super) fn with_project_search(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    record_access: bool,
    context: impl Into<String>,
) -> Result<Vec<memory_core::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(store, params, record_access).map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        server.with_project_store(action)
    } else {
        server.with_project_store_read(action)
    }
}

pub(super) fn with_global_search(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    record_access: bool,
    context: impl Into<String>,
) -> Result<Vec<memory_core::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(store, params, record_access).map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        server.with_global_store(action)
    } else {
        server.with_global_store_read(action)
    }
}
