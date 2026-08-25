//! Pure dispatch boundary shared by the memory server adapter.
//!
//! This crate intentionally contains policy and command construction only.
//! Runtime orchestration, MCP config generation, run artifacts, and database
//! writes stay in `tachi-server`.

pub mod authority;
pub mod certification;
pub mod identity;
mod launcher;
mod model_registry;
mod native_skill_ids;
pub mod policy;
mod profiles;
mod registry;
mod routing;
pub mod signatures;

pub mod eval;
pub use authority::{
    compile_effective_contract, native_skill_requires_workspace_write, probe_provider_version,
    provider_has_sandbox_primitive, qualify_provider, transport_kind, Certification, ContractError,
    ContractInputs, EffectiveContract, Enforcement, ExcludedSkill, NetworkAuthority,
    ProviderQualification, SkillRequest, ToolAuthority, TransportKind, WorkspaceAuthority,
    CALLER_SANDBOX, CEILING_PROFILE, OPERATOR_BYPASS, PROVIDER_QUALIFICATIONS,
    SANDBOX_PRIMITIVE_PROVIDERS,
};
pub use certification::{
    parse_version_output, probe_backend_version, versions_match, CertificationReceipt,
    CertificationResult, CODEX_CLI_RECEIPT, CODEX_KILL_TEST_MATRIX, RECEIPTS,
};
pub use identity::{
    lineages_compatible, model_lineage_id, provider_model_parts, DispatchAcknowledgement,
    DispatchIdentityEffective, DispatchIdentityObserved, DispatchIdentityReceipt,
    DispatchIdentityRequest, IdentityAttributionBasis, DISPATCH_IDENTITY_CONTRACT_ID,
    UNKNOWN_IDENTITY,
};
pub use launcher::{
    build_claude_launch, build_codex_launch, build_custom_launch, build_grok_launch,
    build_kimi_launch, is_trusted_dispatch_command, reject_unsupported_sandbox,
    resolve_permission_profile, tail_chars, validate_codex_sandbox, DispatchLaunchParams,
    LaunchCommand, PermissionProfile, CODEX_SANDBOX_VALUES,
};
pub use model_registry::{
    dispatch_model_card, resolve_dispatch_model, resolve_dispatch_model_card,
    resolve_dispatch_model_with_config, DispatchModelCard, DISPATCH_MODEL_CARDS,
    GLM_CODING_DEFAULT_MODEL, GLM_CODING_ENV_OVERRIDE, GLM_CODING_MODEL_ALIAS,
};
pub use native_skill_ids::{
    is_native_skill_id, NativeSkillId, CODING_ARCHITECTURE_DECISION, CODING_REFACTOR_CHECKLIST,
    CODING_SKILL_IDS, CODING_TEST_STRATEGY, SUPERPOWER_BRAINSTORMING, SUPERPOWER_EXECUTING_PLANS,
    SUPERPOWER_FINISHING_BRANCH, SUPERPOWER_REQUESTING_CODE_REVIEW, SUPERPOWER_SKILL_IDS,
    SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT, SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
    SUPERPOWER_WRITING_PLANS, WAZA_CHECK, WAZA_DESIGN, WAZA_HEALTH, WAZA_HUNT, WAZA_LEARN,
    WAZA_READ, WAZA_SKILL_IDS, WAZA_TACHI, WAZA_THINK, WAZA_WRITE,
};
pub use profiles::{
    dispatch_profile_alias, profile_deprecated_aliases, profile_evidence_contract_json,
    profile_evidence_contract_json_with_overlay, profile_evidence_required,
    profile_evidence_required_with_overlay, profile_json,
    profile_json_with_loadout_and_evidence_contract, profile_json_with_overlay,
    profile_matches_agent, profile_projected_evidence_required_from_overlay,
    profile_required_skill_ids, profile_resolved_model, profile_skill_loadout_json,
    profile_uses_opencode_adapter, profile_weak_against, recommendation_identity_receipt,
    resolve_and_apply_dispatch_profile, resolve_and_apply_staff_assignment_profile,
    resolve_dispatch_profile, DispatchProfileAlias, DispatchProfileDef, ResolvedDispatchProfile,
    DISPATCH_POLICY_PROPOSAL_NS, DISPATCH_PROFILES, DISPATCH_PROFILE_ALIASES,
    MIN_EVOLUTION_SAMPLES, MIN_ROUTE_POLICY_RULE_SAMPLES, PROFILE_CARD_OVERLAY_NS,
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
    RouteEvalRow, RouteEvidenceSource, RoutePerformanceRow, RoutePolicyRuleLoadout,
    RoutePolicyRuleRecord, RouteSimulationChoice, RouteSimulationSummary, RouteSubagentScore,
    SkippedRoutePolicyRule, NO_LEDGER_EVIDENCE_REASON, RETIRED_EVIDENCE_SOURCE_SKIP_REASON,
    ROUTE_EVIDENCE_SOURCE_DECISION_FACT_LEDGER,
};
pub use signatures::{
    dispatch_role_class, normalize_vendor, project_counter_clauses, resolve_signature_id,
    self_report_trust, signature_def, ProjectedCounterClause, Severity, SignatureDef,
    SignatureEvidenceRow, SignatureRowKind, ACT_R_ACTIVATION_FLOOR, ACT_R_DECAY_RATE,
    ACT_R_MIN_AGE_DAYS, COUNTER_CLAUSE_TOP_N, ERROR_SIGNATURE_TAXONOMY, SIGNATURE_ALIASES,
};
pub use tachi_params::{
    ExecutionGrant, LaunchSpec, ResolvedStaffAssignment, StaffAssignmentRequest, StaffRunReceipt,
};
