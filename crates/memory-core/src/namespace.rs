//! Namespace classification helpers for memory rows.
//!
//! These helpers keep cache/wiki/task namespace rules in one place so search,
//! status, and repair surfaces do not each grow their own partial string list.

use crate::types::MemoryEntry;

pub const FOUNDRY_RECALL_CACHE_SOURCE: &str = "foundry_recall_rerank_cache";

fn metadata_bool(entry: &MemoryEntry, key: &str) -> bool {
    entry
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn metadata_str_eq(entry: &MemoryEntry, key: &str, expected: &str) -> bool {
    entry
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

pub fn path_in_namespace(path: &str, namespace: &str) -> bool {
    let namespace = namespace.trim_end_matches('/');
    path == namespace || path.starts_with(&format!("{namespace}/"))
}

pub fn path_contains_recall_cache(path: &str) -> bool {
    path == "/recall-cache"
        || path.ends_with("/recall-cache")
        || path.contains("/recall-cache/")
        || path.contains("foundry_recall_rerank_cache")
}

pub fn path_prefix_opts_into_recall_cache(path_prefix: Option<&str>) -> bool {
    path_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .is_some_and(path_contains_recall_cache)
}

pub fn is_recall_cache_entry(entry: &MemoryEntry) -> bool {
    entry
        .source
        .eq_ignore_ascii_case(FOUNDRY_RECALL_CACHE_SOURCE)
        || entry
            .topic
            .eq_ignore_ascii_case(FOUNDRY_RECALL_CACHE_SOURCE)
        || entry.topic.eq_ignore_ascii_case("recall_rerank_cache")
        || entry.id.starts_with("foundry:recall-cache:")
        || path_contains_recall_cache(&entry.path)
        || metadata_bool(entry, "recall_rerank_cache")
        || metadata_str_eq(entry, "cache_key", FOUNDRY_RECALL_CACHE_SOURCE)
}

pub fn is_wiki_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/wiki")
        || entry.source.eq_ignore_ascii_case("wiki")
        || entry.category.eq_ignore_ascii_case("wiki")
        || entry
            .domain
            .as_deref()
            .is_some_and(|domain| domain.eq_ignore_ascii_case("wiki"))
        || metadata_bool(entry, "wiki")
}

pub fn is_kanban_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/kanban")
        || entry.source.eq_ignore_ascii_case("kanban")
        || entry.category.eq_ignore_ascii_case("kanban")
}

pub fn is_handoff_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/handoff")
        || entry.source.eq_ignore_ascii_case("handoff")
        || entry.category.eq_ignore_ascii_case("handoff")
}

pub fn is_eval_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/eval") || entry.category.eq_ignore_ascii_case("eval")
}

pub fn is_namespace_search_noise(entry: &MemoryEntry, path_prefix: Option<&str>) -> bool {
    let kanban_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/kanban"));
    let handoff_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/handoff"));
    let wiki_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/wiki"));
    (!wiki_scoped
        && (entry.path == "/wiki/_log"
            || metadata_bool(entry, "wiki_log")
            || entry.topic.eq_ignore_ascii_case("wiki_log")))
        || (!path_prefix_opts_into_recall_cache(path_prefix) && is_recall_cache_entry(entry))
        || (!kanban_scoped && is_kanban_entry(entry))
        || (!handoff_scoped && is_handoff_entry(entry))
}
