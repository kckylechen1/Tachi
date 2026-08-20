use super::*;

pub(super) struct DispatchStart {
    pub(super) dispatch_id: String,
    pub(super) agent_norm: String,
    pub(super) resolved_profile: ResolvedDispatchProfile,
    pub(super) resolved_assignment: tachi_params::ResolvedStaffAssignment,
    pub(super) profile_payload: Value,
    pub(super) timeout: Duration,
    pub(super) inject_tachi: bool,
    pub(super) inject_hub: bool,
    pub(super) workspace_dir: PathBuf,
    pub(super) host_adapter: Option<String>,
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

    agent_norm = if let Some(agent) = normalize_dispatch_agent_name(&agent_norm) {
        agent
    } else {
        let agent = params.agent.as_deref().unwrap_or("");
        return Err(format!(
            "Unknown agent '{}'. Supported: {}",
            agent.trim(),
            dispatch_agent_help_list()
        ));
    };
    params.agent = Some(agent_norm.clone());

    // #1815 P1: the profile resolver still writes the legacy ingress for
    // untouched P2/P3/#1814 consumers. This typed result is the authoritative
    // selection record; the exact legacy projection is checked below and must
    // be deleted when those consumers migrate.
    let resolved_assignment = tachi_params::ResolvedStaffAssignment {
        assignment_id: dispatch_id.clone(),
        staffing_reason: params.staffing_reason.clone(),
        selected_worker: agent_norm.clone(),
        selected_profile: resolved_profile.selected_profile.clone(),
        selected_backend: agent_norm.clone(),
        selected_model: params.model.clone(),
        execution_level: params.execution_level.clone(),
        recommendation_ref: None,
        host_adapter: resolved_profile.host_adapter.clone(),
        evidence_required: resolved_profile.evidence_required.clone(),
        fallback_chain: resolved_profile.fallback_chain.clone(),
        route_explanation: resolved_profile.route_explanation.clone(),
        identity_receipt: serde_json::to_value(&resolved_profile.identity_receipt)
            .unwrap_or(serde_json::Value::Null),
    };
    assert_assignment_legacy_projection(params, &resolved_assignment)?;

    let profile_payload =
        serde_json::to_value(&resolved_profile).unwrap_or_else(|_| json!({"agent": agent_norm}));
    let timeout = Duration::from_secs(params.timeout_secs);
    let inject_tachi = params.inject_tachi_mcp.unwrap_or(false);
    let inject_hub = params.inject_hub_mcps.unwrap_or(false);

    // Validate backend/MCP compatibility before creating the run ledger. A
    // rejected dispatch should not leave an empty run directory with no status.
    if inject_tachi || inject_hub {
        if matches!(agent_norm.as_str(), "custom" | "opencode") {
            return Err(
                "inject_tachi_mcp / inject_hub_mcps are not supported for custom/opencode subprocess backends."
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
    let host_adapter = resolved_profile.host_adapter.clone();

    Ok(DispatchStart {
        dispatch_id,
        agent_norm,
        resolved_profile,
        resolved_assignment,
        profile_payload,
        timeout,
        inject_tachi,
        inject_hub,
        workspace_dir,
        host_adapter,
    })
}

/// #1815 P1 compatibility assertion. P2/P3 migrate these consumers to the
/// typed assignment; final #1814 removes the legacy `TachiDispatchParams`
/// projection. Keeping this comparison at the ingress makes a one-sided edit
/// fail before receipt, artifact, backend, or spawn work begins.
pub(super) fn assert_assignment_legacy_projection(
    params: &TachiDispatchParams,
    assignment: &tachi_params::ResolvedStaffAssignment,
) -> Result<(), String> {
    let matches = assignment.staffing_reason == params.staffing_reason
        && params.agent.as_deref() == Some(assignment.selected_backend.as_str())
        && assignment.selected_worker == assignment.selected_backend
        && assignment.selected_profile == params.profile
        && assignment.selected_model == params.model
        && assignment.execution_level == params.execution_level
        && assignment.recommendation_ref.is_none();
    if matches {
        Ok(())
    } else {
        Err("dispatch assignment and legacy compatibility projection diverged".to_string())
    }
}
