use super::*;

#[cfg(test)]
pub(crate) fn resolve_and_apply_dispatch_profile(
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    resolve_and_apply_dispatch_profile_inner(None, params)
}

pub(crate) fn resolve_and_apply_dispatch_profile_for_server(
    server: &MemoryServer,
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    resolve_and_apply_dispatch_profile_inner(Some(server), params)
}

fn resolve_and_apply_dispatch_profile_inner(
    server: Option<&MemoryServer>,
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    let mut route_explanation = Vec::new();
    let requested_agent = params.agent.clone().filter(|s| !s.trim().is_empty());
    let profile = match params.profile.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(raw) => Some(resolve_dispatch_profile(raw).ok_or_else(|| {
            format!(
                "Unknown dispatch profile '{}'. Supported: {}",
                raw.trim(),
                DISPATCH_PROFILES
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?),
        None => None,
    };

    if let Some(profile) = profile {
        route_explanation.push(format!(
            "selected DispatchProfile '{}' ({})",
            profile.name, profile.role
        ));
        if requested_agent.is_none() {
            params.agent = Some(profile.backend.to_string());
            route_explanation.push(format!("profile selected backend '{}'", profile.backend));
        } else if requested_agent.as_deref() != Some(profile.backend) {
            route_explanation.push(format!(
                "explicit agent '{}' overrides profile backend '{}'",
                requested_agent.as_deref().unwrap_or(""),
                profile.backend
            ));
        }
        if params.stage.is_none() {
            params.stage = profile.stage.map(str::to_string);
        }
        if params.model.is_none() {
            params.model = profile.model.map(str::to_string);
        }
        if profile_uses_opencode_adapter(profile) && params.command.is_empty() {
            apply_opencode_profile_command(params, profile, &mut route_explanation)?;
        }
        if params.tool_profile.is_none() {
            params.tool_profile = Some(profile.tool_profile.to_string());
        }
        if params.inject_tachi_mcp.is_none() {
            params.inject_tachi_mcp = Some(profile.inject_tachi_mcp);
        }
        if params.inject_hub_mcps.is_none() {
            params.inject_hub_mcps = Some(profile.inject_hub_mcps);
        }
        if params.auto_capability_bundle.is_none() {
            let effective_stage = params.stage.as_deref().or(profile.stage);
            if matches!(effective_stage, Some("review" | "review_light")) {
                params.auto_capability_bundle = Some(false);
                route_explanation.push(
                    "auto_capability_bundle disabled by default for review-stage dispatch (#457); pass auto_capability_bundle=true to override"
                        .to_string(),
                );
            } else {
                params.auto_capability_bundle = Some(profile.auto_capability_bundle);
            }
        }
        if params.skills.is_empty() {
            params.skills = match server {
                Some(server) => profile_required_skill_ids_for_server(server, profile)?,
                None => profile_required_skill_ids(profile),
            };
        }
        if params.mcp_access.is_none() {
            params.mcp_access = Some(DispatchMcpAccessParams {
                inject_tachi_mcp: Some(profile.inject_tachi_mcp),
                inject_hub_mcps: Some(profile.inject_hub_mcps),
                allowed_facades: profile
                    .allowed_facades
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                allowed_mcp_servers: profile
                    .allowed_mcp_servers
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                github_read: Some(profile.github_read),
                write_actions: Some(profile.write_actions),
                issue_refs: params.issue_ref.iter().cloned().collect(),
                pr_refs: params.pr_ref.iter().cloned().collect(),
                fallback: Some(if profile.github_read {
                    "Use MCP/GitHub read tools when available; if unavailable, report issue_context_unavailable instead of guessing."
                } else {
                    "Use leader-provided issue packet; do not perform GitHub writes."
                }.to_string()),
            });
        }
        if params.allowed_mcp_servers.is_empty() {
            params.allowed_mcp_servers = profile
                .allowed_mcp_servers
                .iter()
                .map(|s| s.to_string())
                .collect();
        }
        let mut added_credential_profiles = Vec::new();
        for credential_profile in profile.credential_profiles {
            if !params
                .credential_profiles
                .iter()
                .any(|existing| existing == credential_profile)
            {
                params
                    .credential_profiles
                    .push((*credential_profile).to_string());
                added_credential_profiles.push(*credential_profile);
            }
        }
        if !added_credential_profiles.is_empty() {
            route_explanation.push(format!(
                "profile requires credential profile(s): {}",
                added_credential_profiles.join(", ")
            ));
        }
    }
    if params.allowed_mcp_servers.is_empty() {
        if let Some(access) = params.mcp_access.as_ref() {
            params.allowed_mcp_servers = access.allowed_mcp_servers.clone();
        }
    }

    let agent = params
        .agent
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "agent or profile is required for dispatch".to_string())?;
    let agent_norm = normalize_dispatch_agent_name(&agent).unwrap_or(agent);
    let mcp_access = params
        .mcp_access
        .get_or_insert_with(|| DispatchMcpAccessParams {
            inject_tachi_mcp: params.inject_tachi_mcp,
            inject_hub_mcps: params.inject_hub_mcps,
            allowed_facades: Vec::new(),
            allowed_mcp_servers: params.allowed_mcp_servers.clone(),
            github_read: Some(params.issue_ref.is_some() || params.pr_ref.is_some()),
            write_actions: Some(false),
            issue_refs: params.issue_ref.iter().cloned().collect(),
            pr_refs: params.pr_ref.iter().cloned().collect(),
            fallback: Some(
                "Use leader-provided context if GitHub/MCP issue reads are unavailable."
                    .to_string(),
            ),
        })
        .clone();
    let evidence_required = match (server, profile) {
        (Some(server), Some(profile)) => profile_evidence_required_for_server(server, profile)?,
        (_, Some(profile)) => profile_evidence_required(profile),
        _ => Vec::new(),
    };
    let fallback_chain = fallback_chain(&agent_norm)
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut credential_profiles = params
        .credential_profiles
        .iter()
        .map(|profile| profile.trim().to_string())
        .filter(|profile| !profile.is_empty())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut credential_profiles);

    Ok(ResolvedDispatchProfile {
        selected_profile: profile.map(|p| p.name.to_string()),
        agent: agent_norm,
        role: profile.map(|p| p.role.to_string()),
        tool_profile: params.tool_profile.clone(),
        auto_capability_bundle: params.auto_capability_bundle.unwrap_or(false),
        mcp_access,
        evidence_required,
        fallback_chain,
        credential_profiles,
        route_explanation,
        host_adapter: profile.and_then(profile_host_adapter).map(str::to_string),
        mbit_card: profile
            .map(|profile| match server {
                Some(server) => profile_json_for_server(server, profile),
                None => Ok(profile_json(profile)),
            })
            .transpose()?,
    })
}

fn apply_opencode_profile_command(
    params: &mut TachiDispatchParams,
    profile: &DispatchProfileDef,
    route_explanation: &mut Vec<String>,
) -> Result<(), String> {
    let model = profile
        .model
        .ok_or_else(|| format!("profile '{}' uses OpenCode but has no model", profile.name))?;
    let transport = params
        .harness_transport
        .clone()
        .or_else(|| std::env::var("TACHI_OPENCODE_TRANSPORT").ok())
        .unwrap_or_else(|| "cli".to_string())
        .to_ascii_lowercase();
    if matches!(transport.as_str(), "serve" | "opencode_serve" | "server") {
        let server_url = params
            .harness_server_url
            .clone()
            .or_else(|| std::env::var("TACHI_OPENCODE_SERVER_URL").ok())
            .unwrap_or_else(|| "http://127.0.0.1:4321".to_string());
        if crate::dispatch_ops::harness_server_attach_ready(&server_url) {
            let directory = params
                .cwd
                .clone()
                .or_else(|| {
                    std::env::current_dir()
                        .ok()
                        .map(|path| path.to_string_lossy().to_string())
                })
                .unwrap_or_else(|| ".".to_string());
            params.harness_transport = Some("opencode_serve".to_string());
            params.harness_server_url = Some(server_url.clone());
            params.command = vec![
                "opencode".to_string(),
                "run".to_string(),
                "--attach".to_string(),
                server_url,
                "--dir".to_string(),
                directory,
                "--agent".to_string(),
                profile.role.to_string(),
                "--model".to_string(),
                model.to_string(),
            ];
            route_explanation.push(format!(
                "profile selected typed OpenCode serve transport for model '{}'",
                model
            ));
        } else {
            params.harness_transport = Some("opencode_cli".to_string());
            params.harness_server_url = Some(server_url.clone());
            params.command = vec![
                "opencode".to_string(),
                "--pure".to_string(),
                "run".to_string(),
                "--model".to_string(),
                model.to_string(),
            ];
            route_explanation.push(format!(
                "requested opencode serve at {server_url}, but readiness probe failed; falling back to opencode CLI for model '{model}'"
            ));
        }
    } else {
        params.harness_transport = Some("opencode_cli".to_string());
        params.command = vec![
            "opencode".to_string(),
            "--pure".to_string(),
            "run".to_string(),
            "--model".to_string(),
            model.to_string(),
        ];
        route_explanation.push(format!(
            "profile selected typed OpenCode CLI transport for model '{}'",
            model
        ));
    }
    Ok(())
}
