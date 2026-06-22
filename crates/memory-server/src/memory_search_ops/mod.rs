mod auto_link;
mod confidence_reinforce;
mod contradiction;
mod routing_config;
mod save_memory;
mod search_helpers;
mod search_memory;
mod text_scrub;

// Re-export all pub/pub(crate) items that external modules use.
pub(crate) use confidence_reinforce::apply_confidence_reinforcement_links;
pub(crate) use contradiction::apply_auto_contradiction_detection;
pub(crate) use save_memory::handle_remember;
pub(crate) use save_memory::handle_save_memory;
pub(crate) use search_helpers::named_project_db_exists;
pub(crate) use search_helpers::named_project_from_db_path;
pub(crate) use search_helpers::resolve_workspace_named_project;
pub(crate) use search_memory::handle_find_similar_memory;
pub(crate) use search_memory::handle_search_memory;
pub(crate) use search_memory::handle_search_memory_with_access;
pub(crate) use search_memory::search_memory_rows;
pub(crate) use search_memory::search_memory_rows_with_access;
pub(crate) use text_scrub::scrub_secrets;

// Shared imports that sub-modules pull in via `use super::*`.
use crate::shared_defs::{slim_entry, slim_l0_rule, slim_search_result};
use crate::tool_params::*;
use crate::utils::{is_active_global_rule, parse_env_bool, stable_hash};
use crate::DbScope;
use crate::MemoryServer;
use memory_core::{MemoryStore, SearchOptions};
use serde_json::json;
