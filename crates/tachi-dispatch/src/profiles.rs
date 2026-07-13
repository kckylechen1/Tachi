//! Dispatch profiles describe who to ask, which context/tools to expose, and
//! what evidence a delegated agent must return. They are intentionally separate
//! from `tachi_hub::ToolProfile`, which only gates MCP tool visibility.

use crate::native_skill_ids::{
    CODING_ARCHITECTURE_DECISION, CODING_REFACTOR_CHECKLIST, CODING_TEST_STRATEGY,
    SUPERPOWER_EXECUTING_PLANS, SUPERPOWER_REQUESTING_CODE_REVIEW,
    SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT, SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
    SUPERPOWER_WRITING_PLANS, WAZA_CHECK, WAZA_LEARN, WAZA_READ, WAZA_TACHI, WAZA_THINK,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use tachi_params::{DispatchMcpAccessParams, TachiDispatchParams};

pub const DISPATCH_POLICY_PROPOSAL_NS: &str = "dispatch_route_policy_proposals";
pub const ROUTE_POLICY_RULE_NS: &str = "dispatch_route_policy_rules";
pub const PROFILE_CARD_OVERLAY_NS: &str = "dispatch_profile_card_overlays";
pub const MIN_ROUTE_POLICY_RULE_SAMPLES: u32 = 2;
pub const MIN_LOADOUT_EVOLUTION_SAMPLES: u32 = 10;
pub const MIN_CARD_RISK_EVOLUTION_SAMPLES: u32 = 3;
pub const ROUTE_POLICY_RULE_SCORE_BONUS: f64 = 35.0;

#[derive(Debug, Clone, Copy)]
pub struct DispatchProfileDef {
    pub name: &'static str,
    pub display_name: &'static str,
    pub backend: &'static str,
    pub role: &'static str,
    pub stage: Option<&'static str>,
    pub model: Option<&'static str>,
    pub model_alias: Option<&'static str>,
    /// Explicit profile authorization for a carrier to substitute a model
    /// from a different lineage (#1065). Off everywhere today; a receipt
    /// freezes this at resolution, so nothing downstream can widen it.
    pub allow_cross_lineage_override: bool,
    pub tool_profile: &'static str,
    pub inject_tachi_mcp: bool,
    pub inject_hub_mcps: bool,
    pub github_read: bool,
    pub write_actions: bool,
    pub auto_capability_bundle: bool,
    pub allowed_facades: &'static [&'static str],
    pub allowed_mcp_servers: &'static [&'static str],
    pub credential_profiles: &'static [&'static str],
    pub common_skills: &'static [&'static str],
    pub signature_skills: &'static [&'static str],
    pub passive_traits: &'static [&'static str],
    pub forbidden_skills: &'static [&'static str],
    pub evidence_required: &'static [&'static str],
    pub strong_against: &'static [&'static str],
    pub weak_against: &'static [&'static str],
}

pub const DISPATCH_PROFILES: &[DispatchProfileDef] = &[
    DispatchProfileDef {
        name: "claude_plan",
        display_name: "Claude Plan",
        backend: "claude",
        role: "planner",
        stage: Some("plan"),
        model: None,
        model_alias: None,
        allow_cross_lineage_override: false,
        tool_profile: "delegate",
        inject_tachi_mcp: true,
        inject_hub_mcps: false,
        github_read: true,
        write_actions: false,
        auto_capability_bundle: true,
        allowed_facades: &[
            "tachi_briefing",
            "tachi_memory",
            "tachi_event",
            "tachi_wiki",
            "tachi_task",
        ],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[SUPERPOWER_WRITING_PLANS, WAZA_THINK],
        signature_skills: &[
            SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT,
            CODING_ARCHITECTURE_DECISION,
        ],
        passive_traits: &["plan_before_execute", "surface_open_questions"],
        forbidden_skills: &["direct_file_edits_without_plan"],
        evidence_required: &["plan", "risks", "validation_plan"],
        strong_against: &["planning", "requirements", "feature_breakdown"],
        weak_against: &["direct_execution", "merge"],
    },
    DispatchProfileDef {
        name: "glm_impl",
        display_name: "GLM Implementer",
        backend: "custom",
        role: "executor",
        stage: Some("execute"),
        model: None,
        model_alias: Some(crate::GLM_CODING_MODEL_ALIAS),
        allow_cross_lineage_override: false,
        tool_profile: "delegate",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: false,
        write_actions: true,
        auto_capability_bundle: true,
        allowed_facades: &["tachi_memory", "tachi_event", "tachi_task"],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[SUPERPOWER_EXECUTING_PLANS],
        signature_skills: &[WAZA_TACHI, CODING_TEST_STRATEGY],
        passive_traits: &["bounded_diff", "tests_required", "leader_owns_merge"],
        forbidden_skills: &["unbounded_redesign", "silent_test_workaround"],
        evidence_required: &["diff", "tests_run", "files_changed"],
        strong_against: &["bounded_patch", "implementation"],
        weak_against: &["ambiguous_architecture", "unbounded_refactor"],
    },
    DispatchProfileDef {
        name: "opencode_builder",
        display_name: "OpenCode Credentialed Builder",
        backend: "opencode",
        role: "executor",
        stage: Some("execute"),
        model: None,
        model_alias: Some(crate::GLM_CODING_MODEL_ALIAS),
        allow_cross_lineage_override: false,
        tool_profile: "delegate",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: false,
        write_actions: true,
        auto_capability_bundle: true,
        allowed_facades: &["tachi_memory", "tachi_event", "tachi_task"],
        allowed_mcp_servers: &[],
        credential_profiles: &["opencode_shared"],
        common_skills: &[SUPERPOWER_EXECUTING_PLANS],
        signature_skills: &[WAZA_TACHI, CODING_TEST_STRATEGY],
        passive_traits: &[
            "bounded_diff",
            "tests_required",
            "credentialed_opencode_config",
        ],
        forbidden_skills: &["unbounded_redesign", "silent_test_workaround"],
        evidence_required: &["diff", "tests_run", "files_changed"],
        strong_against: &["bounded_patch", "implementation", "credentialed_dispatch"],
        weak_against: &["ambiguous_architecture", "unbounded_refactor"],
    },
    DispatchProfileDef {
        name: "codex_55_review",
        display_name: "Codex Senior Reviewer",
        backend: "codex",
        role: "senior_reviewer",
        stage: Some("review"),
        model: None,
        model_alias: None,
        allow_cross_lineage_override: false,
        tool_profile: "standard",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: true,
        write_actions: false,
        auto_capability_bundle: false,
        allowed_facades: &[
            "tachi_briefing",
            "tachi_memory",
            "tachi_event",
            "tachi_wiki",
            "tachi_task",
        ],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[
            SUPERPOWER_REQUESTING_CODE_REVIEW,
            SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
            WAZA_CHECK,
        ],
        signature_skills: &[CODING_REFACTOR_CHECKLIST],
        passive_traits: &["strict_on_missing_tests", "schema_boundary_sense"],
        forbidden_skills: &["large_rewrite", "merge_without_evidence"],
        evidence_required: &["findings_by_severity", "file_refs", "verification_advice"],
        strong_against: &[
            "schema_migration",
            "dispatch_refactor",
            "eval_ledger_changes",
        ],
        weak_against: &["low_risk_docs", "copyedit"],
    },
    DispatchProfileDef {
        name: "codex_53_fast",
        display_name: "Codex Fast Checker",
        backend: "codex",
        role: "fast_checker",
        stage: Some("review_light"),
        model: None,
        model_alias: None,
        allow_cross_lineage_override: false,
        tool_profile: "observe",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: false,
        write_actions: false,
        auto_capability_bundle: false,
        allowed_facades: &["tachi_memory", "tachi_event"],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[WAZA_CHECK],
        signature_skills: &[SUPERPOWER_VERIFICATION_BEFORE_COMPLETION],
        passive_traits: &["fast_sanity_only", "flag_uncertainty"],
        forbidden_skills: &["deep_architecture_review", "write_actions"],
        evidence_required: &["summary", "risk_flags"],
        strong_against: &["quick_sanity", "low_risk_review"],
        weak_against: &["schema_migration", "security_review"],
    },
    DispatchProfileDef {
        name: "kimi_arch",
        display_name: "Kimi Architecture Critic",
        backend: "kimi",
        role: "architect",
        stage: Some("plan_review"),
        model: None,
        model_alias: None,
        allow_cross_lineage_override: false,
        tool_profile: "observe",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: true,
        write_actions: false,
        auto_capability_bundle: true,
        allowed_facades: &["tachi_memory", "tachi_event", "tachi_wiki"],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[WAZA_THINK, CODING_ARCHITECTURE_DECISION],
        signature_skills: &[SUPERPOWER_WRITING_PLANS],
        passive_traits: &["challenge_assumptions", "prefer_rejected_options"],
        forbidden_skills: &["shell_execution", "git_write"],
        evidence_required: &["risks", "rejected_options", "file_refs"],
        strong_against: &["architecture", "schema_boundary", "lifecycle_design"],
        weak_against: &["shell_execution", "git_write"],
    },
    DispatchProfileDef {
        name: "deepseek_explore",
        display_name: "DeepSeek Explorer",
        backend: "custom",
        role: "explore",
        stage: Some("explore"),
        model: Some("deepseek/deepseek-v4-flash"),
        model_alias: None,
        allow_cross_lineage_override: false,
        tool_profile: "observe",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: false,
        write_actions: false,
        auto_capability_bundle: false,
        allowed_facades: &["tachi_memory", "tachi_event"],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[WAZA_READ],
        signature_skills: &[WAZA_LEARN],
        passive_traits: &["read_only_mapping", "cite_files"],
        forbidden_skills: &["implementation", "final_claims_without_verification"],
        evidence_required: &["file_map", "caveats", "test_targets"],
        strong_against: &["repo_mapping", "symbol_search"],
        weak_against: &["implementation", "final_verification"],
    },
    DispatchProfileDef {
        name: "kimi_ux",
        display_name: "Kimi UX Experience Reviewer",
        backend: "kimi",
        role: "ux_researcher",
        stage: Some("review_light"),
        model: None,
        model_alias: None,
        allow_cross_lineage_override: false,
        tool_profile: "observe",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: true,
        write_actions: false,
        auto_capability_bundle: false,
        allowed_facades: &[
            "tachi_briefing",
            "tachi_memory",
            "tachi_event",
            "tachi_task",
            "tachi_wiki",
        ],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[WAZA_CHECK, WAZA_THINK],
        signature_skills: &[WAZA_LEARN, SUPERPOWER_VERIFICATION_BEFORE_COMPLETION],
        passive_traits: &[
            "agent_facing_ux_audit",
            "report_failure_chain",
            "recommend_workflow_changes",
        ],
        forbidden_skills: &["write_actions", "direct_implementation"],
        evidence_required: &[
            "ux_findings",
            "failure_chain",
            "workflow_recommendations",
            "repro_steps",
        ],
        strong_against: &[
            "agent_experience",
            "workflow_ux",
            "tool_surface_friction",
            "ux_matrix",
        ],
        weak_against: &["implementation", "merge", "schema_migration"],
    },
];

#[derive(Debug, Clone, Copy)]
pub struct DispatchProfileAlias {
    pub alias: &'static str,
    pub replacement: &'static str,
    pub reason: &'static str,
    pub release_window: &'static str,
}

pub const DISPATCH_PROFILE_ALIASES: &[DispatchProfileAlias] = &[DispatchProfileAlias {
    alias: "glm_51_impl",
    replacement: "glm_impl",
    reason: "deprecated compatibility alias; GLM executor profiles now resolve through the glm_coding model card",
    release_window: "one_release_window",
}];

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedDispatchProfile {
    pub selected_profile: Option<String>,
    pub agent: String,
    pub role: Option<String>,
    pub tool_profile: Option<String>,
    pub auto_capability_bundle: bool,
    pub mcp_access: DispatchMcpAccessParams,
    pub evidence_required: Vec<String>,
    pub fallback_chain: Vec<String>,
    pub credential_profiles: Vec<String>,
    pub route_explanation: Vec<String>,
    pub host_adapter: Option<String>,
    pub mbit_card: Option<Value>,
    /// Frozen at resolution; runtime consumers copy this receipt instead of
    /// reconstructing identity from profiles that may later change.
    pub identity_receipt: crate::DispatchIdentityReceipt,
}

pub fn resolve_dispatch_profile(raw: &str) -> Option<&'static DispatchProfileDef> {
    let norm = canonical_profile_name(raw)?;
    DISPATCH_PROFILES
        .iter()
        .find(|profile| profile.name == norm)
}

pub fn dispatch_profile_alias(raw: &str) -> Option<&'static DispatchProfileAlias> {
    let norm = raw.trim().to_ascii_lowercase();
    DISPATCH_PROFILE_ALIASES
        .iter()
        .find(|alias| alias.alias == norm)
}

fn canonical_profile_name(raw: &str) -> Option<String> {
    let norm = raw.trim().to_ascii_lowercase();
    if norm.is_empty() {
        return None;
    }
    Some(
        dispatch_profile_alias(&norm)
            .map(|alias| alias.replacement)
            .unwrap_or(norm.as_str())
            .to_string(),
    )
}

pub fn profile_deprecated_aliases(profile: &DispatchProfileDef) -> Vec<&'static str> {
    DISPATCH_PROFILE_ALIASES
        .iter()
        .filter(|alias| alias.replacement == profile.name)
        .map(|alias| alias.alias)
        .collect()
}

pub fn profile_resolved_model(profile: &DispatchProfileDef) -> Option<String> {
    profile
        .model_alias
        .and_then(crate::resolve_dispatch_model)
        .or_else(|| profile.model.map(str::to_string))
}

/// One canonical lineage encoding everywhere (`provider/family`, or the bare
/// backend when no model is declared). An alias is a routing name, not a
/// lineage: it must resolve to its concrete model first, or the receipt's
/// planned lineage could never match a carrier-acknowledged lineage computed
/// from the real model string.
fn profile_model_lineage_id(profile: &DispatchProfileDef) -> String {
    crate::model_lineage_id(profile_resolved_model(profile).as_deref(), profile.backend)
}

pub fn profile_host_adapter(profile: &DispatchProfileDef) -> Option<&'static str> {
    match profile.backend {
        "opencode" => Some("opencode"),
        // Compatibility path: older custom profiles with a model but no command
        // are still materialized as OpenCode CLI/serve commands by routing.
        "custom" if profile.model.is_some() || profile.model_alias.is_some() => Some("opencode"),
        _ => None,
    }
}

pub fn profile_uses_opencode_adapter(profile: &DispatchProfileDef) -> bool {
    profile_host_adapter(profile) == Some("opencode")
}

pub fn profile_matches_agent(profile: &DispatchProfileDef, agent: &str) -> bool {
    profile.backend == agent
        || matches!(
            (profile_host_adapter(profile), agent),
            (Some("opencode"), "custom" | "opencode")
        )
}

/// Recommendation is a planned route, not carrier acknowledgement. It exposes
/// the same receipt shape dispatch will freeze, with observed identity explicit
/// as unconfirmed.
pub fn recommendation_identity_receipt(
    profile: &DispatchProfileDef,
) -> crate::DispatchIdentityReceipt {
    let model = profile_resolved_model(profile);
    let (concrete_model_release, provider_model, provider_model_version) =
        crate::provider_model_parts(model.as_deref());
    let harness = profile_host_adapter(profile)
        .unwrap_or(profile.backend)
        .to_string();
    crate::DispatchIdentityReceipt::planned(
        crate::DispatchIdentityRequest {
            profile: Some(profile.name.to_string()),
            model: model.clone(),
            agent: Some(profile.backend.to_string()),
            harness: Some(harness.clone()),
        },
        crate::DispatchIdentityEffective {
            profile: Some(profile.name.to_string()),
            model: model.clone(),
            backend: profile.backend.to_string(),
            harness,
            model_lineage_id: profile_model_lineage_id(profile),
            concrete_model_release,
            provider_model,
            provider_model_version,
            role: profile.role.to_string(),
            seat: crate::UNKNOWN_IDENTITY.to_string(),
            transport: "planned".to_string(),
            adapter_version: crate::UNKNOWN_IDENTITY.to_string(),
            carrier_version: crate::UNKNOWN_IDENTITY.to_string(),
        },
        "recommendation resolved from static dispatch profile".to_string(),
        profile.allow_cross_lineage_override,
    )
}

pub fn resolve_and_apply_dispatch_profile<F, S, E, C>(
    params: &mut TachiDispatchParams,
    mut profile_required_skills: S,
    mut profile_evidence_required: E,
    profile_mbit_card: C,
    mut harness_attach_ready: F,
) -> Result<ResolvedDispatchProfile, String>
where
    F: FnMut(&str) -> bool,
    S: FnMut(&DispatchProfileDef) -> Result<Vec<String>, String>,
    E: FnMut(&DispatchProfileDef) -> Result<Vec<String>, String>,
    C: FnMut(&DispatchProfileDef) -> Result<Value, String>,
{
    let mut route_explanation = Vec::new();
    let identity_requested = crate::DispatchIdentityRequest {
        profile: params.profile.clone(),
        model: params.model.clone(),
        agent: params.agent.clone(),
        harness: params.harness_transport.clone(),
    };
    let requested_agent = params.agent.clone().filter(|s| !s.trim().is_empty());
    let requested_profile = params.profile.clone().filter(|s| !s.trim().is_empty());
    let alias = requested_profile
        .as_deref()
        .and_then(dispatch_profile_alias);
    let profile = match requested_profile.as_deref() {
        Some(raw) => Some(resolve_dispatch_profile(raw).ok_or_else(|| {
            format!(
                "Unknown dispatch profile '{}'. Supported: {}",
                raw.trim(),
                supported_dispatch_profile_names()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?),
        None => None,
    };

    if let Some(alias) = alias {
        params.profile = Some(alias.replacement.to_string());
        route_explanation.push(format!(
            "deprecated dispatch profile alias '{}' resolved to '{}'; {} ({})",
            alias.alias, alias.replacement, alias.reason, alias.release_window
        ));
    }

    if let Some(profile) = profile {
        if params.profile.as_deref() != Some(profile.name) {
            params.profile = Some(profile.name.to_string());
        }
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
        if let Some(requested_model) = params.model.as_deref() {
            let profile_model = profile_resolved_model(profile);
            let requested_lineage = crate::model_lineage_id(Some(requested_model), profile.backend);
            let profile_lineage =
                crate::model_lineage_id(profile_model.as_deref(), profile.backend);
            if !crate::lineages_compatible(&requested_lineage, &profile_lineage) {
                return Err(format!(
                    "model override '{}' crosses profile '{}' lineage '{}' -> '{}' without explicit profile authorization",
                    requested_model, profile.name, profile_lineage, requested_lineage
                ));
            }
            route_explanation.push(format!(
                "explicit model '{}' overrides profile model within lineage '{}'",
                requested_model, profile_lineage
            ));
        } else {
            params.model = profile_resolved_model(profile);
        }
        if profile_uses_opencode_adapter(profile) && params.command.is_empty() {
            apply_opencode_profile_command(
                params,
                profile,
                &mut route_explanation,
                &mut harness_attach_ready,
            )?;
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
            params.skills = profile_required_skills(profile)?;
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
    let agent_norm = crate::normalize_dispatch_agent_name(&agent).unwrap_or(agent);
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
    let evidence_required = match profile {
        Some(profile) => profile_evidence_required(profile)?,
        None => Vec::new(),
    };
    let fallback_chain = crate::fallback_chain(&agent_norm)
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut credential_profiles = params
        .credential_profiles
        .iter()
        .map(|profile| profile.trim().to_string())
        .filter(|profile| !profile.is_empty())
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut credential_profiles);
    let selected_profile = profile.map(|p| p.name.to_string());
    let planned_model = params.model.clone();
    let (concrete_model_release, provider_model, provider_model_version) =
        crate::provider_model_parts(planned_model.as_deref());
    let planned_backend = agent_norm.clone();
    let planned_harness = profile
        .and_then(profile_host_adapter)
        .unwrap_or(planned_backend.as_str())
        .to_string();
    let identity_receipt = crate::DispatchIdentityReceipt::planned(
        identity_requested,
        crate::DispatchIdentityEffective {
            profile: selected_profile.clone(),
            model: planned_model.clone(),
            model_lineage_id: profile.map(profile_model_lineage_id).unwrap_or_else(|| {
                crate::model_lineage_id(planned_model.as_deref(), &planned_backend)
            }),
            concrete_model_release,
            provider_model,
            provider_model_version,
            backend: planned_backend,
            harness: planned_harness,
            role: profile.map(|p| p.role).unwrap_or("unknown").to_string(),
            seat: crate::UNKNOWN_IDENTITY.to_string(),
            transport: params
                .harness_transport
                .clone()
                .unwrap_or_else(|| "cli".to_string()),
            adapter_version: crate::UNKNOWN_IDENTITY.to_string(),
            carrier_version: crate::UNKNOWN_IDENTITY.to_string(),
        },
        route_explanation.join("; "),
        profile.is_some_and(|p| p.allow_cross_lineage_override),
    );

    Ok(ResolvedDispatchProfile {
        selected_profile,
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
        mbit_card: profile.map(profile_mbit_card).transpose()?,
        identity_receipt,
    })
}

fn apply_opencode_profile_command<F>(
    params: &mut TachiDispatchParams,
    profile: &DispatchProfileDef,
    route_explanation: &mut Vec<String>,
    harness_attach_ready: &mut F,
) -> Result<(), String>
where
    F: FnMut(&str) -> bool,
{
    let model = params
        .model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .or_else(|| profile_resolved_model(profile))
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
        if harness_attach_ready(&server_url) {
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
                model.clone(),
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
                model.clone(),
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
            model.clone(),
        ];
        route_explanation.push(format!(
            "profile selected typed OpenCode CLI transport for model '{}'",
            model
        ));
    }
    Ok(())
}

fn supported_dispatch_profile_names() -> Vec<&'static str> {
    let mut names = DISPATCH_PROFILES
        .iter()
        .map(|profile| profile.name)
        .collect::<Vec<_>>();
    names.extend(DISPATCH_PROFILE_ALIASES.iter().map(|alias| alias.alias));
    names.sort_unstable();
    names
}

pub fn profile_required_skill_ids(profile: &DispatchProfileDef) -> Vec<String> {
    let mut skills = profile
        .common_skills
        .iter()
        .chain(profile.signature_skills.iter())
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut skills);
    skills
}

pub fn profile_required_skill_ids_with_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let mut skills = profile
        .common_skills
        .iter()
        .chain(profile.signature_skills.iter())
        .map(|skill| skill.to_string())
        .collect::<Vec<_>>();
    skills.extend(profile_projected_signature_skills_from_overlay(
        profile, overlay,
    ));
    dedupe_preserve_order(&mut skills);
    skills
}

pub fn profile_skill_loadout_json(profile: &DispatchProfileDef) -> Value {
    json!({
        "common_skills": profile.common_skills,
        "signature_skills": profile.signature_skills,
        "projected_signature_skills": [],
        "passive_traits": profile.passive_traits,
        "projected_passive_traits": [],
        "forbidden_skills": profile.forbidden_skills,
        "projection": {
            "status": "baseline",
        },
    })
}

pub fn profile_skill_loadout_json_with_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Value {
    let projected_signature_skills =
        profile_projected_signature_skills_from_overlay(profile, overlay);
    let projected_passive_traits = profile_projected_passive_traits_from_overlay(profile, overlay);
    let mut signature_skills = profile
        .signature_skills
        .iter()
        .map(|skill| skill.to_string())
        .collect::<Vec<_>>();
    signature_skills.extend(projected_signature_skills.iter().cloned());
    dedupe_preserve_order(&mut signature_skills);
    let mut passive_traits = profile
        .passive_traits
        .iter()
        .map(|trait_id| trait_id.to_string())
        .collect::<Vec<_>>();
    passive_traits.extend(projected_passive_traits.iter().cloned());
    dedupe_preserve_order(&mut passive_traits);
    let source_proposal_ids = overlay_source_proposal_ids(overlay);
    json!({
        "common_skills": profile.common_skills,
        "signature_skills": signature_skills,
        "projected_signature_skills": projected_signature_skills,
        "passive_traits": passive_traits,
        "projected_passive_traits": projected_passive_traits,
        "forbidden_skills": profile.forbidden_skills,
        "projection": {
            "status": if overlay.is_some() { "applied_overlay" } else { "baseline" },
            "namespace": PROFILE_CARD_OVERLAY_NS,
            "key": profile.name,
            "source_proposal_ids": source_proposal_ids,
        },
    })
}

pub fn profile_evidence_required(profile: &DispatchProfileDef) -> Vec<String> {
    let mut evidence = profile
        .evidence_required
        .iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut evidence);
    evidence
}

pub fn profile_evidence_required_with_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let mut evidence = profile_evidence_required(profile);
    evidence.extend(profile_projected_evidence_required_from_overlay(
        profile, overlay,
    ));
    dedupe_preserve_order(&mut evidence);
    evidence
}

pub fn profile_weak_against(profile: &DispatchProfileDef) -> Vec<String> {
    let mut weak = profile
        .weak_against
        .iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut weak);
    weak
}

pub fn profile_weak_against_with_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let mut weak = profile_weak_against(profile);
    weak.extend(profile_projected_weak_against_from_overlay(
        profile, overlay,
    ));
    dedupe_preserve_order(&mut weak);
    weak
}

pub fn profile_evidence_contract_json(profile: &DispatchProfileDef) -> Value {
    json!({
        "required": profile_evidence_required(profile),
        "projected_required": [],
        "projection": {
            "status": "baseline",
        },
    })
}

pub fn profile_evidence_contract_json_with_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Value {
    let projected_required = profile_projected_evidence_required_from_overlay(profile, overlay);
    let mut required = profile_evidence_required(profile);
    required.extend(projected_required.iter().cloned());
    dedupe_preserve_order(&mut required);
    let source_proposal_ids = overlay_source_proposal_ids(overlay);
    json!({
        "required": required,
        "projected_required": projected_required,
        "projection": {
            "status": if overlay.is_some() { "applied_overlay" } else { "baseline" },
            "namespace": PROFILE_CARD_OVERLAY_NS,
            "key": profile.name,
            "source_proposal_ids": source_proposal_ids,
        },
    })
}

pub fn profile_json(profile: &DispatchProfileDef) -> Value {
    profile_json_with_loadout_and_evidence_contract(
        profile,
        profile_skill_loadout_json(profile),
        profile_evidence_contract_json(profile),
        profile_weak_against(profile),
        Vec::new(),
        Vec::new(),
    )
}

pub fn profile_json_with_overlay(profile: &DispatchProfileDef, overlay: Option<&Value>) -> Value {
    profile_json_with_loadout_and_evidence_contract(
        profile,
        profile_skill_loadout_json_with_overlay(profile, overlay),
        profile_evidence_contract_json_with_overlay(profile, overlay),
        profile_weak_against_with_overlay(profile, overlay),
        profile_projected_weak_against_from_overlay(profile, overlay),
        profile_demotion_targets_from_overlay(profile, overlay),
    )
}

pub fn profile_json_with_loadout_and_evidence_contract(
    profile: &DispatchProfileDef,
    skill_loadout: Value,
    evidence_contract: Value,
    weak_against: Vec<String>,
    projected_weak_against: Vec<String>,
    demotion_targets: Vec<String>,
) -> Value {
    let stats = profile_mbit_stats(profile);
    let authority = profile_card_authority_json(profile);
    let guidance = profile_card_guidance_json(&skill_loadout);
    let moves = profile_card_moves_json(&skill_loadout);
    let personality = profile_card_personality_json(&stats);
    let archetype = profile_card_archetype(profile);
    let model_card = profile
        .model_alias
        .and_then(crate::dispatch_model_card)
        .map(|card| {
            json!({
                "alias": card.alias,
                "vendor": card.vendor,
                "role": card.role,
                "default_model": card.default_model,
                "env_override": card.env_override,
            })
        });
    let deprecated_aliases = profile_deprecated_aliases(profile);
    let deprecation_aliases = DISPATCH_PROFILE_ALIASES
        .iter()
        .filter(|alias| alias.replacement == profile.name)
        .map(|alias| {
            json!({
                "alias": alias.alias,
                "replacement": alias.replacement,
                "reason": alias.reason,
                "release_window": alias.release_window,
            })
        })
        .collect::<Vec<_>>();
    let card_projection = json!({
        "status": if projected_weak_against.is_empty() && demotion_targets.is_empty() {
            "baseline"
        } else {
            "applied_overlay"
        },
        "namespace": PROFILE_CARD_OVERLAY_NS,
        "key": profile.name,
    });
    json!({
        "name": profile.name,
        "display_name": profile.display_name,
        "backend": profile.backend,
        "host_adapter": profile_host_adapter(profile),
        "role": profile.role,
        "stage": profile.stage,
        "card_archetype": archetype,
        "model": profile_resolved_model(profile),
        "model_alias": profile.model_alias,
        "model_card": model_card,
        "deprecated_aliases": deprecated_aliases,
        "deprecation": {
            "aliases": deprecation_aliases,
        },
        "tool_profile": profile.tool_profile,
        "mcp_access": {
            "inject_tachi_mcp": profile.inject_tachi_mcp,
            "inject_hub_mcps": profile.inject_hub_mcps,
            "allowed_facades": profile.allowed_facades,
            "allowed_mcp_servers": profile.allowed_mcp_servers,
            "github_read": profile.github_read,
            "write_actions": profile.write_actions,
        },
        "credential_profiles": profile.credential_profiles,
        "skill_loadout": skill_loadout,
        "evidence_contract": evidence_contract,
        "weak_against": weak_against,
        "mbit_card": {
            "display_name": profile.display_name,
            "archetype": archetype,
            "type": [profile.role],
            "stats": stats,
            "authority": authority,
            "guidance": guidance,
            "moves": moves,
            "personality": personality,
            "strong_against": profile.strong_against,
            "weak_against": weak_against,
            "projected_weak_against": projected_weak_against,
            "demotion_targets": demotion_targets,
            "auto_capability_bundle": profile.auto_capability_bundle,
            "skill_loadout": skill_loadout,
            "evidence_contract": evidence_contract,
            "evolution": {
                "projection": card_projection,
            },
        }
    })
}

fn overlay_source_proposal_ids(overlay: Option<&Value>) -> Vec<Value> {
    overlay
        .and_then(|overlay| overlay.get("source_proposal_ids"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub fn profile_projected_signature_skills_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let forbidden = profile.forbidden_skills.iter().collect::<HashSet<_>>();
    let mut skills = overlay
        .get("add_signature_skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|skill| !skill.is_empty())
        .filter(|skill| !forbidden.contains(skill))
        .map(str::to_string)
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut skills);
    skills
}

pub fn profile_projected_passive_traits_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let baseline = profile.passive_traits.iter().collect::<HashSet<_>>();
    let mut traits = overlay
        .get("add_passive_traits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|trait_id| !trait_id.is_empty())
        .filter(|trait_id| !baseline.contains(trait_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut traits);
    traits
}

pub fn profile_projected_evidence_required_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let baseline = profile.evidence_required.iter().collect::<HashSet<_>>();
    let mut evidence = overlay
        .get("add_evidence_required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|evidence_id| !evidence_id.is_empty())
        .filter(|evidence_id| !baseline.contains(evidence_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut evidence);
    evidence
}

pub fn profile_projected_weak_against_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let baseline = profile.weak_against.iter().collect::<HashSet<_>>();
    let mut weak = overlay
        .get("add_weak_against")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|weakness_id| !weakness_id.is_empty())
        .filter(|weakness_id| !baseline.contains(weakness_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut weak);
    weak
}

pub fn profile_demotion_targets_from_overlay(
    profile: &DispatchProfileDef,
    overlay: Option<&Value>,
) -> Vec<String> {
    let Some(overlay) = overlay else {
        return Vec::new();
    };
    let known_skills = profile_required_skill_ids(profile)
        .into_iter()
        .collect::<HashSet<_>>();
    let mut targets = overlay
        .get("demotion_targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|skill_id| !skill_id.is_empty())
        .filter(|skill_id| known_skills.contains(*skill_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut targets);
    targets
}

pub fn profile_card_archetype(profile: &DispatchProfileDef) -> &'static str {
    let stage = profile.stage.unwrap_or_default();
    if profile.role == "explore" || stage == "explore" || stage == "probe" {
        "poke"
    } else if profile.role == "executor" || stage == "execute" || stage == "hotfix" {
        "scv"
    } else {
        "raven"
    }
}

pub fn profile_mbit_stats(profile: &DispatchProfileDef) -> Value {
    let (precision, speed, cost, creativity, risk_control) = match profile.name {
        "claude_plan" => (86, 58, 65, 82, 88),
        "glm_impl" => (78, 76, 52, 70, 72),
        "opencode_builder" => (74, 82, 48, 68, 70),
        "codex_55_review" => (95, 55, 72, 60, 95),
        "codex_53_fast" => (72, 92, 35, 52, 58),
        "kimi_arch" => (88, 64, 58, 86, 84),
        "deepseek_explore" => (76, 88, 30, 72, 62),
        "kimi_ux" => (84, 70, 58, 88, 78),
        _ => (70, 70, 70, 70, 70),
    };
    json!({
        "precision": precision,
        "speed": speed,
        "cost": cost,
        "creativity": creativity,
        "risk_control": risk_control,
    })
}

pub fn profile_card_authority_json(profile: &DispatchProfileDef) -> Value {
    json!({
        "write_code": profile.write_actions,
        "merge": false,
        "github_read": profile.github_read,
        "github_write": false,
        "can_dispatch_followup": false,
        "credential_profiles": profile.credential_profiles,
        "tool_profile": profile.tool_profile,
    })
}

pub fn profile_card_guidance_json(skill_loadout: &Value) -> Value {
    json!({
        "superpowers": profile_card_skills_by_prefix(skill_loadout, "skill:superpowers-"),
    })
}

pub fn profile_card_moves_json(skill_loadout: &Value) -> Value {
    let skills = profile_card_skill_ids_from_loadout(skill_loadout);
    let mut tachi_native = skills
        .iter()
        .filter(|skill| {
            skill.starts_with("skill:")
                && !skill.starts_with("skill:superpowers-")
                && !skill.starts_with("skill:waza-")
        })
        .cloned()
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut tachi_native);
    json!({
        "waza": profile_card_skills_by_prefix(skill_loadout, "skill:waza-"),
        "external": [],
        "tachi_native": tachi_native,
    })
}

pub fn profile_card_personality_json(stats: &Value) -> Value {
    json!({
        "curiosity": stats.get("creativity").and_then(Value::as_i64).unwrap_or(70),
        "caution": stats.get("risk_control").and_then(Value::as_i64).unwrap_or(70),
        "speed": stats.get("speed").and_then(Value::as_i64).unwrap_or(70),
        "risk_control": stats.get("risk_control").and_then(Value::as_i64).unwrap_or(70),
    })
}

pub fn profile_card_skills_by_prefix(skill_loadout: &Value, prefix: &str) -> Vec<String> {
    let mut skills = profile_card_skill_ids_from_loadout(skill_loadout)
        .into_iter()
        .filter(|skill| skill.starts_with(prefix))
        .collect::<Vec<_>>();
    dedupe_preserve_order(&mut skills);
    skills
}

pub fn profile_card_skill_ids_from_loadout(skill_loadout: &Value) -> Vec<String> {
    let mut skills = Vec::new();
    for key in [
        "common_skills",
        "signature_skills",
        "projected_signature_skills",
    ] {
        let Some(items) = skill_loadout.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            if let Some(skill) = item.as_str() {
                skills.push(skill.to_string());
            }
        }
    }
    dedupe_preserve_order(&mut skills);
    skills
}

pub fn dedupe_preserve_order(items: &mut Vec<String>) {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::{Mutex, OnceLock};

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn remove(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, original }
        }

        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = self.original.as_ref() {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn params(profile: &str) -> TachiDispatchParams {
        TachiDispatchParams {
            agent: None,
            profile: Some(profile.to_string()),
            task: "dispatch test".to_string(),
            execution_level: None,
            cwd: None,
            env_id: None,
            unmanaged_cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: None,
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        }
    }

    fn resolve_for_identity_test(params: &mut TachiDispatchParams) -> ResolvedDispatchProfile {
        resolve_and_apply_dispatch_profile(
            params,
            |profile| Ok(profile_required_skill_ids(profile)),
            |profile| Ok(profile_evidence_required(profile)),
            |profile| Ok(profile_json(profile)["mbit_card"].clone()),
            |_| false,
        )
        .expect("route resolves")
    }

    fn receipt_key_paths(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
        let Some(object) = value.as_object() else {
            return;
        };
        for (key, value) in object {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            out.insert(path.clone());
            receipt_key_paths(value, &path, out);
        }
    }

    #[test]
    fn overlay_projection_preserves_profile_card_payload_shape() {
        let profile = resolve_dispatch_profile("opencode_builder").expect("profile");
        let overlay = json!({
            "add_signature_skills": ["skill:custom-fast-fix", "skill:custom-fast-fix"],
            "add_passive_traits": ["evidence_backed_change_control"],
            "add_evidence_required": ["regression_tests"],
            "add_weak_against": ["research_request"],
            "demotion_targets": [SUPERPOWER_EXECUTING_PLANS, "skill:not-in-profile"],
            "source_proposal_ids": ["proposal-a"],
        });

        let loadout = profile_skill_loadout_json_with_overlay(profile, Some(&overlay));
        assert_eq!(
            loadout["projected_signature_skills"],
            json!(["skill:custom-fast-fix"])
        );
        assert_eq!(
            loadout["projection"]["source_proposal_ids"],
            json!(["proposal-a"])
        );

        let evidence = profile_evidence_contract_json_with_overlay(profile, Some(&overlay));
        assert_eq!(evidence["projected_required"], json!(["regression_tests"]));
        assert_eq!(evidence["projection"]["status"], json!("applied_overlay"));

        let card = profile_json_with_overlay(profile, Some(&overlay));
        assert_eq!(
            card["weak_against"],
            json!([
                "ambiguous_architecture",
                "unbounded_refactor",
                "research_request"
            ])
        );
        assert_eq!(
            card["mbit_card"]["projected_weak_against"],
            json!(["research_request"])
        );
        assert_eq!(
            card["mbit_card"]["demotion_targets"],
            json!([SUPERPOWER_EXECUTING_PLANS])
        );
    }

    #[test]
    fn glm_profile_alias_resolves_to_current_model_card_profile() {
        let _lock = env_lock();
        let _env = EnvGuard::remove("TACHI_DISPATCH_GLM_CODING_MODEL");

        let profile = resolve_dispatch_profile("glm_impl").expect("glm profile");
        assert_eq!(profile.name, "glm_impl");
        assert_eq!(profile.role, "executor");

        let alias_profile = resolve_dispatch_profile("glm_51_impl").expect("compat alias");
        assert_eq!(alias_profile.name, "glm_impl");

        let mut params = params("glm_51_impl");
        let resolved = resolve_and_apply_dispatch_profile(
            &mut params,
            |profile| Ok(profile_required_skill_ids(profile)),
            |profile| Ok(profile_evidence_required(profile)),
            |profile| Ok(profile_json(profile)["mbit_card"].clone()),
            |_| false,
        )
        .expect("compat alias should resolve");

        assert_eq!(params.profile.as_deref(), Some("glm_impl"));
        assert_eq!(params.model.as_deref(), Some("zhipuai-coding-plan/glm-5.2"));
        assert_eq!(resolved.selected_profile.as_deref(), Some("glm_impl"));
        let receipt = &resolved.identity_receipt;
        assert_eq!(receipt.requested.profile.as_deref(), Some("glm_51_impl"));
        assert_eq!(receipt.planned.profile.as_deref(), Some("glm_impl"));
        assert_eq!(
            receipt.planned.model.as_deref(),
            Some("zhipuai-coding-plan/glm-5.2")
        );
        assert_eq!(receipt.planned.model_lineage_id, "zhipuai-coding-plan/glm");
        assert_eq!(
            receipt.observed.acknowledgement,
            crate::DispatchAcknowledgement::Unconfirmed
        );
        assert_eq!(receipt.observed.effective.backend, crate::UNKNOWN_IDENTITY);
        assert!(resolved.route_explanation.iter().any(|line| {
            line.contains("deprecated dispatch profile alias 'glm_51_impl'")
                && line.contains("glm_impl")
        }));
    }

    #[test]
    fn same_lineage_override_is_planned_but_not_relabelled_as_observed() {
        let _lock = env_lock();
        let _env = EnvGuard::remove("TACHI_DISPATCH_GLM_CODING_MODEL");
        let mut params = params("glm_impl");
        params.model = Some("zhipuai-coding-plan/glm-5.2@2026-07-13".to_string());
        let resolved = resolve_and_apply_dispatch_profile(
            &mut params,
            |profile| Ok(profile_required_skill_ids(profile)),
            |profile| Ok(profile_evidence_required(profile)),
            |profile| Ok(profile_json(profile)["mbit_card"].clone()),
            |_| false,
        )
        .expect("same-lineage override");
        assert_eq!(
            resolved.identity_receipt.requested.model.as_deref(),
            Some("zhipuai-coding-plan/glm-5.2@2026-07-13")
        );
        assert_eq!(
            resolved.identity_receipt.planned.model,
            resolved.identity_receipt.requested.model
        );
        assert_eq!(
            resolved.identity_receipt.observed.acknowledgement,
            crate::DispatchAcknowledgement::Unconfirmed
        );
    }

    #[test]
    fn cross_lineage_model_override_fails_closed() {
        let mut params = params("glm_impl");
        params.model = Some("openai/gpt-5.6".to_string());
        let error = resolve_and_apply_dispatch_profile(
            &mut params,
            |profile| Ok(profile_required_skill_ids(profile)),
            |profile| Ok(profile_evidence_required(profile)),
            |profile| Ok(profile_json(profile)["mbit_card"].clone()),
            |_| false,
        )
        .expect_err("cross-lineage override must fail closed");
        assert!(error.contains("without explicit profile authorization"));
    }

    #[test]
    fn receipt_rejects_unauthorized_substitution_and_survives_replay() {
        let profile = resolve_dispatch_profile("glm_impl").expect("profile");
        let mut receipt = recommendation_identity_receipt(profile);
        let mut substitute = receipt.planned.clone();
        substitute.model_lineage_id = "openai".to_string();
        substitute.model = Some("openai/gpt-5.6".to_string());
        assert!(receipt
            .acknowledge(
                substitute,
                crate::DispatchAcknowledgement::Substituted,
                "carrier changed model".to_string(),
            )
            .is_err());

        let replay: crate::DispatchIdentityReceipt =
            serde_json::from_value(serde_json::to_value(&receipt).expect("serialize"))
                .expect("deserialize");
        assert_eq!(replay, receipt, "replay must preserve the frozen receipt");
    }

    #[test]
    fn matching_acknowledgement_is_not_a_mismatch_and_wins_attribution() {
        let _lock = env_lock();
        let _env = EnvGuard::remove("TACHI_DISPATCH_GLM_CODING_MODEL");
        let profile = resolve_dispatch_profile("glm_impl").expect("profile");
        let mut receipt = recommendation_identity_receipt(profile);
        assert_eq!(
            receipt.attribution().0,
            receipt.planned,
            "an unconfirmed receipt attributes to the planned identity"
        );

        let mut observed = receipt.planned.clone();
        // A real carrier fills in provenance the planner could not know.
        observed.seat = "worker-3".to_string();
        observed.transport = "acp".to_string();
        observed.adapter_version = "1.2.3".to_string();
        observed.carrier_version = "0.9.0".to_string();
        receipt
            .acknowledge(
                observed,
                crate::DispatchAcknowledgement::Acknowledged,
                "carrier acknowledged launch".to_string(),
            )
            .expect("matching acknowledgement");
        assert!(
            !receipt.observed.mismatch,
            "provenance-only differences must not flag an identity mismatch"
        );
        assert_eq!(receipt.attribution().0, receipt.observed.effective);
    }

    #[test]
    fn same_lineage_release_substitution_is_an_explicit_mismatch() {
        let _lock = env_lock();
        let _env = EnvGuard::remove("TACHI_DISPATCH_GLM_CODING_MODEL");
        let profile = resolve_dispatch_profile("glm_impl").expect("profile");
        let mut receipt = recommendation_identity_receipt(profile);
        let mut observed = receipt.planned.clone();
        observed.model = Some("zhipuai-coding-plan/glm-5.2@2026-07-13".to_string());
        let (release, provider_model, version) =
            crate::provider_model_parts(observed.model.as_deref());
        observed.concrete_model_release = release;
        observed.provider_model = provider_model;
        observed.provider_model_version = version;
        receipt
            .acknowledge(
                observed,
                crate::DispatchAcknowledgement::Substituted,
                "carrier pinned a release".to_string(),
            )
            .expect("same-lineage release substitution is allowed");
        assert!(
            receipt.observed.mismatch,
            "a release substitution must surface as an explicit mismatch"
        );
        assert_eq!(
            receipt.attribution().0.model.as_deref(),
            Some("zhipuai-coding-plan/glm-5.2@2026-07-13"),
            "attribution follows what executed, never the planned claim"
        );
    }

    #[test]
    fn profile_without_model_accepts_same_family_override() {
        let mut params = params("claude_plan");
        params.model = Some("anthropic/claude-sonnet-5".to_string());
        let resolved = resolve_and_apply_dispatch_profile(
            &mut params,
            |profile| Ok(profile_required_skill_ids(profile)),
            |profile| Ok(profile_evidence_required(profile)),
            |profile| Ok(profile_json(profile)["mbit_card"].clone()),
            |_| false,
        )
        .expect("same-family override on a model-less profile must be allowed");
        assert_eq!(
            resolved.identity_receipt.planned.model.as_deref(),
            Some("anthropic/claude-sonnet-5")
        );
    }

    #[test]
    fn profile_without_model_rejects_cross_family_override() {
        let mut params = params("claude_plan");
        params.model = Some("openai/gpt-5.6".to_string());
        let error = resolve_and_apply_dispatch_profile(
            &mut params,
            |profile| Ok(profile_required_skill_ids(profile)),
            |profile| Ok(profile_evidence_required(profile)),
            |profile| Ok(profile_json(profile)["mbit_card"].clone()),
            |_| false,
        )
        .expect_err("cross-family override must fail closed");
        assert!(error.contains("without explicit profile authorization"));
    }

    #[test]
    fn malformed_lineages_fail_closed() {
        assert!(crate::lineages_compatible("anthropic/claude", "claude"));
        assert!(crate::lineages_compatible("claude", "claude"));
        assert!(!crate::lineages_compatible("a/b/claude", "claude"));
        assert!(!crate::lineages_compatible("a/b/c", "a/b/c"));
        assert!(!crate::lineages_compatible("", ""));
        assert!(!crate::lineages_compatible("", "claude"));
        assert!(!crate::lineages_compatible("/claude", "claude"));
        assert!(!crate::lineages_compatible("openai/gpt", "claude"));
        assert!(!crate::lineages_compatible("anthropic/claude", ""));
    }

    #[test]
    fn provider_model_parts_separates_provider_release_and_version() {
        assert_eq!(
            crate::provider_model_parts(Some("zhipuai-coding-plan/glm-5.2@2026-07-13")),
            (
                "zhipuai-coding-plan/glm-5.2".to_string(),
                "glm-5.2".to_string(),
                "2026-07-13".to_string()
            )
        );
        assert_eq!(
            crate::provider_model_parts(Some("glm-5.2")),
            (
                "glm-5.2".to_string(),
                "glm-5.2".to_string(),
                crate::UNKNOWN_IDENTITY.to_string()
            )
        );
    }

    #[test]
    fn every_resolution_entry_route_emits_the_full_identity_receipt_shape() {
        let mut profile = params("glm_impl");
        let mut profile_and_agent = params("glm_impl");
        profile_and_agent.agent = Some("opencode".to_string());
        let mut direct = params("glm_impl");
        direct.profile = None;
        direct.agent = Some("codex".to_string());
        direct.model = Some("openai/gpt-5.6".to_string());
        let mut custom = params("glm_impl");
        custom.profile = None;
        custom.agent = Some("custom".to_string());
        custom.model = Some("example/custom-1".to_string());
        let mut host_adapter = params("opencode_builder");

        let mut expected = None;
        for receipt in [
            resolve_for_identity_test(&mut profile).identity_receipt,
            resolve_for_identity_test(&mut profile_and_agent).identity_receipt,
            resolve_for_identity_test(&mut direct).identity_receipt,
            resolve_for_identity_test(&mut custom).identity_receipt,
            resolve_for_identity_test(&mut host_adapter).identity_receipt,
        ] {
            let mut keys = BTreeSet::new();
            receipt_key_paths(
                &serde_json::to_value(receipt).expect("receipt serializes"),
                "",
                &mut keys,
            );
            if let Some(expected) = &expected {
                assert_eq!(
                    &keys, expected,
                    "every entry route must expose every receipt key"
                );
            } else {
                expected = Some(keys);
            }
        }
    }

    #[test]
    fn glm_model_registry_supports_env_override_and_profile_json_metadata() {
        let _lock = env_lock();
        let _env = EnvGuard::set("TACHI_DISPATCH_GLM_CODING_MODEL", "zhipuai/glm-5.2-custom");

        let profile = resolve_dispatch_profile("opencode_builder").expect("opencode profile");
        let card = profile_json(profile);

        assert_eq!(card["model"], json!("zhipuai/glm-5.2-custom"));
        assert_eq!(card["model_alias"], json!("glm_coding"));
        assert_eq!(card["model_card"]["vendor"], json!("glm"));
        assert_eq!(
            card["model_card"]["default_model"],
            json!("zhipuai-coding-plan/glm-5.2")
        );
        assert_eq!(card["deprecated_aliases"], json!([]));

        let glm_card = profile_json(resolve_dispatch_profile("glm_impl").unwrap());
        assert_eq!(glm_card["name"], json!("glm_impl"));
        assert_eq!(glm_card["model"], json!("zhipuai/glm-5.2-custom"));
        assert_eq!(glm_card["model_card"]["role"], json!("executor"));
        assert_eq!(glm_card["deprecated_aliases"], json!(["glm_51_impl"]));
        assert_eq!(
            glm_card["deprecation"]["aliases"][0]["replacement"],
            json!("glm_impl")
        );
    }

    #[test]
    fn opencode_profile_command_materializes_registry_model() {
        let _lock = env_lock();
        let _env = EnvGuard::set("TACHI_DISPATCH_GLM_CODING_MODEL", "zhipuai/glm-5.2[1m]");
        let mut params = params("opencode_builder");

        resolve_and_apply_dispatch_profile(
            &mut params,
            |profile| Ok(profile_required_skill_ids(profile)),
            |profile| Ok(profile_evidence_required(profile)),
            |profile| Ok(profile_json(profile)["mbit_card"].clone()),
            |_| false,
        )
        .expect("opencode builder should resolve");

        assert_eq!(params.model.as_deref(), Some("zhipuai/glm-5.2[1m]"));
        assert!(params
            .command
            .windows(2)
            .any(|pair| pair == ["--model", "zhipuai/glm-5.2[1m]"]));
    }
}
