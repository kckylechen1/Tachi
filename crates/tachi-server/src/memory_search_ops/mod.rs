pub(crate) mod auto_link;
mod confidence_reinforce;
mod contradiction;
mod eval_capture;
mod library_binding;
mod recall_degradation;
mod rerank;
pub(crate) mod routing_config;
pub(crate) mod save_memory;
mod search_helpers;
mod search_memory;
mod text_scrub;

// Re-export all pub/pub(crate) items that external modules use.
pub(crate) use confidence_reinforce::apply_confidence_reinforcement_links;
pub(crate) use contradiction::apply_auto_contradiction_detection;
pub(crate) use library_binding::{
    format_binding_markdown, library_binding_receipt, scope_downgrade_warning,
};
pub(crate) use recall_degradation::{
    attach_lexical_only_marker, attach_rerank_fallback_degraded, merge_lexical_only_marker,
    short_reason as recall_short_reason,
};
pub(crate) use rerank::{
    apply_search_rerank_policy, expand_search_params_for_rerank, rerank_rows_with_outcome,
    RerankOutcome, SearchRerankPolicy,
};
pub(crate) use save_memory::handle_remember;
pub(crate) use save_memory::handle_save_memory;
pub(crate) use save_memory::handle_save_memory_from_caller;
pub(crate) use save_memory::handle_save_memory_with_references;
pub(crate) use save_memory::save_eval_memory;
#[allow(unused_imports)] // stable internal receipt siblings; producers may be feature-gated
pub(crate) use save_memory::{
    handle_save_memory_with_authorized_reference_mutations,
    handle_save_memory_with_authorized_reference_mutations_and_invocation,
    handle_save_memory_with_wiki_projection, save_eval_memory_with_authorized_reference_mutations,
};
pub(crate) use search_helpers::client_project_precedence;
pub(crate) use search_helpers::explicit_workspace_project;
pub(crate) use search_helpers::list_available_named_projects;
pub(crate) use search_helpers::named_project_db_exists;
pub(crate) use search_helpers::named_project_from_db_path_in_home;
pub(crate) use search_helpers::normalize_json_relevance;
pub(crate) use search_helpers::resolve_effective_named_project;
pub(crate) use search_helpers::resolve_workspace_named_project;

/// Apply the same think-tag and secret scrub used by the ordinary save path to
/// a server-generated durable artifact before its first write.
pub(crate) fn scrub_generated_memory_text(text: &str) -> String {
    let text = text_scrub::scrub_think_tags(text);
    text_scrub::scrub_secrets(&text).0
}
pub(crate) use search_memory::handle_find_similar_memory;
pub(crate) use search_memory::handle_search_memory;
pub(crate) use search_memory::handle_search_memory_with_access;
pub(crate) use search_memory::invalidate_recall_cache_after_write;
pub(crate) use search_memory::search_memory_rows;
pub(crate) use search_memory::search_memory_rows_with_access;
pub(crate) use search_memory::search_memory_rows_with_recall_config;
pub(crate) use search_memory::{
    annotate_wiki_exact_token_matches, search_wiki_store_candidates, WikiStoreSearchCandidate,
};
#[cfg(test)]
pub(crate) use search_memory::{
    RecallCacheRaceHook, RecallCacheRacePoint, RecallCacheTestOverride,
};
pub(crate) use text_scrub::{contains_secret_like, scrub_secrets, scrub_think_tags};
