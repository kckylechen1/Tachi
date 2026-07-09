use crate::tool_params::SearchMemoryParams;
use crate::MemoryServer;
use memcore::{MemoryStore, RecallConfig};
use std::path::Path;

fn search_store(
    store: &mut MemoryStore,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
) -> Result<Vec<memcore::SearchResult>, String> {
    let mut opts =
        params.to_search_options_with_recall_config(store.vec_available, recall_config.cloned());
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
    recall_config: Option<&RecallConfig>,
    context: impl Into<String>,
) -> Result<Vec<memcore::SearchResult>, String> {
    let context = context.into();
    let effective_record_access =
        record_access && named_project_is_bound_project(server, project_name);
    let action = |store: &mut MemoryStore| {
        search_store(store, params, effective_record_access, recall_config)
            .map_err(|e| format!("{context}: {e}"))
    };
    if effective_record_access {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_named_project_store_read(project_name, action)
    }
}

fn named_project_is_bound_project(server: &MemoryServer, project_name: &str) -> bool {
    let Some(bound_project_db) = server.project_db_path_buf() else {
        return false;
    };
    let Ok(named_project_db) = MemoryServer::resolve_named_project_db_path(project_name) else {
        return false;
    };

    same_file(&bound_project_db, &named_project_db)
}

fn same_file(left: &Path, right: &Path) -> bool {
    let Ok(left) = std::fs::canonicalize(left) else {
        return false;
    };
    let Ok(right) = std::fs::canonicalize(right) else {
        return false;
    };
    left == right
}

pub(super) fn with_project_search(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
    context: impl Into<String>,
) -> Result<Vec<memcore::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(store, params, record_access, recall_config)
            .map_err(|e| format!("{context}: {e}"))
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
    recall_config: Option<&RecallConfig>,
    context: impl Into<String>,
) -> Result<Vec<memcore::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(store, params, record_access, recall_config)
            .map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        server.with_global_store(action)
    } else {
        server.with_global_store_read(action)
    }
}
