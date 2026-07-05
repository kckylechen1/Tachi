// lib.rs - Memory MCP Server runtime
//
// Rust MCP server using rmcp SDK to expose memory-core functionality.
// Stateless design: each tool opens its own DB connection per-request.

#![allow(
    clippy::cast_abs_to_unsigned,
    clippy::cloned_ref_to_slice_refs,
    clippy::cmp_owned,
    clippy::collapsible_if,
    clippy::collapsible_str_replace,
    clippy::derivable_impls,
    clippy::doc_overindented_list_items,
    clippy::enum_variant_names,
    clippy::field_reassign_with_default,
    clippy::if_same_then_else,
    clippy::io_other_error,
    clippy::let_and_return,
    clippy::manual_async_fn,
    clippy::manual_clamp,
    clippy::manual_pattern_char_comparison,
    clippy::manual_strip,
    clippy::needless_range_loop,
    clippy::needless_update,
    clippy::ptr_arg,
    clippy::redundant_closure,
    clippy::too_many_arguments,
    clippy::unnecessary_cast,
    clippy::unnecessary_sort_by,
    clippy::useless_conversion,
    clippy::useless_format
)]

mod agent_eval;
mod agent_markdown;
mod agent_profile_ops;
mod agent_registry;
mod arena_ops;
mod bootstrap;
mod builtins;
mod capability_ops;
mod capture_gate;
mod cli_client;
mod complete_ops;
mod continuity_ops;
mod continuity_projector;
mod copilot_ops;
mod credential_profile;
mod daemon_lock;
mod daily_pipeline;
mod dispatch_ops;
mod dispatch_profile;
mod dlq_ops;
pub(crate) mod docs_ops;
mod doctor;
mod doctor_ops;
mod domain_adapter_ops;
mod enrichment;
mod event_ops;
mod facade_memory_ops;
mod facade_save_ops;
mod facade_search_ops;
mod feedback_rule_ops;
mod foundry_ops;
mod foundry_runtime_ops;
mod foundry_scheduler;
mod gh_ops;
mod gh_safe_merge;
mod graph_state_ops;
mod handoff_ops;
mod hub_cli;
mod hub_helpers;
mod hub_ops;
mod kanban;
mod manifest;
mod manifest_audit;
mod mcp_connection;
mod mcp_pool;
mod mcp_proxy;
mod memory_ops;
mod memory_search_ops;
mod network_safety;
mod notes_ops;
mod orchestrator_ops;
mod pack_ops;
mod path_utils;
mod pipeline_ops;
mod profiles;
mod project_db_ops;
mod prompt_envelope;
mod prompts;
mod provenance;
mod provider_config;
mod repair;
mod rescue;
mod sandbox_ops;
mod server_handler;
mod server_instructions;
mod server_methods;
mod shared_defs;
mod shell_ops;
mod skill_chain_ops;
mod skill_policy;
mod status_ops;
mod task_lifecycle;
mod tool_params;
mod tools;
mod utils;
mod vault_crypto;
mod vault_ops;
mod vector_backfill;
mod vector_sweep;
mod verify_ops;
mod web_search_ops;
mod wiki_ops;
mod workflow_closure;

use crate::tool_params::*;

pub(crate) mod server_state;
pub(crate) use server_state::{AgentProfile, CachedVaultKey, DbScope, MemoryServer, VaultState};

// Enrichment batcher methods are in enrichment.rs

// ─── Tool Parameter Types ───────────────────────────────────────────────────────
//
// Note: dead_code warnings are expected here because the #[tool] macro
// generates code that uses these types through macro expansion.

// Parameter and tool schema definitions moved to `tool_params.rs`.

// MCP pool proxy methods are in mcp_pool.rs

// ─── Runtime Entrypoint ──────────────────────────────────────────────────────────

pub fn run_cli() {
    tachi_bootstrap::run_cli_with(bootstrap::run, |error| {
        error
            .downcast_ref::<repair::RepairExit>()
            .map(|exit| exit.code())
    });
}

#[cfg(test)]
mod tests;
