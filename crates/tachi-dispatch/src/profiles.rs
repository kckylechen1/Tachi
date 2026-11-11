//! Dispatch profiles describe who to ask, which context/tools to expose, and
//! what evidence a delegated agent must return. They are intentionally separate
//! from `tachi_hub::ToolProfile`, which only gates MCP tool visibility.

use crate::native_skill_ids::{
    CODING_ARCHITECTURE_DECISION, CODING_TEST_STRATEGY, SUPERPOWER_EXECUTING_PLANS,
    SUPERPOWER_REQUESTING_CODE_REVIEW, SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT,
    SUPERPOWER_VERIFICATION_BEFORE_COMPLETION, SUPERPOWER_WRITING_PLANS, WAZA_CHECK, WAZA_LEARN,
    WAZA_READ, WAZA_TACHI, WAZA_THINK,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use tachi_params::{DispatchMcpAccessParams, StaffAssignmentRequest, TachiDispatchParams};

pub const DISPATCH_POLICY_PROPOSAL_NS: &str = "dispatch_route_policy_proposals";
pub const ROUTE_POLICY_RULE_NS: &str = "dispatch_route_policy_rules";
pub const PROFILE_CARD_OVERLAY_NS: &str = "dispatch_profile_card_overlays";
pub const MIN_EVOLUTION_SAMPLES: u32 = 10;
pub const MIN_ROUTE_POLICY_RULE_SAMPLES: u32 = 2;
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
    pub native_session: Option<NativeSessionPolicy>,
    /// Explicit profile authorization for a carrier to substitute a model
    /// from a different lineage (#1065). Off everywhere today; a receipt
    /// freezes this at resolution, so nothing downstream can widen it.
    pub allow_cross_lineage_override: bool,
    pub tool_profile: &'static str,
    pub inject_tachi_mcp: bool,
    pub inject_hub_mcps: bool,
    pub github_read: bool,
    pub write_actions: bool,
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

/// Fixed supervision policy. Model and backend come from the same profile row.
#[derive(Debug, Clone, Copy)]
pub struct NativeSessionPolicy {
    pub effort: Option<&'static str>,
    pub max_turns: u32,
    pub turn_timeout_secs: u64,
}

/// Requested configuration, never evidence that an adapter or account supports it.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct NativeSessionProfile {
    pub backend: &'static str,
    pub model: &'static str,
    pub effort: Option<&'static str>,
    pub supervised: bool,
    pub max_turns: u32,
    pub writable: bool,
    pub lifetime_timeout_secs: u64,
    pub turn_timeout_secs: u64,
    pub credentials_policy: &'static str,
}

pub fn native_session_profile(profile: &str) -> Option<NativeSessionProfile> {
    let profile = resolve_dispatch_profile(profile)?;
    let policy = profile.native_session?;
    Some(NativeSessionProfile {
        backend: profile.backend,
        model: profile.model?,
        effort: policy.effort,
        supervised: true,
        max_turns: policy.max_turns,
        writable: profile.write_actions,
        turn_timeout_secs: policy.turn_timeout_secs,
        lifetime_timeout_secs: policy.turn_timeout_secs * u64::from(policy.max_turns),
        credentials_policy: "provider_auth_snapshot",
    })
}

const fn native_profile(
    name: &'static str,
    display_name: &'static str,
    backend: &'static str,
    model: &'static str,
    effort: Option<&'static str>,
    writable: bool,
    max_turns: u32,
    turn_timeout_secs: u64,
) -> DispatchProfileDef {
    DispatchProfileDef {
        name,
        display_name,
        backend,
        role: if writable {
            "executor"
        } else {
            "senior_reviewer"
        },
        stage: Some(if writable { "execute" } else { "review" }),
        model: Some(model),
        model_alias: None,
        native_session: Some(NativeSessionPolicy {
            effort,
            max_turns,
            turn_timeout_secs,
        }),
        allow_cross_lineage_override: false,
        tool_profile: "delegate",
        inject_tachi_mcp: true,
        inject_hub_mcps: false,
        github_read: false,
        write_actions: writable,
        allowed_facades: &["tachi_memory", "tachi_event", "tachi_task", "tachi_a2a"],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: if writable {
            &[SUPERPOWER_EXECUTING_PLANS]
        } else {
            &[SUPERPOWER_REQUESTING_CODE_REVIEW]
        },
        signature_skills: &[SUPERPOWER_VERIFICATION_BEFORE_COMPLETION],
        passive_traits: &[
            "bounded_turns",
            "leader_owns_merge",
            "build_through_oz_or_declared_shared_target",
        ],
        forbidden_skills: &[
            "recursive_delegation",
            "silent_model_fallback",
            "self_managed_cargo_target_dir",
        ],
        evidence_required: if writable {
            &["summary", "tests_run", "files_changed"]
        } else {
            &["findings_by_severity", "file_refs", "verification_advice"]
        },
        strong_against: if writable {
            &["bounded_patch", "implementation"]
        } else {
            &["bounded_review", "schema_boundary"]
        },
        weak_against: &["unbounded_redesign"],
    }
}

/// Retired names never silently select a different model or authority.
pub fn retired_dispatch_profile(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "codex_53_fast" => Some("codex_sol_review"),
        "codex_55_review" => Some("codex_astra_review"),
        "opencode_builder" => Some("opencode_terra_write"),
        _ => None,
    }
}

fn profile_resolution_error(raw: &str) -> String {
    match retired_dispatch_profile(raw) {
        Some(replacement) => format!("Dispatch profile '{}' is retired; explicitly select '{}' and review its native Session model/authority contract", raw.trim(), replacement),
        None => format!("Unknown dispatch profile '{}'. Supported: {}", raw.trim(), supported_dispatch_profile_names().join(", ")),
    }
}

pub const DISPATCH_PROFILES: &[DispatchProfileDef] = &[
    DispatchProfileDef {
        name: "dsh_executor",
        display_name: "DeepSeek Harness Headless Executor",
        backend: "dsh",
        role: "executor",
        stage: Some("execute"),
        // The native headless profile selects its model. Do not turn a vendor
        // default into a carrier-acknowledged model identity.
        model: None,
        model_alias: None,
        native_session: None,
        allow_cross_lineage_override: false,
        tool_profile: "delegate",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: false,
        write_actions: true,
        allowed_facades: &[],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[],
        signature_skills: &[],
        passive_traits: &[
            "bounded_diff",
            "leader_owns_merge",
            "advisory_workspace_authority",
        ],
        forbidden_skills: &["claim_certified_sandbox", "self_managed_cargo_target_dir"],
        evidence_required: &["summary", "tests_run", "files_changed"],
        strong_against: &["bounded_execution"],
        weak_against: &[
            "enforced_read_only",
            "tool_allowlists",
            "runtime_mcp_injection",
        ],
    },
    DispatchProfileDef {
        name: "claude_plan",
        display_name: "Claude Plan",
        backend: "claude",
        role: "planner",
        stage: Some("plan"),
        model: None,
        model_alias: None,
        native_session: None,
        allow_cross_lineage_override: false,
        tool_profile: "delegate",
        inject_tachi_mcp: true,
        inject_hub_mcps: false,
        github_read: true,
        write_actions: false,
        allowed_facades: &["tachi_memory", "tachi_event", "tachi_wiki", "tachi_task"],
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
        native_session: None,
        allow_cross_lineage_override: false,
        tool_profile: "delegate",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: false,
        write_actions: true,
        allowed_facades: &["tachi_memory", "tachi_event", "tachi_task"],
        allowed_mcp_servers: &[],
        credential_profiles: &[],
        common_skills: &[SUPERPOWER_EXECUTING_PLANS],
        signature_skills: &[WAZA_TACHI, CODING_TEST_STRATEGY],
        // tachi#1184: the 56G private-target-dir incident (2026-07-11..16)
        // was five parallel write lanes each hitting the shared build's lock
        // and quietly compensating by minting their own `CARGO_TARGET_DIR` —
        // a contract gap, not agent misbehavior. These two traits close it
        // in the one place every dispatched write lane actually reads: the
        // rendered `## Dispatch profile` packet section
        // (`dispatch_ops/prompt/overlays.rs::render_dispatch_profile_overlay`).
        passive_traits: &[
            "bounded_diff",
            "tests_required",
            "leader_owns_merge",
            "build_through_oz_or_declared_shared_target",
        ],
        forbidden_skills: &[
            "unbounded_redesign",
            "silent_test_workaround",
            "self_managed_cargo_target_dir",
        ],
        evidence_required: &["diff", "tests_run", "files_changed"],
        strong_against: &["bounded_patch", "implementation"],
        weak_against: &["ambiguous_architecture", "unbounded_refactor"],
    },
    native_profile(
        "codex_sol_review",
        "Codex Sol Reviewer",
        "codex",
        "gpt-6.1-sol",
        Some("high"),
        false,
        4,
        300,
    ),
    native_profile(
        "codex_astra_review",
        "Codex Astra Reviewer",
        "codex",
        "gpt-6-astra",
        Some("xhigh"),
        false,
        4,
        600,
    ),
    native_profile(
        "codex_terra_write",
        "Codex Terra Implementer",
        "codex",
        "gpt-6.1-terra",
        Some("high"),
        true,
        8,
        600,
    ),
    native_profile(
        "opencode_sol_review",
        "OpenCode Sol Reviewer",
        "opencode",
        "openai/gpt-6.1-sol",
        None,
        false,
        4,
        300,
    ),
    native_profile(
        "opencode_astra_review",
        "OpenCode Astra Reviewer",
        "opencode",
        "openai/gpt-6-astra",
        None,
        false,
        4,
        600,
    ),
    native_profile(
        "opencode_terra_write",
        "OpenCode Terra Implementer",
        "opencode",
        "openai/gpt-6.1-terra",
        None,
        true,
        8,
        600,
    ),
    DispatchProfileDef {
        name: "kimi_arch",
        display_name: "Kimi Architecture Critic",
        backend: "kimi",
        role: "architect",
        stage: Some("plan_review"),
        model: None,
        model_alias: None,
        native_session: None,
        allow_cross_lineage_override: false,
        tool_profile: "observe",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: true,
        write_actions: false,
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
        native_session: None,
        allow_cross_lineage_override: false,
        tool_profile: "observe",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: false,
        write_actions: false,
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
        native_session: None,
        allow_cross_lineage_override: false,
        tool_profile: "observe",
        inject_tachi_mcp: false,
        inject_hub_mcps: false,
        github_read: true,
        write_actions: false,
        allowed_facades: &["tachi_memory", "tachi_event", "tachi_task", "tachi_wiki"],
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
    /// Model selected by the profile resolver. This is resolution-owned rather
    /// than a Staff caller input.
    #[serde(skip_serializing)]
    pub selected_model: Option<String>,
    /// Profile-owned launch defaults. Staff never supplies these fields.
    #[serde(skip_serializing)]
    pub launch_command: Vec<String>,
    #[serde(skip_serializing)]
    pub harness_transport: Option<String>,
    #[serde(skip_serializing)]
    pub harness_server_url: Option<String>,
    pub role: Option<String>,
    pub tool_profile: Option<String>,
    pub mcp_access: DispatchMcpAccessParams,
    #[serde(skip_serializing)]
    pub required_skills: Vec<String>,
    pub evidence_required: Vec<String>,
    pub fallback_chain: Vec<String>,
    pub credential_profiles: Vec<String>,
    pub route_explanation: Vec<String>,
    pub host_adapter: Option<String>,
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
        "dsh" => Some("dsh"),
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

pub fn resolve_and_apply_dispatch_profile<F, S, E>(
    params: &mut TachiDispatchParams,
    mut profile_required_skills: S,
    mut profile_evidence_required: E,
    mut harness_attach_ready: F,
) -> Result<ResolvedDispatchProfile, String>
where
    F: FnMut(&str) -> bool,
    S: FnMut(&DispatchProfileDef) -> Result<Vec<String>, String>,
    E: FnMut(&DispatchProfileDef) -> Result<Vec<String>, String>,
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
        Some(raw) => {
            Some(resolve_dispatch_profile(raw).ok_or_else(|| profile_resolution_error(raw))?)
        }
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
        if profile.backend == "dsh"
            && requested_agent.as_deref().is_some_and(|agent| {
                crate::normalize_dispatch_agent_name(agent).as_deref() != Some("dsh")
            })
        {
            return Err("dsh_executor does not support a worker override".to_string());
        }
        if let Some(native) = native_session_profile(profile.name) {
            if params
                .stage
                .as_deref()
                .is_some_and(|stage| Some(stage) != profile.stage)
            {
                return Err(format!(
                    "native profile '{}' fixes its stage; remove the stage override",
                    profile.name
                ));
            }
            if requested_agent.as_deref().is_some_and(|agent| {
                crate::normalize_dispatch_agent_name(agent).as_deref() != Some(native.backend)
            }) || params
                .model
                .as_deref()
                .is_some_and(|model| model != native.model)
                || !params.command.is_empty()
                || params.harness_server_url.is_some()
                || params
                    .harness_transport
                    .as_deref()
                    .is_some_and(|transport| {
                        crate::transport_kind(transport) != crate::TransportKind::AcpNative
                    })
                || !params.credential_profiles.is_empty()
                || params
                    .max_turns
                    .is_some_and(|turns| turns != native.max_turns)
            {
                return Err(format!("native profile '{}' fixes backend, model, adapter, credentials, transport and turn ceiling; remove launch overrides", profile.name));
            }
            params.agent = Some(native.backend.to_string());
            params.model = Some(native.model.to_string());
            params.harness_transport = Some("acp_native".to_string());
            params.max_turns = Some(native.max_turns);
            params.timeout_secs = native.lifetime_timeout_secs;
        }
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
        if profile.native_session.is_none()
            && profile_uses_opencode_adapter(profile)
            && params.command.is_empty()
        {
            apply_opencode_profile_command(
                params,
                profile,
                &mut route_explanation,
                &mut harness_attach_ready,
            )?;
        }
        if profile.backend == "dsh" {
            if params
                .harness_transport
                .as_deref()
                .is_some_and(|transport| transport != "dsh_headless")
                || params.harness_server_url.is_some()
            {
                return Err("dsh_executor supports only the dsh_headless transport".to_string());
            }
            params.harness_transport = Some("dsh_headless".to_string());
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
    let fallback_chain = if profile.is_some_and(|profile| profile.native_session.is_some()) {
        Vec::new()
    } else {
        crate::fallback_chain(&agent_norm)
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
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
        selected_model: params.model.clone(),
        launch_command: params.command.clone(),
        harness_transport: params.harness_transport.clone(),
        harness_server_url: params.harness_server_url.clone(),
        role: profile.map(|p| p.role.to_string()),
        tool_profile: params.tool_profile.clone(),
        mcp_access,
        required_skills: params.skills.clone(),
        evidence_required,
        fallback_chain,
        credential_profiles,
        route_explanation,
        host_adapter: profile.and_then(profile_host_adapter).map(str::to_string),
        identity_receipt,
    })
}

/// Resolve a model-facing Staff assignment without constructing the legacy
/// flat dispatch facade. The returned profile carries every profile-owned
/// launch default needed by the server-owned canonical launch kernel.
pub fn resolve_and_apply_staff_assignment_profile<F, S, E>(
    request: &mut StaffAssignmentRequest,
    mut profile_required_skills: S,
    mut profile_evidence_required: E,
    _harness_attach_ready: F,
) -> Result<ResolvedDispatchProfile, String>
where
    F: FnMut(&str) -> bool,
    S: FnMut(&DispatchProfileDef) -> Result<Vec<String>, String>,
    E: FnMut(&DispatchProfileDef) -> Result<Vec<String>, String>,
{
    let mut route_explanation = Vec::new();
    let requested_profile = request
        .profile
        .clone()
        .filter(|value| !value.trim().is_empty());
    let requested_worker = request
        .worker
        .clone()
        .filter(|value| !value.trim().is_empty());
    let alias = requested_profile
        .as_deref()
        .and_then(dispatch_profile_alias);
    let raw = requested_profile.as_deref().ok_or_else(|| "Staff start requires a native Session profile (for example codex_sol_review); worker alone does not grant launch authority".to_string())?;
    let admitted = resolve_dispatch_profile(raw).ok_or_else(|| profile_resolution_error(raw))?;
    if admitted.native_session.is_none() && admitted.backend != "dsh" {
        return Err(format!(
            "profile '{}' has no admitted native Staff Session; select a native Session profile",
            admitted.name
        ));
    }
    if requested_worker.as_deref().is_some_and(|worker| {
        crate::normalize_dispatch_agent_name(worker).as_deref() != Some(admitted.backend)
    }) {
        return Err(format!(
            "profile '{}' fixes backend '{}'; worker override is not permitted",
            admitted.name, admitted.backend
        ));
    }
    let profile = Some(admitted);
    let identity_requested = crate::DispatchIdentityRequest {
        profile: request.profile.clone(),
        model: None,
        agent: request.worker.clone(),
        harness: None,
    };

    if let Some(alias) = alias {
        request.profile = Some(alias.replacement.to_string());
        route_explanation.push(format!(
            "deprecated dispatch profile alias '{}' resolved to '{}'; {} ({})",
            alias.alias, alias.replacement, alias.reason, alias.release_window
        ));
    }

    let mut selected_model = None;
    let launch_command = Vec::new();
    let mut harness_transport = None;
    let harness_server_url = None;
    let (
        mcp_access,
        required_skills,
        evidence_required,
        credential_profiles,
        host_adapter,
        selected_profile,
    ) = if let Some(profile) = profile {
        if profile.backend == "dsh"
            && requested_worker.as_deref().is_some_and(|worker| {
                crate::normalize_dispatch_agent_name(worker).as_deref() != Some("dsh")
            })
        {
            return Err("dsh_executor does not support a worker override".to_string());
        }
        request.profile = Some(profile.name.to_string());
        route_explanation.push(format!(
            "selected DispatchProfile '{}' ({})",
            profile.name, profile.role
        ));
        if requested_worker.is_none() {
            request.worker = Some(profile.backend.to_string());
            route_explanation.push(format!("profile selected backend '{}'", profile.backend));
        } else if requested_worker.as_deref() != Some(profile.backend) {
            route_explanation.push(format!(
                "explicit agent '{}' overrides profile backend '{}'",
                requested_worker.as_deref().unwrap_or(""),
                profile.backend
            ));
        }
        if profile.native_session.is_some()
            && request
                .stage
                .as_deref()
                .is_some_and(|stage| Some(stage) != profile.stage)
        {
            return Err(format!(
                "native profile '{}' fixes its stage; remove the stage override",
                profile.name
            ));
        }
        if request.stage.is_none() {
            request.stage = profile.stage.map(str::to_string);
        }
        selected_model = profile_resolved_model(profile);
        if profile.backend == "dsh" {
            harness_transport = Some("dsh_headless".to_string());
            route_explanation.push("profile selected DeepSeek Harness headless; workspace authority is advisory and model/auth remain host-owned".to_string());
        }
        let access = DispatchMcpAccessParams {
            inject_tachi_mcp: Some(profile.inject_tachi_mcp),
            inject_hub_mcps: Some(profile.inject_hub_mcps),
            allowed_facades: profile
                .allowed_facades
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            allowed_mcp_servers: profile
                .allowed_mcp_servers
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            github_read: Some(profile.github_read),
            write_actions: Some(profile.write_actions),
            issue_refs: request.issue_ref.iter().cloned().collect(),
            pr_refs: request.pr_ref.iter().cloned().collect(),
            fallback: Some(if profile.github_read {
                "Use MCP/GitHub read tools when available; if unavailable, report issue_context_unavailable instead of guessing.".to_string()
            } else {
                "Use leader-provided issue packet; do not perform GitHub writes.".to_string()
            }),
        };
        let credentials = profile
            .credential_profiles
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        if profile.native_session.is_some() {
            harness_transport = Some("acp_native".to_string());
            route_explanation.push("profile fixes native ACP Session; model and effort require exact adapter acknowledgement before the first prompt".to_string());
        }
        if !credentials.is_empty() {
            route_explanation.push(format!(
                "profile requires credential profile(s): {}",
                credentials.join(", ")
            ));
        }
        (
            access,
            profile_required_skills(profile)?,
            profile_evidence_required(profile)?,
            credentials,
            profile_host_adapter(profile).map(str::to_string),
            Some(profile.name.to_string()),
        )
    } else {
        (
            DispatchMcpAccessParams {
                inject_tachi_mcp: None,
                inject_hub_mcps: None,
                allowed_facades: Vec::new(),
                allowed_mcp_servers: Vec::new(),
                github_read: Some(request.issue_ref.is_some() || request.pr_ref.is_some()),
                write_actions: Some(false),
                issue_refs: request.issue_ref.iter().cloned().collect(),
                pr_refs: request.pr_ref.iter().cloned().collect(),
                fallback: Some(
                    "Use leader-provided context if GitHub/MCP issue reads are unavailable."
                        .to_string(),
                ),
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
        )
    };
    let worker = request
        .worker
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "agent or profile is required for dispatch".to_string())?;
    let agent = crate::normalize_dispatch_agent_name(&worker).unwrap_or(worker);
    let fallback_chain = if admitted.native_session.is_some() {
        Vec::new()
    } else {
        crate::fallback_chain(&agent)
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    };
    let (concrete_model_release, provider_model, provider_model_version) =
        crate::provider_model_parts(selected_model.as_deref());
    let identity_receipt = crate::DispatchIdentityReceipt::planned(
        identity_requested,
        crate::DispatchIdentityEffective {
            profile: selected_profile.clone(),
            model: selected_model.clone(),
            model_lineage_id: profile
                .map(profile_model_lineage_id)
                .unwrap_or_else(|| crate::model_lineage_id(selected_model.as_deref(), &agent)),
            concrete_model_release,
            provider_model,
            provider_model_version,
            backend: agent.clone(),
            harness: host_adapter.clone().unwrap_or_else(|| agent.clone()),
            role: profile
                .map(|profile| profile.role)
                .unwrap_or("unknown")
                .to_string(),
            seat: crate::UNKNOWN_IDENTITY.to_string(),
            transport: harness_transport
                .clone()
                .unwrap_or_else(|| "cli".to_string()),
            adapter_version: crate::UNKNOWN_IDENTITY.to_string(),
            carrier_version: crate::UNKNOWN_IDENTITY.to_string(),
        },
        route_explanation.join("; "),
        profile.is_some_and(|profile| profile.allow_cross_lineage_override),
    );
    Ok(ResolvedDispatchProfile {
        selected_profile,
        agent,
        selected_model,
        launch_command,
        harness_transport,
        harness_server_url,
        role: profile.map(|profile| profile.role.to_string()),
        tool_profile: profile.map(|profile| profile.tool_profile.to_string()),
        mcp_access,
        required_skills,
        evidence_required,
        fallback_chain,
        credential_profiles,
        route_explanation,
        host_adapter,
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
                "--standalone".to_string(),
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
            "--standalone".to_string(),
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
    )
}

pub fn profile_json_with_overlay(profile: &DispatchProfileDef, overlay: Option<&Value>) -> Value {
    profile_json_with_loadout_and_evidence_contract(
        profile,
        // #1690 B1/C3: the loadout is the STATIC reviewed baseline only. The
        // legacy skill/passive/weakness overlay projections are inert history;
        // only the evidence contract remains enforceable at this boundary.
        profile_skill_loadout_json(profile),
        profile_evidence_contract_json_with_overlay(profile, overlay),
        profile_weak_against(profile),
    )
}

pub fn profile_json_with_loadout_and_evidence_contract(
    profile: &DispatchProfileDef,
    skill_loadout: Value,
    evidence_contract: Value,
    weak_against: Vec<String>,
) -> Value {
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
    json!({
        "name": profile.name,
        "display_name": profile.display_name,
        "backend": profile.backend,
        "host_adapter": profile_host_adapter(profile),
        "native_session": native_session_profile(profile.name),
        "transport": if profile.native_session.is_some() { Some("acp_native") } else { None },
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
    })
}

fn overlay_source_proposal_ids(overlay: Option<&Value>) -> Vec<Value> {
    overlay
        .and_then(|overlay| overlay.get("source_proposal_ids"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
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

pub fn dedupe_preserve_order(items: &mut Vec<String>) {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

#[cfg(test)]
mod tests;
