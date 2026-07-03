//! Dispatch profiles describe who to ask, which context/tools to expose, and
//! what evidence a delegated agent must return. They are intentionally separate
//! from `profiles::ToolProfile`, which only gates MCP tool visibility.

use crate::agent_eval::{
    aggregate_performance_matrix, aggregate_subagent_scores, load_live_eval_rows,
    AgentPerformanceMatrixRow, CompletionStatus, EvalRow,
};
use crate::agent_registry::{fallback_chain, normalize_dispatch_agent_name};
use crate::skill_policy::{
    CODING_ARCHITECTURE_DECISION, CODING_REFACTOR_CHECKLIST, CODING_TEST_STRATEGY,
    SUPERPOWER_EXECUTING_PLANS, SUPERPOWER_REQUESTING_CODE_REVIEW,
    SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT, SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
    SUPERPOWER_WRITING_PLANS, WAZA_CHECK, WAZA_LEARN, WAZA_READ, WAZA_TACHI, WAZA_THINK,
};
use crate::tool_params::{DispatchMcpAccessParams, TachiDispatchParams};
use crate::MemoryServer;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const DISPATCH_POLICY_PROPOSAL_NS: &str = "dispatch_route_policy_proposals";
const ROUTE_POLICY_RULE_NS: &str = "dispatch_route_policy_rules";
const PROFILE_CARD_OVERLAY_NS: &str = "dispatch_profile_card_overlays";
const MIN_ROUTE_POLICY_RULE_SAMPLES: u32 = 2;
const MIN_LOADOUT_EVOLUTION_SAMPLES: u32 = 10;
const MIN_CARD_RISK_EVOLUTION_SAMPLES: u32 = 3;
const ROUTE_POLICY_RULE_SCORE_BONUS: f64 = 35.0;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DispatchProfileDef {
    pub name: &'static str,
    pub display_name: &'static str,
    pub backend: &'static str,
    pub role: &'static str,
    pub stage: Option<&'static str>,
    pub model: Option<&'static str>,
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

pub(crate) const DISPATCH_PROFILES: &[DispatchProfileDef] = &[
    DispatchProfileDef {
        name: "claude_plan",
        display_name: "Claude Plan",
        backend: "claude",
        role: "planner",
        stage: Some("plan"),
        model: None,
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
        name: "glm_51_impl",
        display_name: "GLM 5.1 Implementer",
        backend: "custom",
        role: "executor",
        stage: Some("execute"),
        model: Some("zhipuai-coding-plan/glm-5.1"),
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
        model: Some("zhipuai-coding-plan/glm-5.1"),
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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResolvedDispatchProfile {
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
}

pub(crate) fn resolve_dispatch_profile(raw: &str) -> Option<&'static DispatchProfileDef> {
    let norm = raw.trim().to_ascii_lowercase();
    DISPATCH_PROFILES
        .iter()
        .find(|profile| profile.name == norm)
}

pub(crate) fn profile_host_adapter(profile: &DispatchProfileDef) -> Option<&'static str> {
    match profile.backend {
        "opencode" => Some("opencode"),
        // Compatibility path: older custom profiles with a model but no command
        // are still materialized as OpenCode CLI/serve commands by routing.
        "custom" if profile.model.is_some() => Some("opencode"),
        _ => None,
    }
}

pub(crate) fn profile_uses_opencode_adapter(profile: &DispatchProfileDef) -> bool {
    profile_host_adapter(profile) == Some("opencode")
}

pub(crate) fn profile_matches_agent(profile: &DispatchProfileDef, agent: &str) -> bool {
    profile.backend == agent
        || matches!(
            (profile_host_adapter(profile), agent),
            (Some("opencode"), "custom" | "opencode")
        )
}

pub(crate) fn dispatch_profiles_json_for_server(server: &MemoryServer) -> Result<Value, String> {
    Ok(json!({
        "dispatch_profiles": DISPATCH_PROFILES
            .iter()
            .map(|profile| profile_json_for_server(server, profile))
            .collect::<Result<Vec<_>, _>>()?,
        "note": "DispatchProfile routes agents/context/evidence; ToolProfile gates visible tools.",
        "projection_namespace": PROFILE_CARD_OVERLAY_NS,
    }))
}

#[derive(Debug, Clone, Serialize)]
struct DispatchRisk {
    task_type: String,
    risk: String,
    reasons: Vec<String>,
    required_profiles: Vec<String>,
    blocked_profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ProfileCandidate {
    profile: String,
    agent: String,
    role: String,
    model: Option<String>,
    score: f64,
    reasons: Vec<String>,
    live_samples: u32,
    useful_rate: Option<f64>,
    failure_count: u32,
    performance_samples: u32,
    human_override_rate: Option<f64>,
    avg_retry_count: Option<f64>,
    avg_latency_ms: Option<f64>,
    avg_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct AppliedRoutePolicyRule {
    proposal_id: String,
    policy: String,
    task_type: String,
    prefer_profile: String,
    sample_count: u32,
    score_delta: Option<f64>,
    status: String,
}

#[derive(Debug, Clone, Serialize)]
struct SkippedRoutePolicyRule {
    proposal_id: String,
    reason: String,
    policy: Option<String>,
    task_type: Option<String>,
    prefer_profile: Option<String>,
    sample_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct RoutePolicyRuleLoadout {
    namespace: &'static str,
    min_samples: u32,
    applied: Vec<AppliedRoutePolicyRule>,
    skipped: Vec<SkippedRoutePolicyRule>,
}

#[derive(Debug, Clone, Serialize)]
struct RouteSimulationSummary {
    policy: String,
    selected_route_count: u32,
    sample_count: u32,
    estimated_success_rate: Option<f64>,
    estimated_verification_rate: Option<f64>,
    failure_count: u32,
    avg_retry_count: Option<f64>,
    avg_human_override_rate: Option<f64>,
    avg_latency_ms: Option<f64>,
    avg_cost_usd: Option<f64>,
    total_cost_usd: Option<f64>,
    score: f64,
    route_choices: Vec<RouteSimulationChoice>,
    caveats: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RouteSimulationChoice {
    task_type: String,
    profile: String,
    agent: String,
    samples: u32,
    score: f64,
    success_rate: Option<f64>,
    verification_rate: f64,
    failure_count: u32,
    avg_latency_ms: Option<f64>,
    avg_cost_usd: Option<f64>,
    avg_retry_count: f64,
    human_override_rate: f64,
    reasons: Vec<String>,
}

mod cards;
mod policy;
mod routing;
mod util;

#[cfg(test)]
mod tests;

use self::cards::*;
use self::policy::*;
use self::routing::*;
use self::util::*;

pub(crate) use self::cards::{
    profile_eval_feedback_json, profile_evidence_contract_json,
    profile_evidence_contract_json_for_server, profile_evidence_required,
    profile_evidence_required_for_server, profile_json, profile_json_for_server,
    profile_required_skill_ids, profile_required_skill_ids_for_server, profile_skill_loadout_json,
    profile_skill_loadout_json_for_server, profile_weak_against_for_server,
};
pub(crate) use self::policy::{
    handle_route_policy_apply, handle_route_policy_proposals, handle_route_policy_review,
    handle_route_simulation,
};
#[cfg(test)]
pub(crate) use self::routing::resolve_and_apply_dispatch_profile;
pub(crate) use self::routing::{
    handle_dispatch_recommendation, resolve_and_apply_dispatch_profile_for_server,
};
