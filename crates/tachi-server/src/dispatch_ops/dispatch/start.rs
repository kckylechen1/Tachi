use super::*;

pub(super) struct DispatchStart {
    pub(super) dispatch_id: String,
    pub(super) request: tachi_params::StaffAssignmentRequest,
    pub(super) legacy_auto_capability_bundle: Option<bool>,
    pub(super) requested_skills: Vec<String>,
    pub(super) context_query: Option<String>,
    pub(super) tool_profile: Option<String>,
    pub(super) command: Vec<String>,
    pub(super) harness_transport: Option<String>,
    pub(super) harness_server_url: Option<String>,
    pub(super) raw_cwd: Option<String>,
    pub(super) raw_credential_profiles: Vec<String>,
    pub(super) agent_norm: String,
    pub(super) resolved_profile: ResolvedDispatchProfile,
    pub(super) resolved_assignment: tachi_params::ResolvedStaffAssignment,
    pub(super) profile_payload: Value,
    pub(super) workspace_dir: PathBuf,
    pub(super) inject_card: bool,
    pub(super) verbose: bool,
}

// ─── Dispatch start resolution ───────────────────────────────────────────────

pub(super) fn resolve_dispatch_start(
    server: &MemoryServer,
    params: &mut TachiDispatchParams,
    now: chrono::DateTime<Utc>,
    execution_level: tachi_params::ExecutionLevel,
) -> Result<DispatchStart, String> {
    // Preserve only the caller's profile spelling for diagnostics/replay. The
    // typed request itself must be minted after profile resolution so omitted
    // stage and other profile defaults reach every typed consumer.
    let raw_request_profile = params.profile.clone();
    let raw_cwd = params.cwd.clone();
    // #1815 P1: resolve into the acknowledged compatibility projection first.
    // The canonical outputs below are minted before that projection reaches the
    // remaining P2/P3 consumers; final #1814 deletes this bridge entirely.
    let mut legacy_projection = params.clone();
    let resolved_profile =
        resolve_and_apply_dispatch_profile_for_server(server, &mut legacy_projection)?;
    reconcile_resolved_profile_compatibility(&mut legacy_projection, &resolved_profile);
    // Credential failure receipts are forensic evidence, not launch authority.
    // Capture the resolved compatibility projection before the grant trims and
    // deduplicates it so profile-added defaults and caller spelling are both
    // visible if materialization fails.
    let raw_credential_profiles = legacy_projection.credential_profiles.clone();
    let mut request =
        tachi_params::StaffAssignmentRequest::from_dispatch_params(&legacy_projection);
    request.profile = raw_request_profile;
    let legacy_auto_capability_bundle = legacy_projection.auto_capability_bundle;
    let mut agent_norm = resolved_profile.agent.clone();
    let dispatch_id = new_dispatch_id(now, &agent_norm);

    agent_norm = if let Some(agent) = normalize_dispatch_agent_name(&agent_norm) {
        agent
    } else {
        let agent = legacy_projection.agent.as_deref().unwrap_or("");
        return Err(format!(
            "Unknown agent '{}'. Supported: {}",
            agent.trim(),
            dispatch_agent_help_list()
        ));
    };
    legacy_projection.agent = Some(agent_norm.clone());

    // #1815 P1: the profile resolver still writes the legacy ingress for
    // untouched P2/P3/#1814 consumers. This typed result is the authoritative
    // selection record; the exact legacy projection is checked below and must
    // be deleted when those consumers migrate.
    let resolved_assignment = tachi_params::ResolvedStaffAssignment {
        assignment_id: dispatch_id.clone(),
        staffing_reason: legacy_projection.staffing_reason,
        selected_worker: agent_norm.clone(),
        selected_profile: resolved_profile.selected_profile.clone(),
        selected_backend: agent_norm.clone(),
        selected_model: legacy_projection.model.clone(),
        execution_level: Some(execution_level),
        recommendation_ref: None,
        host_adapter: resolved_profile.host_adapter.clone(),
        evidence_required: resolved_profile.evidence_required.clone(),
        fallback_chain: resolved_profile.fallback_chain.clone(),
        route_explanation: resolved_profile.route_explanation.clone(),
        identity_receipt: serde_json::to_value(&resolved_profile.identity_receipt)
            .unwrap_or(serde_json::Value::Null),
    };
    apply_assignment_legacy_projection(
        params,
        legacy_projection,
        &resolved_assignment,
        &resolved_profile,
    )?;

    let profile_payload =
        serde_json::to_value(&resolved_profile).unwrap_or_else(|_| json!({"agent": agent_norm}));
    let inject_card = params.inject_card.unwrap_or(true);
    let verbose = params.verbose.unwrap_or(false);

    // Validate backend/MCP compatibility before creating the run ledger. A
    // rejected dispatch should not leave an empty run directory with no status.
    if params.inject_tachi_mcp.unwrap_or(false) || params.inject_hub_mcps.unwrap_or(false) {
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
    Ok(DispatchStart {
        dispatch_id,
        request,
        legacy_auto_capability_bundle,
        requested_skills: params.skills.clone(),
        context_query: params.context_query.clone(),
        tool_profile: params.tool_profile.clone(),
        command: params.command.clone(),
        harness_transport: params.harness_transport.clone(),
        harness_server_url: params.harness_server_url.clone(),
        raw_cwd,
        raw_credential_profiles,
        agent_norm,
        resolved_profile,
        resolved_assignment,
        profile_payload,
        workspace_dir,
        inject_card,
        verbose,
    })
}

/// #1815 P1 compatibility assertion. P2/P3 migrate these consumers to the
/// typed assignment; final #1814 removes the legacy `TachiDispatchParams`
/// projection. Keeping this comparison at the ingress makes a one-sided edit
/// fail before receipt, artifact, backend, or spawn work begins.
pub(super) fn assert_assignment_legacy_projection(
    params: &TachiDispatchParams,
    assignment: &tachi_params::ResolvedStaffAssignment,
    resolved_profile: &ResolvedDispatchProfile,
) -> Result<(), String> {
    let matches = assignment.staffing_reason == params.staffing_reason
        && params.agent.as_deref() == Some(assignment.selected_backend.as_str())
        && assignment.selected_worker == assignment.selected_backend
        && assignment.selected_profile
            == params
                .profile
                .as_deref()
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_string)
        && assignment.selected_model == params.model
        && assignment.execution_level == params.execution_level
        && assignment.recommendation_ref.is_none()
        && assignment.host_adapter == resolved_profile.host_adapter
        && assignment.evidence_required == resolved_profile.evidence_required
        && assignment.fallback_chain == resolved_profile.fallback_chain
        && assignment.route_explanation == resolved_profile.route_explanation
        && assignment.identity_receipt
            == serde_json::to_value(&resolved_profile.identity_receipt)
                .unwrap_or(serde_json::Value::Null);
    if matches {
        Ok(())
    } else {
        Err("dispatch assignment and legacy compatibility projection diverged".to_string())
    }
}

pub(super) fn assert_nested_mcp_profile_projection(
    params: &TachiDispatchParams,
    resolved_profile: &ResolvedDispatchProfile,
) -> Result<(), String> {
    if serde_json::to_value(&params.mcp_access).ok()
        == serde_json::to_value(Some(&resolved_profile.mcp_access)).ok()
    {
        Ok(())
    } else {
        Err("nested MCP profile projection diverged from resolved profile".to_string())
    }
}

fn apply_assignment_legacy_projection(
    params: &mut TachiDispatchParams,
    mut legacy_projection: TachiDispatchParams,
    assignment: &tachi_params::ResolvedStaffAssignment,
    resolved_profile: &ResolvedDispatchProfile,
) -> Result<(), String> {
    // This is the sole temporary P1 write-back. The selected values come from
    // the typed assignment; the remaining fields were resolved by the private
    // ResolvedDispatchProfile context while preserving untouched caller input.
    legacy_projection.agent = Some(assignment.selected_backend.clone());
    legacy_projection.model = assignment.selected_model.clone();
    legacy_projection.execution_level = assignment.execution_level;
    *params = legacy_projection;
    assert_assignment_legacy_projection(params, assignment, resolved_profile)
}

/// Preserve the resolver's nested MCP profile context for profile-facing
/// consumers. The existing top-level MCP fields remain the independent
/// workspace/launch authority and are reconciled into the grant later. Credential
/// profiles are the resolver's trimmed and deduplicated context, not the caller's
/// raw spelling.
fn reconcile_resolved_profile_compatibility(
    legacy_projection: &mut TachiDispatchParams,
    resolved_profile: &ResolvedDispatchProfile,
) {
    legacy_projection.mcp_access = Some(resolved_profile.mcp_access.clone());
}
