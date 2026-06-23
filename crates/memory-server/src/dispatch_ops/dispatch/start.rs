use super::*;

pub(super) struct DispatchStart {
    pub(super) dispatch_id: String,
    pub(super) agent_norm: String,
    pub(super) resolved_profile: ResolvedDispatchProfile,
    pub(super) profile_payload: Value,
    pub(super) timeout_secs_for_status: u64,
    pub(super) timeout: Duration,
    pub(super) inject_tachi: bool,
    pub(super) inject_hub: bool,
    pub(super) workspace_dir: PathBuf,
}

// ─── Dispatch start resolution ───────────────────────────────────────────────

pub(super) fn resolve_dispatch_start(
    server: &MemoryServer,
    params: &mut TachiDispatchParams,
    now: chrono::DateTime<Utc>,
) -> Result<DispatchStart, String> {
    let resolved_profile = resolve_and_apply_dispatch_profile_for_server(server, params)?;
    let mut agent_norm = resolved_profile.agent.clone();
    let dispatch_id = new_dispatch_id(now, &agent_norm);

    agent_norm = if agent_norm.eq_ignore_ascii_case("custom") {
        "custom".to_string()
    } else if let Some(def) = resolve_dispatch_agent(&agent_norm) {
        def.name.to_string()
    } else {
        let agent = params.agent.as_deref().unwrap_or("");
        return Err(format!(
            "Unknown agent '{}'. Supported: {}",
            agent.trim(),
            dispatch_agent_help_list()
        ));
    };
    params.agent = Some(agent_norm.clone());

    let profile_payload =
        serde_json::to_value(&resolved_profile).unwrap_or_else(|_| json!({"agent": agent_norm}));
    let timeout_secs_for_status = params.timeout_secs;
    let timeout = Duration::from_secs(timeout_secs_for_status);
    let inject_tachi = params.inject_tachi_mcp.unwrap_or(false);
    let inject_hub = params.inject_hub_mcps.unwrap_or(false);

    // Validate backend/MCP compatibility before creating the run ledger. A
    // rejected dispatch should not leave an empty run directory with no status.
    if inject_tachi || inject_hub {
        if agent_norm == "custom" {
            return Err(
                "inject_tachi_mcp / inject_hub_mcps are not supported for the custom backend."
                    .to_string(),
            );
        }
        let def = resolve_dispatch_agent(&agent_norm).expect("resolved agent");
        if !mcp_inject_supported(def) {
            let hint = match def.name {
                "codex" => "Configure MCP servers in ~/.codex/config.toml instead, or dispatch with agent='claude' or 'grok'.",
                "kimi" => "Dispatch with agent='claude' or 'grok' for Tachi MCP injection.",
                _ => "Use an agent that supports --mcp-config.",
            };
            return Err(format!(
                "inject_tachi_mcp / inject_hub_mcps are not supported for the {} backend. {}",
                def.name, hint
            ));
        }
    }

    let workspace_dir = dispatch_runs_root().join(&dispatch_id);

    Ok(DispatchStart {
        dispatch_id,
        agent_norm,
        resolved_profile,
        profile_payload,
        timeout_secs_for_status,
        timeout,
        inject_tachi,
        inject_hub,
        workspace_dir,
    })
}
