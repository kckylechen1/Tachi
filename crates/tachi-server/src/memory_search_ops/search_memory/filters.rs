use crate::memory_search_ops::routing_config::RoutingConfig;
use crate::memory_search_ops::search_helpers::infer_search_project;
use crate::tool_params::{FindSimilarMemoryParams, SearchMemoryParams};

fn is_training_path(path: &str) -> bool {
    path == "/sft" || path.starts_with("/sft/")
}

fn is_eval_path(path: &str) -> bool {
    path == "/eval" || path.starts_with("/eval/")
}

pub(super) fn is_eval_entry(entry: &memcore::MemoryEntry) -> bool {
    memcore::is_eval_entry(entry) || is_eval_path(&entry.path)
}

pub(super) fn training_recall_opted_in(params: &SearchMemoryParams) -> bool {
    params.include_training
        || params
            .path_prefix
            .as_deref()
            .is_some_and(|prefix| is_training_path(prefix.trim_end_matches('/')))
}

pub(super) fn find_similar_training_opted_in(params: &FindSimilarMemoryParams) -> bool {
    params.include_training
        || params
            .path_prefix
            .as_deref()
            .is_some_and(|prefix| is_training_path(prefix.trim_end_matches('/')))
}

pub(super) fn eval_recall_opted_in(params: &SearchMemoryParams) -> bool {
    params
        .path_prefix
        .as_deref()
        .is_some_and(|prefix| is_eval_path(prefix.trim_end_matches('/')))
}

fn query_explicitly_requests_foreign_sigil_domain_with(
    query: &str,
    domain: Option<&str>,
    config: &RoutingConfig,
) -> bool {
    if domain.is_some() {
        return true;
    }
    let q = query.to_lowercase();
    // Whole-word ASCII terms (so common coding queries like "change"/"channel"
    // → "chan", "quantity" → "quant" don't trip the filter) plus CJK/numeric
    // substrings. Both term lists come from RoutingConfig, not hardcoded here.
    let matches_word = q
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| config.foreign_domain_word_terms.iter().any(|t| t == word));
    matches_word
        || config
            .foreign_domain_substring_terms
            .iter()
            .any(|term| q.contains(term))
}

fn is_foreign_sigil_memory_with(entry: &memcore::MemoryEntry, config: &RoutingConfig) -> bool {
    let domain = entry.domain.as_deref().unwrap_or("");
    let path = entry.path.to_ascii_lowercase();
    config
        .foreign_domains
        .iter()
        .any(|d| d.eq_ignore_ascii_case(domain))
        || config
            .foreign_path_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
}

pub(super) fn project_scope_allows_memory(
    project_name: &str,
    params: &SearchMemoryParams,
    entry: &memcore::MemoryEntry,
) -> bool {
    project_scope_allows_memory_with_config(project_name, params, entry, &RoutingConfig::get())
}

pub(super) fn project_scope_allows_memory_with_config(
    project_name: &str,
    params: &SearchMemoryParams,
    entry: &memcore::MemoryEntry,
    config: &RoutingConfig,
) -> bool {
    if !project_name.eq_ignore_ascii_case("sigil") {
        return true;
    }
    if query_explicitly_requests_foreign_sigil_domain_with(
        &params.query,
        params.domain.as_deref(),
        config,
    ) {
        return true;
    }
    !is_foreign_sigil_memory_with(entry, config)
}

pub(super) fn project_filter_name(
    home: &std::path::Path,
    params: &SearchMemoryParams,
    project_only: bool,
) -> Option<String> {
    params.project.clone().or_else(|| {
        if project_only {
            crate::memory_search_ops::search_helpers::resolve_workspace_named_project()
        } else {
            infer_search_project(home, &params.query, params.domain.as_deref())
        }
    })
}
