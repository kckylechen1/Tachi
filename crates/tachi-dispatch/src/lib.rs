//! Pure dispatch boundary shared by the memory server adapter.
//!
//! This crate intentionally contains policy and command construction only.
//! Runtime orchestration, MCP config generation, run artifacts, and database
//! writes stay in `memory-server`.

mod launcher;
pub mod policy;
mod profiles;
mod registry;
mod routing;

pub mod eval;
pub use launcher::{
    build_claude_launch, build_codex_launch, build_custom_launch, build_grok_launch,
    build_kimi_launch, is_trusted_dispatch_command, resolve_permission_profile, tail_chars,
    DispatchLaunchParams, LaunchCommand, PermissionProfile,
};
pub use profiles::{
    profile_demotion_targets_from_overlay, profile_evidence_contract_json,
    profile_evidence_contract_json_with_overlay, profile_evidence_required,
    profile_evidence_required_with_overlay, profile_json,
    profile_json_with_loadout_and_evidence_contract, profile_json_with_overlay,
    profile_matches_agent, profile_projected_evidence_required_from_overlay,
    profile_projected_passive_traits_from_overlay, profile_projected_signature_skills_from_overlay,
    profile_projected_weak_against_from_overlay, profile_required_skill_ids,
    profile_required_skill_ids_with_overlay, profile_skill_loadout_json,
    profile_skill_loadout_json_with_overlay, profile_uses_opencode_adapter, profile_weak_against,
    profile_weak_against_with_overlay, resolve_and_apply_dispatch_profile,
    resolve_dispatch_profile, DispatchProfileDef, ResolvedDispatchProfile,
    DISPATCH_POLICY_PROPOSAL_NS, DISPATCH_PROFILES, MIN_CARD_RISK_EVOLUTION_SAMPLES,
    MIN_LOADOUT_EVOLUTION_SAMPLES, MIN_ROUTE_POLICY_RULE_SAMPLES, PROFILE_CARD_OVERLAY_NS,
    ROUTE_POLICY_RULE_NS, ROUTE_POLICY_RULE_SCORE_BONUS,
};
pub use registry::{
    dispatch_agent_help_list, fallback_chain, mcp_inject_supported, normalize_dispatch_agent_name,
    resolve_dispatch_agent, select_agent_for_intent, select_agent_for_task, DispatchAgentDef,
    DispatchMcpSupport, DISPATCH_AGENTS,
};
pub use routing::{
    build_dispatch_recommendation_response, build_profile_fallback_chain,
    build_route_policy_rule_loadout, classify_dispatch_risk, profile_role_matches,
    recommend_dispatch_profile_candidates, route_eval_rows, route_performance_rows,
    route_simulation_caveats, route_subagent_scores, sanitize_policy_key, simulate_route_policy,
    AppliedRoutePolicyRule, DispatchRisk, ProfileCandidate, RecommendationProfilePayload,
    RouteEvalRow, RoutePerformanceRow, RoutePolicyRuleLoadout, RoutePolicyRuleRecord,
    RouteSimulationChoice, RouteSimulationSummary, RouteSubagentScore, SkippedRoutePolicyRule,
};
