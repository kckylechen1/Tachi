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
mod agent_registry;
mod arena_ops;
mod backend_tier;
mod bootstrap;
mod builtins;
mod capability_ops;
mod capture_gate;
mod claude_pool;
mod cli;
mod cli_client;
mod complete_ops;
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
mod enrichment;
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
mod llm;
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

use crate::builtins::seed_builtin_capabilities;
use crate::foundry_runtime_ops::FoundryWorkerStats;
use crate::profiles::ToolProfile;
use crate::shared_defs::DeadLetter;
use crate::tool_params::*;
#[cfg(test)]
use crate::utils::lock_or_recover;
use crate::vault_ops::load_unlocked_env_secrets_for_child_env;

#[cfg(test)]
use chrono::Utc;
use clap::Parser;
#[cfg(test)]
use memory_core::{HubCapability, MemoryEntry, MemoryStore};
#[cfg(test)]
use rmcp::handler::server::wrapper::Parameters;
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::time::{Duration, Instant};

use crate::cli::Cli;
use crate::mcp_pool::McpClientPool;

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
    let cli = Cli::parse();
    if let Err(e) = bootstrap::run(cli) {
        if let Some(exit) = e.downcast_ref::<repair::RepairExit>() {
            std::process::exit(exit.code());
        }
        eprintln!("Fatal: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
