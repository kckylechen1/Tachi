use crate::tool_params::SearchMemoryParams;
use crate::MemoryServer;
use memcore::{MemoryStore, RecallConfig};
#[cfg(test)]
use memory_server_runtime::ReadPoolCheckoutReceipt;
use memory_server_runtime::RequestScopedReadStore;
use std::collections::HashMap;
use std::path::Path;

/// Physical stores read by pipeline row augmentation. Both the row producer
/// and recall-cache generation planner consume this list; adding a source
/// therefore requires an exhaustive match in both places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PipelineRuleReadSource {
    BoundProject,
    Global,
}

pub(super) fn pipeline_rule_read_sources(server: &MemoryServer) -> Vec<PipelineRuleReadSource> {
    if !server.pipeline_enabled {
        return Vec::new();
    }

    let mut sources = Vec::with_capacity(2);
    if server.has_project_db() {
        sources.push(PipelineRuleReadSource::BoundProject);
    }
    sources.push(PipelineRuleReadSource::Global);
    sources
}

/// Direct read-only stores retained for one cache-capable request. Entries are
/// installed only for currently unattached named paths; attached paths retain
/// their established pool routing instead.
pub(super) struct RequestScopedNamedProjectReads {
    stores: HashMap<String, RequestScopedReadStore>,
}

impl RequestScopedNamedProjectReads {
    pub(super) fn new() -> Self {
        Self {
            stores: HashMap::new(),
        }
    }

    pub(super) fn open_if_unattached(
        &mut self,
        server: &MemoryServer,
        project_name: &str,
        canonical_path: &Path,
    ) -> Result<(), String> {
        if self.stores.contains_key(project_name) {
            return Ok(());
        }
        // Bare project name: this label becomes the opened store's `db_label`
        // (tachi#1569), and it must equal the write-side label for the same
        // file or the store-keyed Wiki gate sees a different store here than
        // it does through the attached-path route.
        if let Some(store) = server
            .db
            .open_unattached_path_store_read_session_with_label(canonical_path, project_name)?
        {
            self.stores.insert(project_name.to_string(), store);
        }
        Ok(())
    }

    pub(super) fn with_store<T>(
        &mut self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Option<Result<T, String>> {
        self.stores
            .get_mut(project_name)
            .map(|store| store.with_store(f))
    }
}

fn search_store(
    store: &mut MemoryStore,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
    bypass_wiki_lifecycle_gate: bool,
) -> Result<Vec<memcore::SearchResult>, String> {
    let mut opts =
        params.to_search_options_with_recall_config(store.vec_available, recall_config.cloned());
    opts.record_access = record_access;
    opts.bypass_wiki_lifecycle_gate = bypass_wiki_lifecycle_gate;
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

/// Test-proven, production-dormant (#1125): the consumer that flips search
/// sampling on operationally does not exist yet — same dormancy as the whole
/// receipt API. Lift this gate in the leaf that adds that consumer.
#[cfg(test)]
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
    bypass_wiki_lifecycle_gate: bool,
) -> Result<(Vec<memcore::SearchResult>, memcore::SearchPhaseReceipt), String> {
    let mut opts =
        params.to_search_options_with_recall_config(store.vec_available, recall_config.cloned());
    opts.record_access = record_access;
    opts.bypass_wiki_lifecycle_gate = bypass_wiki_lifecycle_gate;
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
    named_project_reads: Option<&mut RequestScopedNamedProjectReads>,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
    bypass_wiki_lifecycle_gate: bool,
    context: impl Into<String>,
) -> Result<Vec<memcore::SearchResult>, String> {
    let context = context.into();
    let effective_record_access =
        record_access && named_project_is_bound_project(server, project_name);
    let action = |store: &mut MemoryStore| {
        search_store(
            store,
            params,
            effective_record_access,
            recall_config,
            bypass_wiki_lifecycle_gate,
        )
        .map_err(|e| format!("{context}: {e}"))
    };
    if effective_record_access {
        server.with_named_project_store(project_name, action)
    } else if let Some(result) =
        named_project_reads.and_then(|reads| reads.with_store(project_name, action))
    {
        result
    } else {
        server.with_named_project_store_read(project_name, action)
    }
}

fn named_project_is_bound_project(server: &MemoryServer, project_name: &str) -> bool {
    let Some(bound_project_db) = server.project_db_path_buf() else {
        return false;
    };
    let Ok(named_project_db) = server.resolve_server_named_project_db_path(project_name) else {
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
    bypass_wiki_lifecycle_gate: bool,
    context: impl Into<String>,
) -> Result<Vec<memcore::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(
            store,
            params,
            record_access,
            recall_config,
            bypass_wiki_lifecycle_gate,
        )
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
    bypass_wiki_lifecycle_gate: bool,
    context: impl Into<String>,
) -> Result<Vec<memcore::SearchResult>, String> {
    let context = context.into();
    let action = |store: &mut MemoryStore| {
        search_store(
            store,
            params,
            record_access,
            recall_config,
            bypass_wiki_lifecycle_gate,
        )
        .map_err(|e| format!("{context}: {e}"))
    };
    if record_access {
        server.with_global_store(action)
    } else {
        server.with_global_store_read(action)
    }
}

/// Test-proven, production-dormant (#1125): the consumer that flips search
/// sampling on operationally does not exist yet — same dormancy as the whole
/// receipt API. Lift this gate in the leaf that adds that consumer.
#[cfg(test)]
/// #1125 recording twin of [`with_global_search`]. The global read path
/// always checks a slot out of the global read pool, so the pool receipt is
/// `Some` whenever `record_access` is false; the write-store branch returns
/// `None` (no read pool). Project / named-project twins were deliberately NOT
/// kept: they had zero callers (test or production) — the leaf that needs
/// project-path sampling mints them WITH their discriminating tests.
pub(super) fn with_global_search_recording(
    server: &MemoryServer,
    params: &SearchMemoryParams,
    record_access: bool,
    recall_config: Option<&RecallConfig>,
    bypass_wiki_lifecycle_gate: bool,
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
        search_store_recording(
            store,
            params,
            record_access,
            recall_config,
            bypass_wiki_lifecycle_gate,
        )
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
