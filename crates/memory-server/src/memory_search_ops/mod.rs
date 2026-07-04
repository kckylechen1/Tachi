mod auto_link;
mod confidence_reinforce;
mod contradiction;
mod rerank;
mod routing_config;
mod save_memory;
mod search_helpers;
mod search_memory;
mod text_scrub;

// Re-export all pub/pub(crate) items that external modules use.
pub(crate) use confidence_reinforce::apply_confidence_reinforcement_links;
pub(crate) use contradiction::apply_auto_contradiction_detection;
pub(crate) use rerank::{
    apply_search_rerank_policy, expand_search_params_for_rerank, SearchRerankPolicy,
};
pub(crate) use save_memory::handle_remember;
pub(crate) use save_memory::handle_save_memory;
pub(crate) use search_helpers::client_project_precedence;
pub(crate) use search_helpers::explicit_workspace_project;
pub(crate) use search_helpers::list_available_named_projects;
pub(crate) use search_helpers::named_project_db_exists;
pub(crate) use search_helpers::named_project_from_db_path;
pub(crate) use search_helpers::normalize_json_relevance;
pub(crate) use search_helpers::resolve_effective_named_project;
pub(crate) use search_helpers::resolve_workspace_named_project;
pub(crate) use search_memory::handle_find_similar_memory;
pub(crate) use search_memory::handle_search_memory;
pub(crate) use search_memory::handle_search_memory_with_access;
pub(crate) use search_memory::search_memory_rows;
pub(crate) use search_memory::search_memory_rows_with_access;
pub(crate) use search_memory::search_memory_rows_with_recall_config;
pub(crate) use text_scrub::scrub_secrets;
