use crate::tool_params::SearchMemoryParams;
use crate::MemoryServer;
use memcore::{MemoryStore, RecallConfig};
use memory_server_runtime::ReadPoolCheckoutReceipt;
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
    let results = store
        .search(&params.query, Some(opts))
        .map_err(|e| e.to_string())?;
    // Strong access signal: results were recalled and access was recorded
    // ("recalled and used"). Opt-in personal /eval corpus capture lives here so
    // it cannot break the search path (log + skip on any capture error).
    if record_access {
        crate::memory_search_ops::eval_capture::maybe_capture_after_access(store, params, &results);
    }
    Ok(results)
}

/// #1125 recording twin of [`search_store`]: identical search + access-capture
/// behavior, but runs `MemoryStore::search_with_receipt` (the sampled path)
/// instead of the plain `search`, returning the per-phase `SearchPhaseReceipt`
/// alongside the results. The receipt's `pool_wait` arrives here as
/// `LayerAvailability::Unavailable` — `hybrid_search` takes a bare
/// `&Connection` and cannot see the pool checkout that happened ABOVE this
/// closure. The caller (`with_*_search_recording`) returns the pool checkout
/// receipt separately; the rows.rs call site assembles the two (injecting
/// `Measured` only where a checkout was actually measured). This function
/// performs NO `Instant::now` of its own beyond what the existing sampled
/// search already does — it rides the receipt API's own `sample` plumbing.
fn search_store_recording(
    store: &mut MemoryStore,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
) -> Result<(Vec<memcore::SearchResult>, memcore::SearchPhaseReceipt), String> {
    let mut opts =
        params.to_search_options_with_recall_config(store.vec_available, recall_config.cloned());
    opts.record_access = record_access;
    let (results, receipt) = store
        .search_with_receipt(&params.query, Some(opts))
        .map_err(|e| e.to_string())?;
    if record_access {
        crate::memory_search_ops::eval_capture::maybe_capture_after_access(store, params, &results);
    }
    Ok((results, receipt))
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

/// #1125 recording twin of [`with_named_project_search`]. Returns the search
/// results + phase receipt PLUS the pool checkout receipt, as a 3-tuple. The
/// pool receipt is `Some` on the attached/cached branch (which checks a slot
/// out of the project read pool) and `None` on the uncached branch (a fresh
/// read-only connection with no pool) — the rows call site maps `None` to
/// `Unavailable`, never to a fake zero. When `record_access` routes through
/// the WRITE store there is no read pool at all, so it returns `None` too.
pub(super) fn with_named_project_search_recording(
    server: &MemoryServer,
    project_name: &str,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
    context: impl Into<String>,
) -> Result<
    (
        Vec<memcore::SearchResult>,
        memcore::SearchPhaseReceipt,
        Option<ReadPoolCheckoutReceipt>,
    ),
    String,
> {
    let context = context.into();
    let effective_record_access =
        record_access && named_project_is_bound_project(server, project_name);
    let action = |store: &mut MemoryStore| {
        search_store_recording(store, params, effective_record_access, recall_config)
            .map_err(|e| format!("{context}: {e}"))
    };
    if effective_record_access {
        // Write store: no read pool → no checkout to measure.
        let (results, receipt) = server.with_named_project_store(project_name, action)?;
        Ok((results, receipt, None))
    } else {
        // `with_named_project_store_read` resolves the path then calls
        // `DbRuntime::with_path_store_read_with_label`. Reaching the
        // recording checkout without adding a MemoryServer method (which is
        // outside this leaf's allowlist) requires resolving the path the same
        // way and calling the DbRuntime recording twin directly via the
        // pub(crate) `db` field — same path resolution, same label, identical
        // semantics, only the receipt is observed.
        let db_path = MemoryServer::resolve_named_project_db_path(project_name)?;
        let ((results, receipt), pool) = server.db.with_path_store_read_with_label_recording(
            &db_path,
            &format!("named-project:{project_name}"),
            action,
        )?;
        Ok((results, receipt, pool))
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

/// #1125 recording twin of [`with_project_search`] — see
/// [`with_named_project_search_recording`]. The project read path always
/// checks a slot out of the project read pool, so the pool receipt is `Some`
/// whenever `record_access` is false; the write-store branch returns `None`
/// (no read pool).
pub(super) fn with_project_search_recording(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
    context: impl Into<String>,
) -> Result<
    (
        Vec<memcore::SearchResult>,
        memcore::SearchPhaseReceipt,
        Option<ReadPoolCheckoutReceipt>,
    ),
    String,
> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store_recording(store, params, record_access, recall_config)
            .map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        let (results, receipt) = server.with_project_store(action)?;
        Ok((results, receipt, None))
    } else {
        let ((results, receipt), pool) = server.db.with_project_store_read_recording(action)?;
        Ok((results, receipt, Some(pool)))
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

/// #1125 recording twin of [`with_global_search`] — see
/// [`with_named_project_search_recording`]. The global read path always
/// checks a slot out of the global read pool, so the pool receipt is `Some`
/// whenever `record_access` is false; the write-store branch returns `None`
/// (no read pool).
pub(super) fn with_global_search_recording(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
    context: impl Into<String>,
) -> Result<
    (
        Vec<memcore::SearchResult>,
        memcore::SearchPhaseReceipt,
        Option<ReadPoolCheckoutReceipt>,
    ),
    String,
> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store_recording(store, params, record_access, recall_config)
            .map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        let (results, receipt) = server.with_global_store(action)?;
        Ok((results, receipt, None))
    } else {
        let ((results, receipt), pool) = server.db.with_global_store_read_recording(action)?;
        Ok((results, receipt, Some(pool)))
    }
}
