//! Dispatch profiles describe who to ask, which context/tools to expose, and
//! what evidence a delegated agent must return. They are intentionally separate
//! from `tachi_hub::ToolProfile`, which only gates MCP tool visibility.

#[cfg(test)]
use crate::agent_eval::CompletionStatus;
// tachi#1675 PR4: `aggregate_subagent_scores` left with the recommendation
// path's evidence flip — the decision-fact ledger has no subagent-role rollup
// to aggregate. `load_live_eval_rows`/`aggregate_performance_matrix` stay for
// the profile CARD surface (`cards::profile_eval_feedback_json`), which reads
// `/eval` entries as human-readable feedback, not as routing evidence.
use crate::agent_eval::{
    aggregate_performance_matrix, load_live_eval_rows, AgentPerformanceMatrixRow, EvalRow,
};
use crate::tool_params::TachiDispatchParams;
use crate::MemoryServer;
use serde_json::{json, Value};
pub(crate) use tachi_dispatch::DispatchProfileDef;
pub(crate) use tachi_dispatch::{
    profile_uses_opencode_adapter, resolve_dispatch_profile, DispatchRisk, ResolvedDispatchProfile,
    RoutePolicyRuleRecord, RouteSimulationSummary, DISPATCH_POLICY_PROPOSAL_NS, DISPATCH_PROFILES,
    PROFILE_CARD_OVERLAY_NS, ROUTE_POLICY_RULE_NS,
};
// tachi#1675 BUG-8 follow-up: `RoutePolicyRuleLoadout` has no remaining
// production reference in this crate (the last one, `policy::rules`, was
// deleted — see the module doc on `dispatch_profile::policy`) — only
// `dispatch_profile::tests::scoring` still names the bare type directly.
#[cfg(test)]
pub(crate) use tachi_dispatch::RoutePolicyRuleLoadout;

/// tachi#1173 item 2: `verbose=false` returns one slim row per profile —
/// name/backend/model/role — instead of the full mbit_card
/// (stats/guidance/moves/personality/skill_loadout/evidence_contract) that a
/// listing call doesn't need. `verbose=true` preserves the pre-#1173 full-card
/// shape for callers that need it (e.g. the `tachi card` CLI).
///
/// Callers: `tachi_task(action='profiles')` (the listing) passes the
/// caller's `verbose` straight through, defaulting to slim. Per #1182
/// checkpoint 2, `action='profile'`/`action='card'` (the issue's "or
/// action=profile" escape hatch) invert that default — they call this same
/// function but default to `verbose=true` unless the caller explicitly asks
/// for the slim shape.
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

// tachi#1675 BUG-8 follow-up: `self::policy::*` used to be the only path
// bringing `load_route_policy_rule_loadout` into scope (a `pub(super)`
// item, so not reachable via the named re-export below); with that function
// deleted, `policy` has nothing left the named re-export at line 91 doesn't
// already cover, so the glob import is gone rather than narrowed to nothing.
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
