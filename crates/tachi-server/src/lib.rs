// lib.rs - Memory MCP Server runtime
//
// Rust MCP server using rmcp SDK to expose memcore functionality.
// Stateless design: each tool opens its own DB connection per-request.

//! # Build profiles: `full` (default) vs `portable` (Refs #924 / #770 #790 #798)
//!
//! `tachi-server` ships two Cargo profiles (see `[features]` in `Cargo.toml`):
//!
//! * **`full`** (default) — today's product runtime, unchanged. Memory kernel
//!   plus every operator/product surface. Every existing build resolves this
//!   profile, so behavior is byte-identical for full-profile users.
//!
//! * **`portable`** — the stripped HyperMem-cutover profile for the downstream
//!   trading runtime (Hyperion/Quant). It exists so the Hyperion-HyperTachi
//!   kernel fork can retire to a thin adapter over an upstream binary instead
//!   of patching operator code out.
//!
//! ## `portable` — what is IN
//!
//! * Memory facades: `save` / `search` / `get` / `briefing` / `checkpoint`.
//! * Serving: stdio MCP + HTTP, daemon mode with per-scope pid/lock isolation.
//! * `status` / health readiness.
//! * The #791 `DecayPolicy` / `hybrid_score_with_policy` scorer hook (in
//!   `memcore`), so A-share scoring re-lands downstream as policy config, not
//!   fork patches.
//! * Opens DBs written by full Tachi (admin tables present but unused — this is
//!   guaranteed by `portable-kernel`'s `default-features = false` memcore).
//!
//! ## `portable` — what is OUT (operator/product surfaces)
//!
//! * Vault secrets, hub / skill catalog, foundry job queue, dispatch /
//!   ship / merge, `tachi_gh`, PR lifecycle. The crates backing these
//!   (`tachi-hub`, `tachi-foundry`, `tachi-dispatch`, `tachi-merge-ops`, and
//!   the operator CLIs) are `optional` deps enabled only by `full`, so a
//!   `portable` build never compiles them into the trading runtime.
//!
//! ## Status (skeleton — #924)
//!
//! The Cargo dependency partition is landed. The source-level module/router
//! gates that make `cargo check --no-default-features --features portable`
//! actually COMPILE are the staged follow-up (the operator surfaces are
//! interleaved through `MemoryServer`'s constructor, the `Commands` CLI enum,
//! the ~17 tool routers, and `status_ops`; see the issue's staged plan). Until
//! those gates land, `full` is the only compiling profile.

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
mod bootstrap;
mod build_broker;
pub mod build_info;
mod builtins;
mod capability_ops;
mod claims_ops;
mod cli_client;
mod complete_ops;
mod component_governance_ops;
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
mod exec_env_ops;
/// #894 S2e — the parent-owned `detect-and-reject` postflight gate (ordered
/// enforcement points 4 and 5). Public because it is the mechanism the
/// effective-authority compiler (a separate slice) composes: it takes a lease
/// workspace, a write contract and a liveness probe, and returns a verdict, a
/// receipt, and — on any prohibited delta — a loud failure with NO patch.
/// It proves "no change was accepted"; it is never read-only enforcement.
///
/// **NOT WIRED YET — see #894 S2 wiring slice.** This declaration is the module's
/// only reference in the whole crate: no dispatch path calls it, so no dispatch
/// is gated by it today.
pub mod exec_env_postflight;
mod exec_env_reaper;
mod facade_memory_ops;
mod facade_save_ops;
mod facade_search_ops;
mod feedback_rule_ops;
mod foundry_runtime_ops;
mod foundry_scheduler;
mod gh_ops;
mod gh_safe_merge;
mod handoff_ops;
mod host_profile;
mod hub_ops;
mod kanban;
pub mod lesson_forge_ops;
mod manifest;
mod mcp_connection;
mod mcp_pool;
mod mcp_proxy;
mod memory_ops;
mod memory_search_ops;
mod network_safety;
mod notes_ops;
mod orchestrator_ops;
mod path_utils;
mod peer_ops;
mod pipeline_ops;
mod precedent_ops;
mod project_db_ops;
mod prompts;
mod provenance;
mod provider_config;
mod refinery_ops;
mod repair;
mod research_ops;
mod sandbox_ops;
mod server_handler;
mod server_instructions;
mod server_methods;
mod session_identity;
mod shared_defs;
mod shell_ops;
mod signature_evidence;
mod skill_chain_ops;
mod skill_policy;
mod status_ops;
mod sticky_ops;
mod task_lifecycle;
#[cfg(test)]
mod test_support;
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
pub(crate) use server_state::{CachedVaultKey, DbScope, MemoryServer, VaultState};

// Enrichment batcher methods are in enrichment.rs

// ─── Tool Parameter Types ───────────────────────────────────────────────────────
//
// Note: dead_code warnings are expected here because the #[tool] macro
// generates code that uses these types through macro expansion.

// Parameter and tool schema definitions moved to `tool_params.rs`.

// MCP pool proxy methods are in mcp_pool.rs

// ─── Runtime Entrypoint ──────────────────────────────────────────────────────────

pub fn run_cli() {
    // Install the rustls `ring` crypto provider before any HTTPS client is
    // built. reqwest uses rustls-no-provider, so this is required for TLS.
    ensure_tls_provider();

    tachi_bootstrap::run_cli_with(bootstrap::run, |error| {
        error
            .downcast_ref::<repair::RepairExit>()
            .map(|exit| exit.code())
    });
}

/// Install the rustls `ring` crypto provider as the process default.
/// Thin wrapper over `tachi_llm::install_tls_provider`; re-exposed so that
/// internal modules building reqwest clients (mcp_connection, gh_ops,
/// wiki_ops) share one idempotent install site.
pub fn ensure_tls_provider() {
    tachi_llm::install_tls_provider();
}

#[cfg(test)]
mod tests;
