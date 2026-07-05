//! Dispatch profiles describe who to ask, which context/tools to expose, and
//! what evidence a delegated agent must return. They are intentionally separate
//! from `tachi_hub::ToolProfile`, which only gates MCP tool visibility.

use crate::agent_eval::{
    aggregate_performance_matrix, aggregate_subagent_scores, load_live_eval_rows,
    AgentPerformanceMatrixRow, CompletionStatus, EvalRow,
};
use crate::tool_params::TachiDispatchParams;
use crate::MemoryServer;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
pub(crate) use tachi_dispatch::DispatchProfileDef;
pub(crate) use tachi_dispatch::{
    fallback_chain, profile_matches_agent, profile_uses_opencode_adapter, resolve_dispatch_profile,
    ResolvedDispatchProfile, DISPATCH_POLICY_PROPOSAL_NS, DISPATCH_PROFILES,
    MIN_CARD_RISK_EVOLUTION_SAMPLES, MIN_LOADOUT_EVOLUTION_SAMPLES, MIN_ROUTE_POLICY_RULE_SAMPLES,
    PROFILE_CARD_OVERLAY_NS, ROUTE_POLICY_RULE_NS, ROUTE_POLICY_RULE_SCORE_BONUS,
};

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

#[cfg(test)]
pub(crate) use self::cards::profile_evidence_required;
pub(crate) use self::cards::{
    profile_eval_feedback_json, profile_evidence_contract_json,
    profile_evidence_contract_json_for_server, profile_evidence_required_for_server, profile_json,
    profile_json_for_server, profile_required_skill_ids, profile_required_skill_ids_for_server,
    profile_skill_loadout_json, profile_skill_loadout_json_for_server,
    profile_weak_against_for_server,
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
