//! Caller identity resolution for stickies.
//!
//! Mirrors `handoff_ops::identity` (same idiom, kept as a separate small
//! module rather than a cross-`*_ops` dependency — each `*_ops` sibling is
//! self-contained per the existing facade-ops convention in this crate).

use crate::server_state::MemoryServer;

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_agent_id(server: &MemoryServer) -> Option<String> {
    let guard = server.agent_runtime_read();
    guard
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.trim().to_string())
        .filter(|agent_id| !agent_id.is_empty())
}

/// Round-3 fix (codex final review of #964/PR #1003, BUG CP2, sender half):
/// this used to fall back to `TACHI_PROFILE` — the configured *tool profile*
/// (`worker`/`delegate`/`standard`/`codex`), a capability-surface selector,
/// not a seat identity (same defect as the delivery-path bug fixed in
/// `resolve_caller_agent_id` below). A tool launched with `TACHI_PROFILE` set
/// could still resolve its OWN `from_agent` (the sender identity stamped on
/// a `sticky_leave`) to that shared profile string, so two workers on the
/// same tool profile would author stickies under the same `from_agent`. The
/// sender path now shares the exact same chain the delivery path
/// (`resolve_caller_agent_id`) already trusts: no caller-supplied
/// `agent_id` param here (there is none on the `sticky_leave` FROM side), so
/// this is `server-side agent_profile` -> `TACHI_AGENT_SEAT` -> `None`.
pub(super) fn fallback_agent_id(registered_agent: Option<String>) -> String {
    registered_agent
        .or_else(|| non_empty_env("TACHI_AGENT_SEAT"))
        .unwrap_or_else(|| "unknown-agent".to_string())
}

pub(super) fn resolve_from_agent(server: &MemoryServer) -> String {
    fallback_agent_id(current_agent_id(server))
}

/// Server-side identity resolution for the sticky DELIVERY path (briefing
/// inclusion + `sticky_check`), mirroring the same three-step chain
/// `sticky_leave` already trusts via `resolve_from_agent` above (CP2,
/// opus xhigh review of #964/PR #1003).
///
/// `sticky_leave` resolves `from_agent` server-side; the delivery path used
/// to trust ONLY the caller-supplied `params.agent_id`, with no fallback —
/// so a worker briefing call with no `agent_id` param was silently treated
/// as the leader and consumed broadcast (`to`-absent) stickies meant for the
/// real leader.
///
/// Chain: `params.agent_id` -> server-side `agent_profile.agent_id` ->
/// `TACHI_AGENT_SEAT` env var -> `None` (leader).
///
/// Round-2 fix (codex final review of #964/PR #1003, BUG CP2): the env
/// fallback used to read `TACHI_PROFILE` — but that variable carries the
/// configured *tool profile* (`worker`/`delegate`/`standard`/`codex`, see
/// `dispatch_ops/mcp_config.rs`), a capability-surface selector, not a seat
/// identity. A leader launched with `TACHI_PROFILE=standard` would resolve
/// here as `Some("standard")`, not `None` (leader), and would silently stop
/// consuming its own broadcasts; workers would resolve to a shared
/// capability-profile name instead of their individual seat. `TACHI_AGENT_SEAT`
/// is a dedicated env var, set only by the dispatch harness alongside
/// `TACHI_PROFILE` (see `dispatch_ops/mcp_config.rs::generate_mcp_config`),
/// carrying the actual per-worker seat name.
///
/// The middle link (`agent_profile`) is expected to be structurally `None`
/// once #973 retires `agent_register` at runtime; it is kept here only
/// because this function mirrors the exact chain `handoff_ops::identity`
/// already uses, and because a future non-dispatch caller could still
/// populate it. Dispatch-bound worker seats get `TACHI_AGENT_SEAT` injected
/// by the dispatch harness; handcrafted worker sessions calling the MCP tool
/// directly must pass `agent_id` explicitly (or address via `to:`) —
/// otherwise they resolve to `None` (leader) here, same as an identity-less
/// caller (documented caller-honesty residual: a handcrafted session cannot
/// be forced to self-identify).
pub(crate) fn resolve_caller_agent_id(
    server: &MemoryServer,
    params_agent_id: Option<&str>,
) -> Option<String> {
    params_agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| current_agent_id(server))
        .or_else(|| non_empty_env("TACHI_AGENT_SEAT"))
}
