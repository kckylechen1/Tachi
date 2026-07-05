//! Pure dispatch boundary shared by the memory server adapter.
//!
//! This crate intentionally contains policy and command construction only.
//! Runtime orchestration, MCP config generation, run artifacts, and database
//! writes stay in `memory-server`.

mod launcher;
mod registry;

pub use launcher::{
    build_claude_launch, build_codex_launch, build_custom_launch, build_grok_launch,
    build_kimi_launch, is_trusted_dispatch_command, resolve_permission_profile, tail_chars,
    DispatchLaunchParams, LaunchCommand, PermissionProfile,
};
pub use registry::{
    dispatch_agent_help_list, fallback_chain, mcp_inject_supported, normalize_dispatch_agent_name,
    resolve_dispatch_agent, select_agent_for_intent, select_agent_for_task, DispatchAgentDef,
    DispatchMcpSupport, DISPATCH_AGENTS,
};
