use super::bundle::{build_bundle_section, infer_host_tools};
use super::scoring::{normalize_host_label, recommend_capabilities_inner};
use super::types::CapabilityBundle;
use crate::tool_params::{
    PrepareCapabilityBundleParams, RecommendCapabilityParams, RecommendSkillParams,
    RecommendToolchainParams,
};
use crate::MemoryServer;
use serde_json::json;

pub(crate) async fn handle_recommend_capability(
    server: &MemoryServer,
    params: RecommendCapabilityParams,
) -> Result<String, String> {
    let results = recommend_capabilities_inner(
        server,
        &params.query,
        params.host.as_deref(),
        params.cap_type.as_deref(),
        params.limit.max(1),
        params.include_hidden,
        params.include_uncallable,
    )?;

    serde_json::to_string(&json!({
        "query": params.query,
        "host": normalize_host_label(params.host.as_deref()),
        "recommendations": results,
        "count": results.len(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_recommend_skill(
    server: &MemoryServer,
    params: RecommendSkillParams,
) -> Result<String, String> {
    let results = recommend_capabilities_inner(
        server,
        &params.query,
        params.host.as_deref(),
        Some("skill"),
        params.limit.max(1),
        false,
        params.include_uncallable,
    )?;

    serde_json::to_string(&json!({
        "query": params.query,
        "host": normalize_host_label(params.host.as_deref()),
        "skills": results,
        "count": results.len(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_recommend_toolchain(
    server: &MemoryServer,
    params: RecommendToolchainParams,
) -> Result<String, String> {
    let host = normalize_host_label(params.host.as_deref());
    let skills = recommend_capabilities_inner(
        server,
        &params.query,
        host.as_deref(),
        Some("skill"),
        params.skill_limit.max(1),
        false,
        false,
    )?;
    let capabilities = recommend_capabilities_inner(
        server,
        &params.query,
        host.as_deref(),
        None,
        params.capability_limit.max(1),
        false,
        false,
    )?
    .into_iter()
    .filter(|rec| rec.cap_type != "skill")
    .take(params.capability_limit.max(1))
    .collect::<Vec<_>>();
    let host_tools = infer_host_tools(&params.query);

    let mut rationale = Vec::new();
    if let Some(top) = skills.first() {
        rationale.push(format!("Top skill match: {}", top.id));
    }
    if let Some(top) = capabilities.first() {
        rationale.push(format!("Supporting capability: {}", top.id));
    }
    if !host_tools.is_empty() {
        rationale.push(format!("Suggested host tools: {}", host_tools.join(", ")));
    }

    serde_json::to_string(&json!({
        "query": params.query,
        "host": host,
        "skills": skills,
        "capabilities": capabilities,
        "host_tools": host_tools,
        "rationale": rationale,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_prepare_capability_bundle(
    server: &MemoryServer,
    params: PrepareCapabilityBundleParams,
) -> Result<String, String> {
    let host = normalize_host_label(params.host.as_deref());
    let skills = recommend_capabilities_inner(
        server,
        &params.query,
        host.as_deref(),
        Some("skill"),
        params.skill_limit.max(1),
        false,
        false,
    )?;
    let primary_skill = skills.first().cloned();
    let supporting_capabilities = recommend_capabilities_inner(
        server,
        &params.query,
        host.as_deref(),
        None,
        params.capability_limit.max(1) + 1,
        false,
        false,
    )?
    .into_iter()
    .filter(|rec| rec.cap_type != "skill")
    .take(params.capability_limit.max(1))
    .collect::<Vec<_>>();
    let host_tools = infer_host_tools(&params.query);

    let mut activation_steps = Vec::new();
    if let Some(skill) = primary_skill.as_ref() {
        if let Some(tool_name) = skill.suggested_tool_name.as_ref() {
            activation_steps.push(format!(
                "Load or call {tool_name} as the primary skill path."
            ));
        } else {
            activation_steps.push(format!("Start with skill {}.", skill.id));
        }
    }
    if !host_tools.is_empty() {
        activation_steps.push(format!(
            "Grant or prepare host tools: {}.",
            host_tools.join(", ")
        ));
    }
    for capability in supporting_capabilities.iter().take(2) {
        activation_steps.push(format!(
            "Keep {} available as a supporting capability.",
            capability.id
        ));
    }

    let mut rationale = Vec::new();
    if let Some(skill) = primary_skill.as_ref() {
        rationale.push(format!("Primary skill match: {}", skill.id));
    }
    if !host_tools.is_empty() {
        rationale.push(format!("Host tool fit: {}", host_tools.join(", ")));
    }
    rationale.extend(
        supporting_capabilities
            .iter()
            .take(2)
            .map(|cap| format!("Supporting capability: {}", cap.id)),
    );

    let section = if params.include_section {
        Some(build_bundle_section(
            &params.query,
            primary_skill.as_ref(),
            &supporting_capabilities,
            &host_tools,
            &activation_steps,
        ))
    } else {
        None
    };

    let bundle = CapabilityBundle {
        primary_skill,
        supporting_capabilities,
        host_tools,
        activation_steps,
        rationale,
        section,
    };

    serde_json::to_string(&json!({
        "query": params.query,
        "host": host,
        "bundle": bundle,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
