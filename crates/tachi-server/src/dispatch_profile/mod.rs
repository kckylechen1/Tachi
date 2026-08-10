//! Dispatch profiles describe who to ask, which context/tools to expose, and
//! what evidence a delegated agent must return. They are intentionally separate
//! from `tachi_hub::ToolProfile`, which only gates MCP tool visibility.

#[cfg(test)]
use crate::agent_eval::CompletionStatus;
use crate::agent_eval::{
    aggregate_performance_matrix, aggregate_subagent_scores, load_live_eval_rows,
    AgentPerformanceMatrixRow, EvalRow,
};
use crate::tool_params::TachiDispatchParams;
use crate::MemoryServer;
use serde_json::{json, Value};
pub(crate) use tachi_dispatch::DispatchProfileDef;
pub(crate) use tachi_dispatch::{
    profile_uses_opencode_adapter, resolve_dispatch_profile, DispatchRisk, ResolvedDispatchProfile,
    RoutePolicyRuleLoadout, RoutePolicyRuleRecord, RouteSimulationSummary,
    DISPATCH_POLICY_PROPOSAL_NS, DISPATCH_PROFILES, PROFILE_CARD_OVERLAY_NS, ROUTE_POLICY_RULE_NS,
};

/// Server-side registry callers can request a slim or full dispatch-profile
/// projection. The operator `tachi card` command does not use this model-facing
/// JSON builder; it reads static profile/admission owners directly.
pub(crate) fn dispatch_profiles_json_for_server(
    server: &MemoryServer,
    verbose: bool,
) -> Result<Value, String> {
    let profiles = DISPATCH_PROFILES
        .iter()
        .map(|profile| profile_json_for_server(server, profile))
        .collect::<Result<Vec<_>, _>>()?;
    let profiles = if verbose {
        profiles
    } else {
        profiles.iter().map(slim_profile_row).collect()
    };
    Ok(json!({
        "dispatch_profiles": profiles,
        "note": "DispatchProfile routes agents/context/evidence; ToolProfile gates visible tools.",
        "projection_namespace": PROFILE_CARD_OVERLAY_NS,
        "verbose": verbose,
    }))
}

/// One row: name/backend/model/role. `full` is a `profile_json_for_server`
/// output — these four fields are always top-level on that shape (see
/// `tachi_dispatch::profiles::profile_json_with_loadout_and_evidence_contract`),
/// so slimming is a field-drop, not a re-derivation.
fn slim_profile_row(full: &Value) -> Value {
    json!({
        "name": full.get("name").cloned().unwrap_or(Value::Null),
        "backend": full.get("backend").cloned().unwrap_or(Value::Null),
        "model": full.get("model").cloned().unwrap_or(Value::Null),
        "role": full.get("role").cloned().unwrap_or(Value::Null),
    })
}

mod cards;
mod policy;
mod routing;

#[cfg(test)]
mod tests;

use self::policy::*;
use self::routing::*;

#[cfg(test)]
pub(crate) use self::cards::profile_evidence_required;
pub(crate) use self::cards::{
    profile_demotion_targets, profile_eval_feedback_json,
    profile_evidence_contract_json_for_server, profile_evidence_required_for_server, profile_json,
    profile_json_for_server, profile_required_skill_ids, profile_required_skill_ids_for_server,
    profile_skill_loadout_json_for_server, profile_weak_against_for_server,
};
pub(crate) use self::policy::{route_simulation_caveats, simulate_route_policy};
#[cfg(test)]
pub(crate) use self::routing::resolve_and_apply_dispatch_profile;
pub(crate) use self::routing::{
    classify_dispatch_risk, handle_dispatch_recommendation,
    resolve_and_apply_dispatch_profile_for_server,
};
