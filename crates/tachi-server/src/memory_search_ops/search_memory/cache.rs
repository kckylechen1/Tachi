use crate::tool_params::SearchMemoryParams;
use crate::utils::{parse_env_bool, stable_hash};

pub(super) fn recall_cache_recall_opted_in(path_prefix: Option<&str>) -> bool {
    memcore::path_prefix_opts_into_recall_cache(path_prefix)
}

/// Master gate for the recall-cache read short-circuit + write-through.
/// Off by default; enabled per-deployment via `~/.tachi/config.env`.
///
/// `pub(crate)` (not `pub(super)`): tachi#1435 slice 3 / #2059's save-side
/// cache invalidation lives in the sibling `save_memory` module and must gate
/// on the exact same flag — a save-time invalidation that ran unconditionally
/// (or independently re-read the env var) would drift from this read/write
/// gate by construction. Re-exported at `memory_search_ops` via
/// `search_memory::recall_cache_read_enabled`.
pub(crate) fn recall_cache_read_enabled() -> bool {
    parse_env_bool("TACHI_ENABLE_RECALL_CACHE").unwrap_or(false)
}

/// Freshness window for a cached entry, in seconds. A short default bounds how
/// long a just-added memory can stay hidden behind a stale entry; write-through
/// keeps actually-run queries fresh.
pub(super) fn recall_cache_ttl_secs() -> i64 {
    std::env::var("TACHI_RECALL_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(900)
}

fn normalize_cache_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Build the opaque recall-cache key from every `SearchMemoryParams` field that
/// changes which rows are returned. Keep this in sync with the read-side
/// filters below — a result-affecting field missing here would let one query
/// serve another's cached rows. `enable_rerank` is deliberately excluded so the
/// background rerank job can upgrade the same entry in place; rerank intent is
/// reconciled against the stored `reranked` flag at read time.
pub(super) fn recall_cache_key(
    params: &SearchMemoryParams,
    top_k: usize,
    project_only: bool,
) -> String {
    let seed = format!(
        "rcv1|{q}|{proj}|{prefix}|{domain}|{top_k}|{po}|{tr}|{ar}|{meta}|{role}",
        q = normalize_cache_query(&params.query),
        proj = params.project.as_deref().unwrap_or(""),
        prefix = params.path_prefix.as_deref().unwrap_or(""),
        domain = params.domain.as_deref().unwrap_or(""),
        top_k = top_k,
        po = project_only as u8,
        tr = params.include_training as u8,
        ar = params.include_archived as u8,
        meta = params.include_metadata as u8,
        role = params.agent_role.as_deref().unwrap_or(""),
    );
    format!("rc:{}", stable_hash(&seed))
}
