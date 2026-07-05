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
use serde_json::{json, Value};
pub(crate) use tachi_dispatch::DispatchProfileDef;
pub(crate) use tachi_dispatch::{
    profile_uses_opencode_adapter, resolve_dispatch_profile, DispatchRisk, ResolvedDispatchProfile,
    RouteEvalRow, RoutePerformanceRow, RoutePolicyRuleLoadout, RoutePolicyRuleRecord,
    RouteSimulationSummary, RouteSubagentScore, DISPATCH_POLICY_PROPOSAL_NS, DISPATCH_PROFILES,
    PROFILE_CARD_OVERLAY_NS, ROUTE_POLICY_RULE_NS,
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

mod cards;
mod policy;
mod routing;

#[cfg(test)]
mod tests;

use self::cards::*;
use self::policy::*;
use self::routing::*;

#[cfg(test)]
pub(crate) use self::cards::profile_evidence_required;
pub(crate) use self::cards::{
    profile_eval_feedback_json, profile_evidence_contract_json_for_server,
    profile_evidence_required_for_server, profile_json, profile_json_for_server,
    profile_required_skill_ids, profile_required_skill_ids_for_server,
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
