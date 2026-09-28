//! Static registry for `tachi_dispatch` CLI workers (Phase 1 fleet).
//!
//! Canonical policy: `docs/engineering/architecture/agent-fleet.md`.

pub(crate) use tachi_dispatch::{
    dispatch_agent_help_list, fallback_chain, mcp_inject_supported, normalize_dispatch_agent_name,
    resolve_dispatch_agent, select_agent_for_task, DISPATCH_AGENTS,
};

pub(crate) fn registry_json(server: &crate::MemoryServer) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "agents": DISPATCH_AGENTS.iter().map(|a| serde_json::json!({
            "name": a.name,
            "display_name": a.display_name,
            "aliases": a.aliases,
            "binary": a.binary,
            "default_timeout_secs": a.default_timeout_secs,
            "mcp_inject": mcp_inject_supported(a),
        })).collect::<Vec<_>>(),
        // `tachi_agents` list/profiles is an established registry consumer of
        // the server-side projected dispatch profile rows. This surface is
        // separate from the model-facing `tachi_task` action inventory and
        // from the local operator diagnostics command.
        "dispatch_profiles": crate::dispatch_profile::dispatch_profiles_json_for_server(server, true)?
            .get("dispatch_profiles")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        "fleet_policy": "docs/engineering/architecture/agent-fleet.md",
    }))
}

pub(crate) async fn handle_agents(
    server: &crate::MemoryServer,
    params: crate::tool_params::TachiAgentsParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    match action.as_str() {
        "list" | "profiles" => {
            serde_json::to_string(&registry_json(server)?).map_err(|e| format!("serialize: {e}"))
        }
        "select" => {
            let intent = params.intent.as_deref().unwrap_or("other");
            let task = params.task.as_deref().unwrap_or("");
            let primary = select_agent_for_task(intent, task);
            let chain = fallback_chain(primary);
            serde_json::to_string(&serde_json::json!({
                "intent": intent,
                "agent": primary,
                "fallback_chain": chain,
            }))
            .map_err(|e| format!("serialize: {e}"))
        }
        other => Err(format!(
            "Invalid agents action '{other}'. Use list or select."
        )),
    }
}
