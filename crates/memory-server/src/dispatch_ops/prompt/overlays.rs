use crate::tool_params::TachiDispatchParams;
use crate::MemoryServer;

pub(super) fn render_task_route_overlay(route: &crate::copilot_ops::TaskBriefRouting) -> String {
    let intent = route.intent;
    let mut lines = vec![
        "## Tachi task route".to_string(),
        format!("- intent: {intent}"),
    ];

    let labels = route
        .selected_sops
        .iter()
        .take(4)
        .filter_map(|sop| {
            let id = sop.get("id").and_then(|v| v.as_str())?;
            let reason = sop.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            Some(if reason.is_empty() {
                format!("  - {id}")
            } else {
                format!("  - {id}: {reason}")
            })
        })
        .collect::<Vec<_>>();
    if !labels.is_empty() {
        lines.push("- selected_sops:".to_string());
        lines.extend(labels);
    }

    let steps = route
        .tool_plan
        .iter()
        .take(5)
        .filter_map(|step| {
            let tool = step.get("tool").and_then(|v| v.as_str())?;
            let action = step.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let when = step.get("when").and_then(|v| v.as_str()).unwrap_or("");
            Some(format!("  - {tool}({action}): {when}"))
        })
        .collect::<Vec<_>>();
    if !steps.is_empty() {
        lines.push("- tool_plan:".to_string());
        lines.extend(steps);
    }

    lines.join("\n")
}

pub(super) fn render_dispatch_profile_overlay(
    server: &MemoryServer,
    params: &TachiDispatchParams,
) -> String {
    let mut lines = vec!["## Dispatch profile".to_string()];
    if let Some(profile) = params.profile.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- profile: {profile}"));
        if let Some(profile_def) = crate::dispatch_profile::resolve_dispatch_profile(profile) {
            lines.push("- skill_loadout:".to_string());
            match crate::dispatch_profile::profile_skill_loadout_json_for_server(
                server,
                profile_def,
            ) {
                Ok(loadout) => {
                    for label in [
                        "common_skills",
                        "signature_skills",
                        "projected_signature_skills",
                        "passive_traits",
                        "projected_passive_traits",
                        "forbidden_skills",
                    ] {
                        let items = loadout
                            .get(label)
                            .and_then(|value| value.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.as_str())
                            .collect::<Vec<_>>();
                        if !items.is_empty() {
                            lines.push(format!("  - {label}: {}", items.join(", ")));
                        }
                    }
                    if let Some(status) = loadout
                        .get("projection")
                        .and_then(|projection| projection.get("status"))
                        .and_then(|status| status.as_str())
                    {
                        lines.push(format!("  - projection_status: {status}"));
                    }
                }
                Err(err) => {
                    lines.push(format!("  - loadout_error: {err}"));
                }
            }
            match crate::dispatch_profile::profile_evidence_contract_json_for_server(
                server,
                profile_def,
            ) {
                Ok(contract) => {
                    lines.push("- evidence_contract:".to_string());
                    for label in ["required", "projected_required"] {
                        let items = contract
                            .get(label)
                            .and_then(|value| value.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.as_str())
                            .collect::<Vec<_>>();
                        if !items.is_empty() {
                            lines.push(format!("  - {label}: {}", items.join(", ")));
                        }
                    }
                    if let Some(status) = contract
                        .get("projection")
                        .and_then(|projection| projection.get("status"))
                        .and_then(|status| status.as_str())
                    {
                        lines.push(format!("  - evidence_projection_status: {status}"));
                    }
                }
                Err(err) => {
                    lines.push(format!("  - evidence_contract_error: {err}"));
                }
            }
            match crate::dispatch_profile::profile_json_for_server(server, profile_def) {
                Ok(profile_json) => {
                    if let Some(card) = profile_json.get("mbit_card") {
                        let card_fields = ["projected_weak_against", "demotion_targets"];
                        let mut emitted = false;
                        for label in card_fields {
                            let items = card
                                .get(label)
                                .and_then(|value| value.as_array())
                                .into_iter()
                                .flatten()
                                .filter_map(|value| value.as_str())
                                .collect::<Vec<_>>();
                            if !items.is_empty() {
                                if !emitted {
                                    lines.push("- mbit_card_evolution:".to_string());
                                    emitted = true;
                                }
                                lines.push(format!("  - {label}: {}", items.join(", ")));
                            }
                        }
                    }
                }
                Err(err) => {
                    lines.push(format!("  - mbit_card_error: {err}"));
                }
            }
        }
    }
    if let Some(agent) = params.agent.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- backend: {agent}"));
    }
    if let Some(stage) = params.stage.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- stage: {stage}"));
    }
    if let Some(tool_profile) = params
        .tool_profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(format!("- tachi_tool_profile: {tool_profile}"));
    }
    if let Some(flow_id) = params.flow_id.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- flow_id: {flow_id}"));
    }
    if let Some(issue_ref) = params.issue_ref.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- issue_ref: {issue_ref}"));
    }
    if let Some(pr_ref) = params.pr_ref.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- pr_ref: {pr_ref}"));
    }
    if let Some(access) = params.mcp_access.as_ref() {
        if let Ok(compact) = serde_json::to_string(access) {
            lines.push(format!("- tool_access: {compact}"));
        }
    }
    if !params.allowed_mcp_servers.is_empty() {
        lines.push(format!(
            "- allowed_mcp_servers: {}",
            params.allowed_mcp_servers.join(", ")
        ));
    }
    lines.push("- completion_report: report files changed, tests run, blockers, and any unavailable MCP/GitHub context explicitly.".to_string());
    lines.join("\n")
}
