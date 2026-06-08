//! Dispatch profiles describe who to ask, which context/tools to expose, and
//! what evidence a delegated agent must return. They are intentionally separate
//! from `profiles::ToolProfile`, which only gates MCP tool visibility.

use crate::agent_eval::{
    aggregate_performance_matrix, aggregate_subagent_scores, load_live_eval_rows,
    AgentPerformanceMatrixRow, CompletionStatus, EvalRow,
};
use crate::agent_registry::{fallback_chain, resolve_dispatch_agent};
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
        allowed_facades: &["tachi_briefing", "tachi_memory", "tachi_wiki", "tachi_task"],
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
        allowed_facades: &["tachi_memory", "tachi_task"],
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
        allowed_facades: &["tachi_memory", "tachi_task"],
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
        auto_capability_bundle: true,
        allowed_facades: &["tachi_briefing", "tachi_memory", "tachi_wiki", "tachi_task"],
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
        allowed_facades: &["tachi_memory"],
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
        allowed_facades: &["tachi_memory", "tachi_wiki"],
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
        allowed_facades: &["tachi_memory"],
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
    pub mbit_card: Option<Value>,
}

pub(crate) fn resolve_dispatch_profile(raw: &str) -> Option<&'static DispatchProfileDef> {
    let norm = raw.trim().to_ascii_lowercase();
    DISPATCH_PROFILES
        .iter()
        .find(|profile| profile.name == norm)
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

pub(crate) fn handle_dispatch_recommendation(
    server: &MemoryServer,
    task: &str,
    risk_override: Option<&str>,
    limit: usize,
    file_paths: &[String],
) -> Result<String, String> {
    let risk = classify_dispatch_risk(task, risk_override, file_paths);
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let subagent_scores = aggregate_subagent_scores(&rows);
    let performance_matrix = aggregate_performance_matrix(&rows);
    let route_policy_rules = load_route_policy_rule_loadout(server, &risk)?;

    let mut candidates = DISPATCH_PROFILES
        .iter()
        .map(|profile| {
            score_profile_candidate(profile, &risk, &rows, &subagent_scores, &performance_matrix)
        })
        .collect::<Vec<_>>();
    apply_route_policy_rules_to_candidates(&mut candidates, &route_policy_rules, &risk);
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.profile.cmp(&b.profile))
    });

    let best = candidates
        .first()
        .ok_or_else(|| "no dispatch profiles configured".to_string())?;
    let best_profile = resolve_dispatch_profile(&best.profile)
        .ok_or_else(|| format!("internal missing profile {}", best.profile))?;
    let fallback = build_profile_fallback_chain(best_profile, &candidates);
    let live_matched_samples = candidates.iter().map(|c| c.live_samples).sum::<u32>();
    let performance_matrix_hits = candidates
        .iter()
        .map(|c| c.performance_samples)
        .sum::<u32>();
    let evidence_note = if !route_policy_rules.applied.is_empty() {
        "route_policy_weighted: recommendation used matching /eval evidence plus approved route-policy rules."
    } else if live_matched_samples == 0 {
        "low_sample_fallback: no matching live /eval profile/subagent evidence; deterministic MBIT/risk fit dominated."
    } else {
        "live_eval_weighted: recommendation used matching /eval profile/subagent evidence."
    };

    serde_json::to_string(&json!({
        "task": task,
        "task_type": risk.task_type,
        "risk": risk.risk,
        "risk_reasons": risk.reasons,
        "required_profiles": risk.required_profiles,
        "blocked_profiles": risk.blocked_profiles,
        "recommended_profile": best.profile,
        "recommended_agent": best.agent,
        "role": best.role,
        "tool_profile": best_profile.tool_profile,
        "evidence_required": best_profile.evidence_required,
        "resolved_skills": profile_required_skill_ids_for_server(server, best_profile)?,
        "resolved_skill_loadout": profile_skill_loadout_json_for_server(server, best_profile)?,
        "fallback_chain": fallback,
        "reason": best.reasons,
        "route_explanation": best.reasons,
        "evidence_note": evidence_note,
        "live_eval": {
            "row_count": rows.len(),
            "matched_samples": live_matched_samples,
            "performance_matrix_hits": performance_matrix_hits,
        },
        "route_policy_rules": route_policy_rules,
        "mbit_card": profile_json_for_server(server, best_profile)?.get("mbit_card").cloned().unwrap_or(Value::Null),
        "candidates": candidates,
    }))
    .map_err(|e| format!("serialize recommendation: {e}"))
}

pub(crate) fn handle_route_simulation(
    server: &MemoryServer,
    limit: usize,
    focus_task: Option<&str>,
    risk_override: Option<&str>,
    file_paths: &[String],
) -> Result<String, String> {
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let focus = focus_task
        .filter(|task| !task.trim().is_empty())
        .map(|task| classify_dispatch_risk(task, risk_override, file_paths));

    let summaries = ["current", "cost_sensitive", "quality_first"]
        .iter()
        .map(|policy| simulate_route_policy(policy, &performance_matrix, focus.as_ref()))
        .collect::<Vec<_>>();

    serde_json::to_string(&json!({
        "action": "route_simulate",
        "read_only": true,
        "source": "live_memory_eval",
        "row_count": rows.len(),
        "matrix_rows": performance_matrix.len(),
        "limit": limit.max(1),
        "focus": focus.as_ref().map(|risk| json!({
            "task_type": risk.task_type,
            "risk": risk.risk,
            "risk_reasons": risk.reasons,
            "required_profiles": risk.required_profiles,
            "blocked_profiles": risk.blocked_profiles,
        })),
        "policies": summaries,
        "caveats": route_simulation_caveats(&rows, &performance_matrix),
    }))
    .map_err(|e| format!("serialize route simulation: {e}"))
}

pub(crate) fn handle_route_policy_proposals(
    server: &MemoryServer,
    limit: usize,
    status_filter: Option<&str>,
) -> Result<String, String> {
    let rows = load_live_eval_rows(server, limit.max(1))?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let current = simulate_route_policy("current", &performance_matrix, None);
    let variants = ["cost_sensitive", "quality_first"]
        .iter()
        .map(|policy| simulate_route_policy(policy, &performance_matrix, None))
        .collect::<Vec<_>>();
    let mut proposals = build_route_policy_proposals(&current, &variants, rows.len(), limit.max(1));
    proposals.extend(build_loadout_evolution_proposals(
        server,
        &performance_matrix,
        limit.max(1),
    )?);

    server.with_global_store(|store| {
        for proposal in proposals {
            let id = proposal["proposal_id"]
                .as_str()
                .ok_or_else(|| "route policy proposal missing id".to_string())?
                .to_string();
            let mut next = proposal;
            if let Some((existing, _version)) = store
                .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, &id)
                .map_err(|e| format!("load route policy proposal: {e}"))?
            {
                if let Ok(existing_json) = serde_json::from_str::<Value>(&existing) {
                    let existing_status = existing_json
                        .get("status")
                        .and_then(|value| value.as_str())
                        .unwrap_or("pending");
                    if existing_status != "pending" {
                        next["status"] = json!(existing_status);
                    }
                    if let Some(review) = existing_json.get("review") {
                        next["review"] = review.clone();
                    }
                    if let Some(applied_at) = existing_json.get("applied_at") {
                        next["applied_at"] = applied_at.clone();
                    }
                }
            }
            let raw = serde_json::to_string(&next)
                .map_err(|e| format!("serialize route policy proposal: {e}"))?;
            store
                .set_state(DISPATCH_POLICY_PROPOSAL_NS, &id, &raw)
                .map_err(|e| format!("persist route policy proposal: {e}"))?;
        }
        Ok(())
    })?;

    let desired = status_filter
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all")
        .map(|value| value.to_ascii_lowercase());
    let mut records = server.with_global_store_read(|store| {
        store
            .list_state(DISPATCH_POLICY_PROPOSAL_NS)
            .map_err(|e| format!("list route policy proposals: {e}"))
    })?;
    records.truncate(limit.max(1).min(100));
    let mut out = Vec::new();
    for row in records {
        let mut value: Value = serde_json::from_str(&row.value_json)
            .unwrap_or_else(|_| json!({ "proposal_id": row.key, "raw": row.value_json }));
        value["state_version"] = json!(row.version);
        value["updated_at"] = json!(row.updated_at);
        let status = value
            .get("status")
            .and_then(|status| status.as_str())
            .unwrap_or("pending");
        if desired.as_deref().is_some_and(|wanted| wanted != status) {
            continue;
        }
        out.push(value);
    }

    serde_json::to_string(&json!({
        "action": "proposals",
        "kind": "dispatch_policy",
        "proposal_kinds": ["route_policy", "loadout_evolution"],
        "read_only": false,
        "requires_human_approval": true,
        "generated_from": {
            "source": "live_memory_eval",
            "row_count": rows.len(),
            "matrix_rows": performance_matrix.len(),
            "limit": limit.max(1),
        },
        "count": out.len(),
        "proposals": out,
        "next_actions": [
            "tachi_task(action='review_proposal', proposal_id=..., review_status='approved')",
            "tachi_task(action='apply_proposals', proposal_id=..., confirm=true) for approved route_policy rules",
            "tachi_task(action='apply_proposals', proposal_id=..., confirm=true) for approved loadout_evolution proposals to project reviewed profile/card loadout overlays"
        ],
    }))
    .map_err(|e| format!("serialize route policy proposals: {e}"))
}

pub(crate) fn handle_route_policy_review(
    server: &MemoryServer,
    proposal_id: &str,
    review_status: &str,
    note: Option<&str>,
) -> Result<String, String> {
    let proposal_id = proposal_id.trim();
    if proposal_id.is_empty() {
        return Err("proposal_id is required when action='review_proposal'".to_string());
    }
    let status = match review_status.trim().to_ascii_lowercase().as_str() {
        "approved" | "approve" => "approved",
        "rejected" | "reject" => "rejected",
        other => {
            return Err(format!(
                "Invalid review_status '{}'. Expected approved|rejected",
                other
            ))
        }
    };
    let reviewed_at = Utc::now().to_rfc3339();
    let updated = server.with_global_store(|store| {
        let (raw, _version) = store
            .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load route policy proposal: {e}"))?
            .ok_or_else(|| format!("route policy proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse route policy proposal: {e}"))?;
        value["status"] = json!(status);
        value["review"] = json!({
            "status": status,
            "note": note,
            "reviewed_at": reviewed_at,
        });
        let next = serde_json::to_string(&value)
            .map_err(|e| format!("serialize route policy review: {e}"))?;
        store
            .set_state(DISPATCH_POLICY_PROPOSAL_NS, proposal_id, &next)
            .map_err(|e| format!("persist route policy review: {e}"))?;
        Ok(value)
    })?;

    serde_json::to_string(&json!({
        "action": "review_proposal",
        "proposal_id": proposal_id,
        "proposal": updated,
    }))
    .map_err(|e| format!("serialize route policy review response: {e}"))
}

pub(crate) fn handle_route_policy_apply(
    server: &MemoryServer,
    proposal_id: &str,
    confirm: bool,
) -> Result<String, String> {
    let proposal_id = proposal_id.trim();
    if proposal_id.is_empty() {
        return Err("proposal_id is required when action='apply_proposals'".to_string());
    }
    if !confirm {
        return Err(
            "apply_proposals requires confirm=true after human approval; no routing changes applied"
                .to_string(),
        );
    }
    let applied_at = Utc::now().to_rfc3339();
    let updated = server.with_global_store(|store| {
        let (raw, _version) = store
            .get_state_kv(DISPATCH_POLICY_PROPOSAL_NS, proposal_id)
            .map_err(|e| format!("load route policy proposal: {e}"))?
            .ok_or_else(|| format!("route policy proposal not found: {proposal_id}"))?;
        let mut value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse route policy proposal: {e}"))?;
        let status = value
            .get("status")
            .and_then(|status| status.as_str())
            .unwrap_or("pending");
        if status != "approved" {
            return Err(format!(
                "route policy proposal {proposal_id} must be approved before apply; current status={status}"
            ));
        }
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("route_policy");
        match kind {
            "route_policy" => {
                value["status"] = json!("applied");
                value["applied_at"] = json!(applied_at);
                let next = serde_json::to_string(&value)
                    .map_err(|e| format!("serialize applied route policy proposal: {e}"))?;
                store
                    .set_state(DISPATCH_POLICY_PROPOSAL_NS, proposal_id, &next)
                    .map_err(|e| format!("persist applied route policy proposal: {e}"))?;
                store
                    .set_state(ROUTE_POLICY_RULE_NS, proposal_id, &next)
                    .map_err(|e| format!("persist route policy rule: {e}"))?;
            }
            "loadout_evolution" => {
                let profile_name = value
                    .get("profile")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!("loadout_evolution proposal {proposal_id} missing profile")
                    })?;
                let profile = resolve_dispatch_profile(profile_name).ok_or_else(|| {
                    format!(
                        "loadout_evolution proposal {proposal_id} references unknown profile {profile_name}"
                    )
                })?;
                let operation = value
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if operation != "promote_observed_skill_to_signature" {
                    return Err(format!(
                        "unsupported loadout_evolution operation for {proposal_id}: {operation}"
                    ));
                }
                let skill_id = value
                    .get("skill_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|skill| !skill.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        format!("loadout_evolution proposal {proposal_id} missing skill_id")
                    })?;
                if profile
                    .forbidden_skills
                    .iter()
                    .any(|skill| *skill == skill_id)
                {
                    return Err(format!(
                        "loadout_evolution proposal {proposal_id} targets forbidden skill {skill_id}"
                    ));
                }

                let mut overlay = if let Some((raw, _version)) = store
                    .get_state_kv(PROFILE_CARD_OVERLAY_NS, profile.name)
                    .map_err(|e| format!("load profile/card overlay: {e}"))?
                {
                    serde_json::from_str::<Value>(&raw)
                        .map_err(|e| format!("parse profile/card overlay: {e}"))?
                } else {
                    json!({
                        "kind": "profile_card_loadout_overlay",
                        "profile": profile.name,
                        "add_signature_skills": [],
                        "source_proposal_ids": [],
                        "created_at": applied_at,
                    })
                };

                let baseline_skills = profile_required_skill_ids(profile);
                if baseline_skills.iter().any(|skill| skill == &skill_id) {
                    return Err(format!(
                        "loadout_evolution proposal {proposal_id} targets existing baseline skill {skill_id}"
                    ));
                }
                let mut overlay_skills =
                    profile_projected_signature_skills_from_overlay(profile, Some(&overlay));
                let already_projected = overlay_skills.iter().any(|skill| skill == &skill_id);

                if !overlay_skills.iter().any(|skill| skill == &skill_id) {
                    overlay_skills.push(skill_id.clone());
                }
                crate::skill_policy::dedupe_preserve_order(&mut overlay_skills);

                let mut source_proposals = overlay
                    .get("source_proposal_ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                source_proposals.push(proposal_id.to_string());
                crate::skill_policy::dedupe_preserve_order(&mut source_proposals);

                overlay["profile"] = json!(profile.name);
                overlay["kind"] = json!("profile_card_loadout_overlay");
                overlay["add_signature_skills"] = json!(overlay_skills);
                overlay["source_proposal_ids"] = json!(source_proposals);
                overlay["updated_at"] = json!(applied_at);
                overlay["last_applied_proposal_id"] = json!(proposal_id);
                let overlay_raw = serde_json::to_string(&overlay)
                    .map_err(|e| format!("serialize profile/card overlay: {e}"))?;
                store
                    .set_state(PROFILE_CARD_OVERLAY_NS, profile.name, &overlay_raw)
                    .map_err(|e| format!("persist profile/card overlay: {e}"))?;

                value["status"] = json!("applied");
                value["applied_at"] = json!(applied_at);
                value["projection"] = json!({
                    "status": "applied_profile_card_overlay",
                    "namespace": PROFILE_CARD_OVERLAY_NS,
                    "key": profile.name,
                    "already_projected": already_projected,
                    "added_signature_skills": [skill_id],
                    "note": "Reviewed loadout evolution is projected as a durable profile/card overlay; built-in static definitions remain the baseline."
                });
                let next = serde_json::to_string(&value)
                    .map_err(|e| format!("serialize applied loadout proposal: {e}"))?;
                store
                    .set_state(DISPATCH_POLICY_PROPOSAL_NS, proposal_id, &next)
                    .map_err(|e| format!("persist applied loadout proposal: {e}"))?;
            }
            other => {
                return Err(format!(
                    "apply_proposals does not support proposal kind {other} for {proposal_id}"
                ));
            }
        }
        Ok(value)
    })?;
    let applied_kind = updated
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("route_policy");

    serde_json::to_string(&json!({
        "action": "apply_proposals",
        "proposal_id": proposal_id,
        "applied": true,
        "routing_mutated": applied_kind == "route_policy",
        "profile_card_mutated": applied_kind == "loadout_evolution",
        "rule_namespace": if applied_kind == "route_policy" { Value::String(ROUTE_POLICY_RULE_NS.to_string()) } else { Value::Null },
        "projection_namespace": if applied_kind == "loadout_evolution" { Value::String(PROFILE_CARD_OVERLAY_NS.to_string()) } else { Value::Null },
        "proposal": updated,
        "note": if applied_kind == "route_policy" {
            "Approved route-policy rule was persisted and will be consumed by recommend() when task type, risk gates, and sample thresholds match."
        } else {
            "Approved loadout-evolution proposal was projected into the profile/card overlay and will be visible in profile, loadout, recommend, and dispatch prompt surfaces."
        },
    }))
    .map_err(|e| format!("serialize route policy apply response: {e}"))
}

fn load_route_policy_rule_loadout(
    server: &MemoryServer,
    risk: &DispatchRisk,
) -> Result<RoutePolicyRuleLoadout, String> {
    let records = server.with_global_store_read(|store| {
        store
            .list_state(ROUTE_POLICY_RULE_NS)
            .map_err(|e| format!("list route policy rules: {e}"))
    })?;
    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    for row in records {
        let proposal_id = row.key.clone();
        let value: Value = match serde_json::from_str(&row.value_json) {
            Ok(value) => value,
            Err(err) => {
                skipped.push(SkippedRoutePolicyRule {
                    proposal_id,
                    reason: format!("invalid_json:{err}"),
                    policy: None,
                    task_type: None,
                    prefer_profile: None,
                    sample_count: None,
                });
                continue;
            }
        };
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let review_status = value
            .get("review")
            .and_then(|review| review.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let policy_rule = value.get("policy_rule").unwrap_or(&Value::Null);
        let policy = policy_rule
            .get("policy")
            .and_then(Value::as_str)
            .or_else(|| value.get("policy").and_then(Value::as_str))
            .map(str::to_string);
        let task_type = policy_rule
            .get("when_task_type")
            .and_then(Value::as_str)
            .map(str::to_string);
        let prefer_profile = policy_rule
            .get("prefer_profile")
            .and_then(Value::as_str)
            .map(str::to_string);
        let sample_count = route_policy_rule_sample_count(&value);
        let score_delta = value.get("score_delta").and_then(Value::as_f64);

        let skip_reason = if status != "applied" {
            Some(format!("status_not_applied:{status}"))
        } else if review_status != "approved" {
            Some(format!("review_not_approved:{review_status}"))
        } else if task_type.as_deref() != Some(risk.task_type.as_str()) {
            Some(format!(
                "task_type_mismatch:{}",
                task_type.as_deref().unwrap_or("missing")
            ))
        } else if sample_count < MIN_ROUTE_POLICY_RULE_SAMPLES {
            Some(format!(
                "insufficient_samples:{sample_count}<{}",
                MIN_ROUTE_POLICY_RULE_SAMPLES
            ))
        } else if prefer_profile
            .as_deref()
            .is_none_or(|profile| resolve_dispatch_profile(profile).is_none())
        {
            Some(format!(
                "unknown_prefer_profile:{}",
                prefer_profile.as_deref().unwrap_or("missing")
            ))
        } else if prefer_profile.as_deref().is_some_and(|profile| {
            risk.blocked_profiles
                .iter()
                .any(|blocked| blocked == profile)
        }) {
            Some(format!(
                "blocked_by_risk_classifier:{}",
                prefer_profile.as_deref().unwrap_or("missing")
            ))
        } else {
            None
        };

        if let Some(reason) = skip_reason {
            skipped.push(SkippedRoutePolicyRule {
                proposal_id,
                reason,
                policy,
                task_type,
                prefer_profile,
                sample_count: Some(sample_count),
            });
            continue;
        }

        applied.push(AppliedRoutePolicyRule {
            proposal_id,
            policy: policy.unwrap_or_else(|| "unknown".to_string()),
            task_type: task_type.unwrap_or_else(|| risk.task_type.clone()),
            prefer_profile: prefer_profile.unwrap_or_default(),
            sample_count,
            score_delta,
            status: status.to_string(),
        });
    }

    Ok(RoutePolicyRuleLoadout {
        namespace: ROUTE_POLICY_RULE_NS,
        min_samples: MIN_ROUTE_POLICY_RULE_SAMPLES,
        applied,
        skipped,
    })
}

fn route_policy_rule_sample_count(value: &Value) -> u32 {
    value
        .get("evidence")
        .and_then(|evidence| evidence.get("proposed"))
        .and_then(|proposed| proposed.get("samples"))
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .get("evidence")
                .and_then(|evidence| evidence.get("row_count"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

fn apply_route_policy_rules_to_candidates(
    candidates: &mut [ProfileCandidate],
    rules: &RoutePolicyRuleLoadout,
    risk: &DispatchRisk,
) {
    for rule in &rules.applied {
        if rule.task_type != risk.task_type {
            continue;
        }
        if let Some(candidate) = candidates
            .iter_mut()
            .find(|candidate| candidate.profile == rule.prefer_profile)
        {
            candidate.score = round2(candidate.score + ROUTE_POLICY_RULE_SCORE_BONUS);
            candidate.reasons.push(format!(
                "approved_route_policy_rule:{} policy={} samples={} score_delta={:.2}",
                rule.proposal_id,
                rule.policy,
                rule.sample_count,
                rule.score_delta.unwrap_or_default()
            ));
        }
    }
}

#[cfg(test)]
pub(crate) fn resolve_and_apply_dispatch_profile(
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    resolve_and_apply_dispatch_profile_inner(None, params)
}

pub(crate) fn resolve_and_apply_dispatch_profile_for_server(
    server: &MemoryServer,
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    resolve_and_apply_dispatch_profile_inner(Some(server), params)
}

fn resolve_and_apply_dispatch_profile_inner(
    server: Option<&MemoryServer>,
    params: &mut TachiDispatchParams,
) -> Result<ResolvedDispatchProfile, String> {
    let mut route_explanation = Vec::new();
    let requested_agent = params.agent.clone().filter(|s| !s.trim().is_empty());
    let profile = match params.profile.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(raw) => Some(resolve_dispatch_profile(raw).ok_or_else(|| {
            format!(
                "Unknown dispatch profile '{}'. Supported: {}",
                raw.trim(),
                DISPATCH_PROFILES
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?),
        None => None,
    };

    if let Some(profile) = profile {
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
        if params.model.is_none() {
            params.model = profile.model.map(str::to_string);
        }
        if profile.backend == "custom" && params.command.is_empty() {
            if let Some(model) = profile.model {
                params.command = vec![
                    "opencode".to_string(),
                    "--pure".to_string(),
                    "run".to_string(),
                    "--model".to_string(),
                    model.to_string(),
                ];
                route_explanation.push(format!(
                    "profile selected opencode custom command for model '{}'",
                    model
                ));
            }
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
            params.auto_capability_bundle = Some(profile.auto_capability_bundle);
        }
        if params.skills.is_empty() {
            params.skills = match server {
                Some(server) => profile_required_skill_ids_for_server(server, profile)?,
                None => profile_required_skill_ids(profile),
            };
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
    let agent_norm = if agent.eq_ignore_ascii_case("custom") {
        "custom".to_string()
    } else if let Some(def) = resolve_dispatch_agent(&agent) {
        def.name.to_string()
    } else {
        agent
    };
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
    let evidence_required = profile
        .map(|p| p.evidence_required.iter().map(|s| s.to_string()).collect())
        .unwrap_or_else(Vec::new);
    let fallback_chain = fallback_chain(&agent_norm)
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut credential_profiles = params
        .credential_profiles
        .iter()
        .map(|profile| profile.trim().to_string())
        .filter(|profile| !profile.is_empty())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut credential_profiles);

    Ok(ResolvedDispatchProfile {
        selected_profile: profile.map(|p| p.name.to_string()),
        agent: agent_norm,
        role: profile.map(|p| p.role.to_string()),
        tool_profile: params.tool_profile.clone(),
        auto_capability_bundle: params.auto_capability_bundle.unwrap_or(false),
        mcp_access,
        evidence_required,
        fallback_chain,
        credential_profiles,
        route_explanation,
        mbit_card: profile
            .map(|profile| match server {
                Some(server) => profile_json_for_server(server, profile),
                None => Ok(profile_json(profile)),
            })
            .transpose()?,
    })
}

pub(crate) fn profile_json(profile: &DispatchProfileDef) -> Value {
    profile_json_with_loadout(profile, profile_skill_loadout_json(profile))
}

pub(crate) fn profile_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    Ok(profile_json_with_loadout(
        profile,
        profile_skill_loadout_json_for_server(server, profile)?,
    ))
}

fn profile_json_with_loadout(profile: &DispatchProfileDef, skill_loadout: Value) -> Value {
    json!({
        "name": profile.name,
        "display_name": profile.display_name,
        "backend": profile.backend,
        "role": profile.role,
        "stage": profile.stage,
        "model": profile.model,
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
        "evidence_contract": {
            "required": profile.evidence_required,
        },
        "mbit_card": {
            "display_name": profile.display_name,
            "type": [profile.role],
            "strong_against": profile.strong_against,
            "weak_against": profile.weak_against,
            "auto_capability_bundle": profile.auto_capability_bundle,
            "skill_loadout": skill_loadout,
        }
    })
}

pub(crate) fn profile_required_skill_ids(profile: &DispatchProfileDef) -> Vec<String> {
    let mut skills = profile
        .common_skills
        .iter()
        .chain(profile.signature_skills.iter())
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut skills);
    skills
}

pub(crate) fn profile_required_skill_ids_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let mut skills = profile
        .common_skills
        .iter()
        .chain(profile.signature_skills.iter())
        .map(|skill| skill.to_string())
        .collect::<Vec<_>>();
    skills.extend(profile_projected_signature_skills(server, profile)?);
    crate::skill_policy::dedupe_preserve_order(&mut skills);
    Ok(skills)
}

pub(crate) fn profile_skill_loadout_json(profile: &DispatchProfileDef) -> Value {
    json!({
        "common_skills": profile.common_skills,
        "signature_skills": profile.signature_skills,
        "passive_traits": profile.passive_traits,
        "forbidden_skills": profile.forbidden_skills,
        "projection": {
            "status": "baseline",
        },
    })
}

pub(crate) fn profile_skill_loadout_json_for_server(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Value, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    let projected_signature_skills =
        profile_projected_signature_skills_from_overlay(profile, overlay.as_ref());
    let mut signature_skills = profile
        .signature_skills
        .iter()
        .map(|skill| skill.to_string())
        .collect::<Vec<_>>();
    signature_skills.extend(projected_signature_skills.iter().cloned());
    crate::skill_policy::dedupe_preserve_order(&mut signature_skills);
    let source_proposal_ids = overlay
        .as_ref()
        .and_then(|overlay| overlay.get("source_proposal_ids"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(json!({
        "common_skills": profile.common_skills,
        "signature_skills": signature_skills,
        "projected_signature_skills": projected_signature_skills,
        "passive_traits": profile.passive_traits,
        "forbidden_skills": profile.forbidden_skills,
        "projection": {
            "status": if overlay.is_some() { "applied_overlay" } else { "baseline" },
            "namespace": PROFILE_CARD_OVERLAY_NS,
            "key": profile.name,
            "source_proposal_ids": source_proposal_ids,
        },
    }))
}

fn profile_projected_signature_skills(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
) -> Result<Vec<String>, String> {
    let overlay = load_profile_overlay(server, profile.name)?;
    Ok(profile_projected_signature_skills_from_overlay(
        profile,
        overlay.as_ref(),
    ))
}

fn profile_projected_signature_skills_from_overlay(
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
    crate::skill_policy::dedupe_preserve_order(&mut skills);
    skills
}

fn load_profile_overlay(server: &MemoryServer, profile: &str) -> Result<Option<Value>, String> {
    server
        .with_global_store_read(|store| {
            store
                .get_state_kv(PROFILE_CARD_OVERLAY_NS, profile)
                .map_err(|e| format!("load profile/card overlay: {e}"))
        })?
        .map(|(raw, _version)| {
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| format!("parse profile/card overlay: {e}"))
        })
        .transpose()
}

pub(crate) fn profile_eval_feedback_json(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
    limit: usize,
) -> Result<Value, String> {
    let limit = limit.max(1);
    let rows = load_live_eval_rows(server, limit)?;
    let performance_matrix = aggregate_performance_matrix(&rows);
    let profile_rows = performance_matrix
        .iter()
        .filter(|row| row.profile.as_deref() == Some(profile.name))
        .cloned()
        .collect::<Vec<_>>();
    let fallback_rows = performance_matrix
        .iter()
        .filter(|row| {
            row.profile.is_none()
                && (row.agent == profile.backend
                    || row
                        .role
                        .as_deref()
                        .is_some_and(|role| profile_role_matches(profile, role)))
        })
        .cloned()
        .collect::<Vec<_>>();

    let profile_samples = sum_matrix_samples(&profile_rows);
    let fallback_role_samples = sum_matrix_samples(&fallback_rows);
    let summary = summarize_matrix_rows(&profile_rows);
    let failure_count = sum_matrix_failures(&profile_rows);
    let human_override_rate =
        weighted_matrix_rate(&profile_rows, |row| Some(row.human_override_rate));
    let avg_retry_count = weighted_matrix_rate(&profile_rows, |row| Some(row.avg_retry_count));
    let verification_rate = weighted_matrix_rate(&profile_rows, |row| Some(row.verification_rate));
    let success_rate = weighted_matrix_rate(&profile_rows, |row| row.success_rate);
    let useful_rate = weighted_matrix_rate(&profile_rows, |row| row.useful_rate);

    let mut guidance = Vec::new();
    if profile_samples == 0 {
        guidance.push(
            "low_sample: no live /eval rows for this profile; keep deterministic MBIT loadout"
                .to_string(),
        );
    } else if profile_samples < MIN_LOADOUT_EVOLUTION_SAMPLES {
        guidance.push(format!(
            "low_sample: {} profile samples below evolution threshold {}",
            profile_samples, MIN_LOADOUT_EVOLUTION_SAMPLES
        ));
    } else {
        guidance.push(
            "evidence_available: profile has enough live samples for loadout review".to_string(),
        );
    }
    if failure_count > 0 {
        guidance.push(format!(
            "caution: {} failures present before promoting signature skills",
            failure_count
        ));
    }
    if human_override_rate.unwrap_or(0.0) >= 0.10 {
        guidance.push("review_required: human overrides are elevated for this profile".to_string());
    }
    if avg_retry_count.unwrap_or(0.0) >= 1.0 {
        guidance
            .push("review_required: retry count suggests loadout or prompt friction".to_string());
    }
    if verification_rate.unwrap_or(0.0) < 0.50 && profile_samples > 0 {
        guidance.push("evidence_gap: completions need stronger verification evidence".to_string());
    }
    if profile_samples >= MIN_LOADOUT_EVOLUTION_SAMPLES
        && failure_count == 0
        && human_override_rate.unwrap_or(0.0) < 0.10
        && avg_retry_count.unwrap_or(0.0) < 1.0
        && success_rate.or(useful_rate).unwrap_or(0.0) >= 0.80
    {
        guidance.push(
            "promotion_candidate: stable profile feedback can seed a reviewed loadout proposal"
                .to_string(),
        );
    }

    Ok(json!({
        "source": "live_eval",
        "limit": limit,
        "row_count": rows.len(),
        "matrix_rows": performance_matrix.len(),
        "profile_samples": profile_samples,
        "fallback_role_samples": fallback_role_samples,
        "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
        "summary": summary,
        "performance_by_task": profile_rows,
        "role_backend_fallback": fallback_rows,
        "guidance": guidance,
    }))
}

fn profile_role_matches(profile: &DispatchProfileDef, role: &str) -> bool {
    role == profile.role
        || (profile.role == "planner" && role == "architect")
        || (profile.role == "architect" && role == "critic")
        || (profile.role == "executor" && role == "implementer")
        || (profile.role.contains("review") && role.contains("review"))
}

fn sum_matrix_samples(rows: &[AgentPerformanceMatrixRow]) -> u32 {
    rows.iter().map(|row| row.samples).sum()
}

fn sum_matrix_failures(rows: &[AgentPerformanceMatrixRow]) -> u32 {
    rows.iter().map(|row| row.failure_count).sum()
}

fn weighted_matrix_rate<F>(rows: &[AgentPerformanceMatrixRow], value: F) -> Option<f64>
where
    F: Fn(&AgentPerformanceMatrixRow) -> Option<f64>,
{
    let mut weighted_sum = 0.0;
    let mut samples = 0_u32;
    for row in rows {
        let Some(value) = value(row) else {
            continue;
        };
        weighted_sum += value * row.samples as f64;
        samples += row.samples;
    }
    (samples > 0).then(|| round4(weighted_sum / samples as f64))
}

fn summarize_matrix_rows(rows: &[AgentPerformanceMatrixRow]) -> Value {
    json!({
        "samples": sum_matrix_samples(rows),
        "success_rate": weighted_matrix_rate(rows, |row| row.success_rate),
        "useful_rate": weighted_matrix_rate(rows, |row| row.useful_rate),
        "verification_rate": weighted_matrix_rate(rows, |row| Some(row.verification_rate)),
        "failure_count": sum_matrix_failures(rows),
        "human_override_rate": weighted_matrix_rate(rows, |row| Some(row.human_override_rate)),
        "avg_retry_count": weighted_matrix_rate(rows, |row| Some(row.avg_retry_count)),
        "avg_latency_ms": weighted_matrix_rate(rows, |row| row.avg_latency_ms),
        "avg_cost_usd": weighted_matrix_rate(rows, |row| row.avg_cost_usd),
        "avg_quality_score": weighted_matrix_rate(rows, |row| row.avg_quality_score),
    })
}

fn classify_dispatch_risk(
    task: &str,
    risk_override: Option<&str>,
    file_paths: &[String],
) -> DispatchRisk {
    let route = crate::copilot_ops::build_task_brief_routing(task, &[]);
    let task_type = route.intent.to_string();
    let lower = task.to_ascii_lowercase();
    let lower_paths = file_paths
        .iter()
        .map(|path| path.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut reasons = Vec::new();
    let mut risk = "medium".to_string();

    let mut push_reason = |reason: &str| {
        if !reasons.iter().any(|existing| existing == reason) {
            reasons.push(reason.to_string());
        }
    };

    for (needle, reason) in dispatch_risk_needles() {
        if lower.contains(needle) {
            push_reason(reason);
        }
    }
    for lower_path in &lower_paths {
        for (needle, reason) in dispatch_risk_needles() {
            if lower_path.contains(needle) {
                push_reason(reason);
            }
        }
    }
    if matches!(
        task_type.as_str(),
        "migration_request" | "refactor_request" | "review_request"
    ) {
        push_reason(&format!("task_type={task_type}"));
    }
    let missing_verification = indicates_missing_verification(&lower);
    if missing_verification {
        push_reason("missing_verification_signal");
    }
    if indicates_prior_failure(&lower) {
        push_reason("prior_failure_or_regression_hint");
    }
    if !reasons.is_empty()
        && (contains_high_risk_surface(&lower)
            || lower_paths
                .iter()
                .any(|path| contains_high_risk_surface(path))
            || missing_verification
            || reasons
                .iter()
                .any(|reason| reason == "prior_failure_or_regression_hint"))
    {
        risk = "high".to_string();
    } else if matches!(task_type.as_str(), "explain_request" | "research_request") {
        risk = "low".to_string();
    }
    if let Some(override_risk) = risk_override.filter(|s| !s.trim().is_empty()) {
        risk = override_risk.trim().to_ascii_lowercase();
        reasons.push(format!("user_override={risk}"));
    }
    if reasons.is_empty() {
        reasons.push("default deterministic route classification".to_string());
    }

    let required_profiles = match risk.as_str() {
        "high" | "critical" => vec!["claude_plan".to_string(), "codex_55_review".to_string()],
        "low" if task_type == "review_request" => vec!["codex_53_fast".to_string()],
        _ if task_type == "plan_request" => vec!["claude_plan".to_string()],
        _ if task_type == "review_request" => vec!["codex_55_review".to_string()],
        _ => Vec::new(),
    };
    let blocked_profiles = if matches!(risk.as_str(), "high" | "critical") {
        vec!["codex_53_fast".to_string()]
    } else {
        Vec::new()
    };

    DispatchRisk {
        task_type,
        risk,
        reasons,
        required_profiles,
        blocked_profiles,
    }
}

fn dispatch_risk_needles() -> &'static [(&'static str, &'static str)] {
    &[
        ("dispatch_profile.rs", "touched_area:dispatch_refactor"),
        ("dispatch_ops", "touched_area:dispatch_refactor"),
        ("dispatch", "touches dispatch routing"),
        ("agent_eval.rs", "touched_area:eval_ledger_changes"),
        ("complete_ops.rs", "touched_area:eval_ledger_changes"),
        ("tachi_complete", "touched_area:eval_ledger_changes"),
        ("aggregate_live", "touched_area:eval_ledger_changes"),
        ("performance_matrix", "touched_area:eval_ledger_changes"),
        ("eval", "touches eval/routing evidence"),
        ("safe_merge", "touches GitHub merge gate"),
        ("gh_safe_merge.rs", "touches GitHub merge gate"),
        ("merge", "touches merge/release gate"),
        ("schema", "touches schema boundary"),
        ("migration", "touches migration behavior"),
        ("vault_ops.rs", "touches vault/secrets boundary"),
        ("credential_profile", "touches vault/secrets boundary"),
        ("vault", "touches vault/secrets boundary"),
        ("secret", "touches vault/secrets boundary"),
        ("api key", "touches vault/secrets boundary"),
        ("sandbox", "touches sandbox boundary"),
        ("profiles.rs", "touches tool surface/profile visibility"),
        ("tool profile", "touches tool surface/profile visibility"),
        ("mcp", "touches MCP/tool boundary"),
    ]
}

fn contains_high_risk_surface(lower: &str) -> bool {
    [
        "dispatch",
        "eval",
        "merge",
        "schema",
        "migration",
        "vault",
        "secret",
        "api key",
        "sandbox",
        "mcp",
        "profiles.rs",
        "tool profile",
        "dispatch profile",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn indicates_missing_verification(lower: &str) -> bool {
    [
        "without tests",
        "without verification",
        "no tests",
        "not tested",
        "untested",
        "skip tests",
        "skipped tests",
        "tests not run",
        "did not run tests",
        "没跑测试",
        "没有测试",
        "没验证",
        "未验证",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn indicates_prior_failure(lower: &str) -> bool {
    [
        "regression",
        "failed before",
        "retry loop",
        "flaky",
        "human override",
        "still failing",
        "keeps failing",
        "still broken",
        "failed again",
        "blocked by failure",
        "stuck in",
        "又坏",
        "回归",
        "失败过",
        "卡住",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn score_profile_candidate(
    profile: &DispatchProfileDef,
    risk: &DispatchRisk,
    rows: &[EvalRow],
    subagent_scores: &[crate::agent_eval::SubagentTaskScore],
    performance_matrix: &[AgentPerformanceMatrixRow],
) -> ProfileCandidate {
    let mut score = 0.0;
    let mut reasons = Vec::new();

    if risk.required_profiles.iter().any(|p| p == profile.name) {
        score += 45.0;
        reasons.push("required_by_risk_classifier".to_string());
    }
    if risk.blocked_profiles.iter().any(|p| p == profile.name) {
        score -= 40.0;
        reasons.push("blocked_or_deprioritized_by_risk_classifier".to_string());
    }

    match risk.task_type.as_str() {
        "review_request" if profile.role.contains("review") => {
            score += 30.0;
            reasons.push("role_matches_review_request".to_string());
        }
        "plan_request" if matches!(profile.role, "planner" | "architect") => {
            score += 30.0;
            reasons.push("role_matches_plan_request".to_string());
        }
        "fix_request" | "refactor_request" | "migration_request" if profile.role == "executor" => {
            score += 25.0;
            reasons.push("role_matches_execution_request".to_string());
        }
        "test_request" if profile.role.contains("checker") || profile.role.contains("reviewer") => {
            score += 20.0;
            reasons.push("role_matches_verification_request".to_string());
        }
        _ => {}
    }

    for signal in &risk.reasons {
        if profile
            .strong_against
            .iter()
            .any(|s| signal.contains(s) || s.contains("dispatch") && signal.contains("dispatch"))
        {
            score += 15.0;
            reasons.push(format!("strong_against_signal:{signal}"));
        }
    }
    if matches!(risk.risk.as_str(), "high" | "critical") && profile.role == "fast_checker" {
        score -= 20.0;
        reasons.push("fast_checker_deprioritized_for_high_risk".to_string());
    }

    let mut live_samples = 0u32;
    let mut useful_sum = 0.0;
    let mut useful_count = 0u32;
    let mut failure_count = 0u32;
    let mut performance_samples = 0u32;
    let mut human_override_sum = 0.0;
    let mut retry_sum = 0.0;
    let mut latency_sum = 0.0;
    let mut latency_count = 0u32;
    let mut cost_sum = 0.0;
    let mut cost_count = 0u32;
    for row in rows {
        if row.profile.as_deref() == Some(profile.name) {
            live_samples += 1;
            if row.completion_status == CompletionStatus::Completed && row.verification_present {
                score += 12.0;
                useful_sum += 1.0;
            } else if row.completion_status == CompletionStatus::Completed {
                score += 5.0;
                useful_sum += 0.6;
            } else {
                score -= 8.0;
                failure_count += 1;
            }
            useful_count += 1;
        }
    }
    for sub in subagent_scores {
        let role_match = sub.role == profile.role
            || (profile.role.contains("review") && sub.role.contains("review"))
            || (profile.role == "architect" && sub.role == "critic");
        let agent_match = sub.agent == profile.backend;
        let task_match = sub.task_type == risk.task_type;
        if role_match && agent_match && task_match {
            live_samples += sub.samples;
            useful_sum += sub.useful_rate * sub.samples as f64;
            useful_count += sub.samples;
            failure_count += sub.failure_count;
            score += sub.useful_rate * 20.0;
            score -= sub.failure_count as f64 * 5.0;
            reasons.push(format!(
                "live_subagent_evidence:{}:{} samples={} useful_rate={:.2}",
                sub.agent, sub.task_type, sub.samples, sub.useful_rate
            ));
        }
    }

    for perf in performance_matrix {
        let profile_match = perf.profile.as_deref() == Some(profile.name);
        let task_match = perf.task_type == risk.task_type;
        if !profile_match || !task_match {
            continue;
        }
        let role_match = perf.scope == "leader"
            || perf.role.as_deref().is_some_and(|role| {
                role == profile.role
                    || (profile.role.contains("review") && role.contains("review"))
                    || (profile.role == "architect" && role == "critic")
                    || (profile.role == "executor" && role == "implementer")
            });
        if !role_match {
            continue;
        }

        performance_samples += perf.samples;
        let weight = (perf.samples as f64).min(20.0) / 20.0;

        if perf.failure_count > 0 {
            failure_count += perf.failure_count;
            let penalty = (perf.failure_count as f64 * 4.0).min(16.0) * weight;
            score -= penalty;
            reasons.push(format!(
                "perf_failure_count:{}:{} failures={}",
                perf.scope, perf.task_type, perf.failure_count
            ));
        }
        if perf.human_override_rate > 0.0 {
            human_override_sum += perf.human_override_rate * perf.samples as f64;
            let penalty = perf.human_override_rate * 18.0 * weight;
            score -= penalty;
            reasons.push(format!(
                "perf_human_override_rate={:.2}",
                perf.human_override_rate
            ));
        }
        if perf.avg_retry_count > 0.0 {
            retry_sum += perf.avg_retry_count * perf.samples as f64;
            let penalty = (perf.avg_retry_count * 3.0).min(15.0) * weight;
            score -= penalty;
            reasons.push(format!("perf_avg_retry_count={:.2}", perf.avg_retry_count));
        }
        if let Some(latency) = perf.avg_latency_ms {
            latency_sum += latency * perf.samples as f64;
            latency_count += perf.samples;
            if latency > 600_000.0 {
                score -= 4.0 * weight;
                reasons.push(format!("perf_slow_avg_latency_ms={latency:.0}"));
            }
        }
        if let Some(cost) = perf.avg_cost_usd {
            cost_sum += cost * perf.samples as f64;
            cost_count += perf.samples;
            if cost > 1.0 {
                score -= 4.0 * weight;
                reasons.push(format!("perf_high_avg_cost_usd={cost:.4}"));
            } else if perf.avg_quality_score.unwrap_or(0.0) >= 0.8 && cost <= 0.10 {
                score += 3.0 * weight;
                reasons.push(format!("perf_cost_efficient_usd={cost:.4}"));
            }
        }
    }

    let useful_rate = (useful_count > 0).then(|| useful_sum / useful_count as f64);
    if let Some(rate) = useful_rate {
        reasons.push(format!("live_useful_rate={rate:.2}"));
    }
    if reasons.is_empty() {
        reasons.push("baseline_mbit_fit".to_string());
    }

    ProfileCandidate {
        profile: profile.name.to_string(),
        agent: profile.backend.to_string(),
        role: profile.role.to_string(),
        score: (score * 100.0).round() / 100.0,
        reasons,
        live_samples,
        useful_rate,
        failure_count,
        performance_samples,
        human_override_rate: (performance_samples > 0)
            .then(|| human_override_sum / performance_samples as f64),
        avg_retry_count: (performance_samples > 0).then(|| retry_sum / performance_samples as f64),
        avg_latency_ms: (latency_count > 0).then(|| latency_sum / latency_count as f64),
        avg_cost_usd: (cost_count > 0).then(|| cost_sum / cost_count as f64),
    }
}

fn build_profile_fallback_chain(
    primary: &DispatchProfileDef,
    candidates: &[ProfileCandidate],
) -> Vec<String> {
    let mut out = vec![primary.name.to_string()];
    for candidate in candidates.iter().skip(1) {
        if out.len() >= 4 {
            break;
        }
        if !out.iter().any(|p| p == &candidate.profile) {
            out.push(candidate.profile.clone());
        }
    }
    for agent in fallback_chain(primary.backend) {
        if out.len() >= 5 {
            break;
        }
        if let Some(profile) = DISPATCH_PROFILES
            .iter()
            .find(|profile| profile.backend == *agent && !out.iter().any(|p| p == profile.name))
        {
            out.push(profile.name.to_string());
        }
    }
    out
}

fn simulate_route_policy(
    policy: &str,
    performance_matrix: &[AgentPerformanceMatrixRow],
    focus: Option<&DispatchRisk>,
) -> RouteSimulationSummary {
    let profile_names = DISPATCH_PROFILES
        .iter()
        .map(|profile| profile.name)
        .collect::<Vec<_>>();
    let mut by_task: HashMap<String, Vec<&AgentPerformanceMatrixRow>> = HashMap::new();
    for row in performance_matrix {
        if row.scope != "leader" {
            continue;
        }
        let Some(profile) = row.profile.as_deref() else {
            continue;
        };
        if !profile_names.contains(&profile) {
            continue;
        }
        if let Some(focus) = focus {
            if row.task_type != focus.task_type {
                continue;
            }
        }
        by_task.entry(row.task_type.clone()).or_default().push(row);
    }

    let mut choices = Vec::new();
    for (task_type, rows) in by_task {
        let mut scored = rows
            .into_iter()
            .filter_map(|row| {
                row.profile.as_deref()?;
                let score = route_policy_score(policy, row, focus);
                Some((row, score, route_policy_reasons(policy, row, focus)))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|(a, a_score, _), (b, b_score, _)| {
            b_score
                .partial_cmp(a_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.profile
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.profile.as_deref().unwrap_or(""))
                })
        });
        if let Some((row, score, reasons)) = scored.first() {
            choices.push(RouteSimulationChoice {
                task_type,
                profile: row.profile.clone().unwrap_or_default(),
                agent: row.agent.clone(),
                samples: row.samples,
                score: round2(*score),
                success_rate: row.success_rate,
                verification_rate: row.verification_rate,
                failure_count: row.failure_count,
                avg_latency_ms: row.avg_latency_ms,
                avg_cost_usd: row.avg_cost_usd,
                avg_retry_count: row.avg_retry_count,
                human_override_rate: row.human_override_rate,
                reasons: reasons.clone(),
            });
        }
    }
    choices.sort_by(|a, b| a.task_type.cmp(&b.task_type));

    summarize_route_simulation(policy, choices, focus)
}

fn build_route_policy_proposals(
    current: &RouteSimulationSummary,
    variants: &[RouteSimulationSummary],
    row_count: usize,
    limit: usize,
) -> Vec<Value> {
    let current_by_task = current
        .route_choices
        .iter()
        .map(|choice| (choice.task_type.as_str(), choice))
        .collect::<HashMap<_, _>>();
    let mut out = Vec::new();
    for variant in variants {
        for choice in &variant.route_choices {
            let Some(current_choice) = current_by_task.get(choice.task_type.as_str()) else {
                continue;
            };
            if current_choice.profile == choice.profile {
                continue;
            }
            let id = format!(
                "route_policy:{}:{}:{}",
                sanitize_policy_key(&variant.policy),
                sanitize_policy_key(&choice.task_type),
                sanitize_policy_key(&choice.profile)
            );
            out.push(json!({
                "proposal_id": id,
                "kind": "route_policy",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "policy": variant.policy,
                "task_type": choice.task_type,
                "current_profile": current_choice.profile,
                "proposed_profile": choice.profile,
                "current_score": current_choice.score,
                "proposed_score": choice.score,
                "score_delta": round2(choice.score - current_choice.score),
                "policy_rule": {
                    "when_task_type": choice.task_type,
                    "prefer_profile": choice.profile,
                    "policy": variant.policy,
                    "fallback_to_current_profile": current_choice.profile,
                },
                "evidence": {
                    "source": "live_memory_eval",
                    "row_count": row_count,
                    "limit": limit,
                    "current": current_choice,
                    "proposed": choice,
                    "route_simulate_call": "tachi_task(action='route_simulate', limit=...)",
                },
                "rationale": format!(
                    "{} replay prefers {} over current {} for {}",
                    variant.policy, choice.profile, current_choice.profile, choice.task_type
                ),
            }));
        }
    }
    out
}

#[derive(Default)]
struct LoadoutSkillEvidence {
    hits: u32,
    verified: u32,
    success: u32,
    quality_sum: f64,
    quality_count: u32,
    task_types: HashMap<String, u32>,
    eval_refs: Vec<String>,
}

fn build_loadout_evolution_proposals(
    server: &MemoryServer,
    performance_matrix: &[AgentPerformanceMatrixRow],
    limit: usize,
) -> Result<Vec<Value>, String> {
    let entries = load_live_eval_entries(server, limit)?;
    let mut out = Vec::new();

    for profile in DISPATCH_PROFILES {
        let profile_rows = performance_matrix
            .iter()
            .filter(|row| row.profile.as_deref() == Some(profile.name))
            .cloned()
            .collect::<Vec<_>>();
        let profile_samples = sum_matrix_samples(&profile_rows);
        if profile_samples < MIN_LOADOUT_EVOLUTION_SAMPLES {
            continue;
        }
        let failure_count = sum_matrix_failures(&profile_rows);
        let human_override_rate =
            weighted_matrix_rate(&profile_rows, |row| Some(row.human_override_rate)).unwrap_or(0.0);
        let avg_retry_count =
            weighted_matrix_rate(&profile_rows, |row| Some(row.avg_retry_count)).unwrap_or(0.0);
        let success_rate =
            weighted_matrix_rate(&profile_rows, |row| row.success_rate).unwrap_or(0.0);
        let useful_rate = weighted_matrix_rate(&profile_rows, |row| row.useful_rate).unwrap_or(0.0);
        let positive_rate = success_rate.max(useful_rate);

        if failure_count > 0
            || human_override_rate >= 0.10
            || avg_retry_count >= 1.0
            || positive_rate < 0.80
        {
            continue;
        }

        let existing_skills = profile_required_skill_ids_for_server(server, profile)?
            .into_iter()
            .chain(
                profile
                    .forbidden_skills
                    .iter()
                    .map(|skill| skill.to_string()),
            )
            .collect::<HashSet<_>>();
        let mut buckets: HashMap<String, LoadoutSkillEvidence> = HashMap::new();
        for entry in &entries {
            let Some(meta) = entry.metadata.as_object() else {
                continue;
            };
            if meta.get("profile").and_then(Value::as_str) != Some(profile.name) {
                continue;
            }
            let task_type = meta
                .get("task_type")
                .and_then(Value::as_str)
                .unwrap_or("other")
                .to_string();
            let outcome = meta
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let verified = meta
                .get("verification_present")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let quality = meta.get("quality_score").and_then(Value::as_f64);
            let skills = meta
                .get("skills_used")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|skill| !skill.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();

            for skill in skills {
                if existing_skills.contains(&skill) {
                    continue;
                }
                let evidence = buckets.entry(skill).or_default();
                evidence.hits += 1;
                if verified {
                    evidence.verified += 1;
                }
                if matches!(outcome.as_str(), "success" | "completed") {
                    evidence.success += 1;
                }
                if let Some(quality) = quality {
                    evidence.quality_sum += quality;
                    evidence.quality_count += 1;
                }
                *evidence.task_types.entry(task_type.clone()).or_insert(0) += 1;
                if evidence.eval_refs.len() < 5 {
                    evidence.eval_refs.push(entry.path.clone());
                }
            }
        }

        let min_skill_hits = (profile_samples / 2).max(3);
        for (skill, evidence) in buckets {
            if evidence.hits < min_skill_hits {
                continue;
            }
            let verified_rate = evidence.verified as f64 / evidence.hits as f64;
            let success_rate = evidence.success as f64 / evidence.hits as f64;
            if verified_rate < 0.50 || success_rate < 0.80 {
                continue;
            }
            let avg_quality = (evidence.quality_count > 0)
                .then(|| evidence.quality_sum / evidence.quality_count as f64);
            let id = format!(
                "loadout_evolution:{}:promote_signature:{}",
                sanitize_policy_key(profile.name),
                sanitize_policy_key(&skill)
            );
            out.push(json!({
                "proposal_id": id,
                "kind": "loadout_evolution",
                "status": "pending",
                "requires_human_approval": true,
                "created_or_refreshed_at": Utc::now().to_rfc3339(),
                "profile": profile.name,
                "operation": "promote_observed_skill_to_signature",
                "skill_id": skill.clone(),
                "current_loadout": profile_skill_loadout_json(profile),
                "proposed_patch": {
                    "add_signature_skills": [skill.clone()],
                    "preserve_common_skills": profile.common_skills,
                    "preserve_forbidden_skills": profile.forbidden_skills,
                },
                "evidence": {
                    "source": "live_memory_eval",
                    "limit": limit,
                    "profile_samples": profile_samples,
                    "min_samples_for_evolution": MIN_LOADOUT_EVOLUTION_SAMPLES,
                    "min_skill_hits": min_skill_hits,
                    "skill_hits": evidence.hits,
                    "verified_rate": round2(verified_rate),
                    "success_rate": round2(success_rate),
                    "avg_quality_score": avg_quality.map(round2),
                    "profile_summary": summarize_matrix_rows(&profile_rows),
                    "task_types": evidence.task_types,
                    "eval_refs": evidence.eval_refs,
                    "loadout_call": "tachi_skill(action='loadout', profile=..., limit=...)",
                },
                "rationale": format!(
                    "{} appeared in {}/{} verified successful {} runs and is not part of the current sparse loadout",
                    skill, evidence.hits, profile_samples, profile.name
                ),
                "projection": {
                    "status": "pending_profile_card_projection",
                    "note": "Human approval records the proposal; mutating built-in profile/card definitions is handled by the next MBIT projection slice."
                }
            }));
        }
    }

    Ok(out)
}

fn load_live_eval_entries(
    server: &MemoryServer,
    limit: usize,
) -> Result<Vec<memory_core::MemoryEntry>, String> {
    let limit = limit.max(1);
    let mut entries = server.with_global_store_read(|store| {
        store
            .list_by_path("/eval", limit, false)
            .map_err(|e| format!("list global eval entries: {e}"))
    })?;
    if server.has_project_db() {
        let mut project_entries = server.with_project_store_read(|store| {
            store
                .list_by_path("/eval", limit, false)
                .map_err(|e| format!("list project eval entries: {e}"))
        })?;
        entries.append(&mut project_entries);
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit);
    Ok(entries)
}

fn sanitize_policy_key(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    sanitized.trim_matches('-').to_string()
}

fn route_policy_score(
    policy: &str,
    row: &AgentPerformanceMatrixRow,
    focus: Option<&DispatchRisk>,
) -> f64 {
    let success = row.success_rate.unwrap_or(0.0);
    let quality = row.avg_quality_score.unwrap_or(success);
    let verification = row.verification_rate;
    let cost = row.avg_cost_usd.unwrap_or(0.0);
    let latency_minutes = row.avg_latency_ms.unwrap_or(0.0) / 60_000.0;
    let retry = row.avg_retry_count;
    let override_rate = row.human_override_rate;
    let failure_rate = if row.samples > 0 {
        row.failure_count as f64 / row.samples as f64
    } else {
        0.0
    };
    let mut score = match policy {
        "cost_sensitive" => {
            success * 45.0 + verification * 15.0 + quality * 10.0
                - cost * 35.0
                - latency_minutes * 1.5
                - retry * 10.0
                - override_rate * 20.0
                - failure_rate * 30.0
        }
        "quality_first" => {
            success * 55.0 + quality * 35.0 + verification * 20.0
                - failure_rate * 45.0
                - override_rate * 12.0
                - retry * 6.0
                - cost * 6.0
                - latency_minutes * 0.5
        }
        _ => {
            success * 45.0 + quality * 25.0 + verification * 18.0
                - failure_rate * 35.0
                - override_rate * 18.0
                - retry * 8.0
                - cost * 10.0
                - latency_minutes
        }
    };

    if let (Some(focus), Some(profile)) = (focus, row.profile.as_deref()) {
        if focus.required_profiles.iter().any(|p| p == profile) {
            score += 20.0;
        }
        if focus.blocked_profiles.iter().any(|p| p == profile) {
            score -= 30.0;
        }
    }
    score
}

fn route_policy_reasons(
    policy: &str,
    row: &AgentPerformanceMatrixRow,
    focus: Option<&DispatchRisk>,
) -> Vec<String> {
    let mut reasons = vec![
        format!(
            "{} samples success={:.2}",
            row.samples,
            row.success_rate.unwrap_or(0.0)
        ),
        format!("verification={:.2}", row.verification_rate),
    ];
    if row.failure_count > 0 {
        reasons.push(format!("failures={}", row.failure_count));
    }
    if row.avg_cost_usd.is_some() {
        reasons.push(format!(
            "avg_cost_usd={:.4}",
            row.avg_cost_usd.unwrap_or_default()
        ));
    }
    if row.avg_latency_ms.is_some() {
        reasons.push(format!(
            "avg_latency_ms={:.0}",
            row.avg_latency_ms.unwrap_or_default()
        ));
    }
    match policy {
        "cost_sensitive" => reasons.push("policy_prioritizes_cost_and_latency".to_string()),
        "quality_first" => {
            reasons.push("policy_prioritizes_success_quality_verification".to_string())
        }
        _ => reasons.push("policy_balances_quality_cost_and_failures".to_string()),
    }
    if let (Some(focus), Some(profile)) = (focus, row.profile.as_deref()) {
        if focus.required_profiles.iter().any(|p| p == profile) {
            reasons.push("focus_task_required_profile_bonus".to_string());
        }
        if focus.blocked_profiles.iter().any(|p| p == profile) {
            reasons.push("focus_task_blocked_profile_penalty".to_string());
        }
    }
    reasons
}

fn summarize_route_simulation(
    policy: &str,
    choices: Vec<RouteSimulationChoice>,
    focus: Option<&DispatchRisk>,
) -> RouteSimulationSummary {
    let sample_count = choices.iter().map(|choice| choice.samples).sum::<u32>();
    let sample_count_f = sample_count as f64;
    let mut success_sum = 0.0;
    let mut success_samples = 0u32;
    let mut verification_sum = 0.0;
    let mut failures = 0u32;
    let mut retry_sum = 0.0;
    let mut override_sum = 0.0;
    let mut latency_sum = 0.0;
    let mut latency_samples = 0u32;
    let mut cost_sum = 0.0;
    let mut cost_samples = 0u32;
    let mut score_sum = 0.0;

    for choice in &choices {
        if let Some(success) = choice.success_rate {
            success_sum += success * choice.samples as f64;
            success_samples += choice.samples;
        }
        verification_sum += choice.verification_rate * choice.samples as f64;
        failures += choice.failure_count;
        if let Some(latency) = choice.avg_latency_ms {
            latency_sum += latency * choice.samples as f64;
            latency_samples += choice.samples;
        }
        if let Some(cost) = choice.avg_cost_usd {
            cost_sum += cost * choice.samples as f64;
            cost_samples += choice.samples;
        }
        retry_sum += choice.avg_retry_count * choice.samples as f64;
        override_sum += choice.human_override_rate * choice.samples as f64;
        score_sum += choice.score * choice.samples as f64;
    }

    let mut caveats = Vec::new();
    if sample_count == 0 {
        caveats.push(
            "no matching leader/profile eval rows; policy comparison is evidence-empty".to_string(),
        );
    }
    if focus.is_some() && choices.is_empty() {
        caveats.push("focus task type has no matching live eval rows".to_string());
    }
    if sample_count < 10 && sample_count > 0 {
        caveats.push("low sample count; treat as directional, not learned policy".to_string());
    }

    RouteSimulationSummary {
        policy: policy.to_string(),
        selected_route_count: choices.len() as u32,
        sample_count,
        estimated_success_rate: (success_samples > 0)
            .then(|| round4(success_sum / success_samples as f64)),
        estimated_verification_rate: (sample_count > 0)
            .then(|| round4(verification_sum / sample_count_f)),
        failure_count: failures,
        avg_retry_count: (sample_count > 0).then(|| round4(retry_sum / sample_count_f)),
        avg_human_override_rate: (sample_count > 0).then(|| round4(override_sum / sample_count_f)),
        avg_latency_ms: (latency_samples > 0).then(|| round2(latency_sum / latency_samples as f64)),
        avg_cost_usd: (cost_samples > 0).then(|| round4(cost_sum / cost_samples as f64)),
        total_cost_usd: (cost_samples > 0).then(|| round4(cost_sum)),
        score: if sample_count > 0 {
            round2(score_sum / sample_count_f)
        } else {
            0.0
        },
        route_choices: choices,
        caveats,
    }
}

fn route_simulation_caveats(
    rows: &[EvalRow],
    performance_matrix: &[AgentPerformanceMatrixRow],
) -> Vec<String> {
    let mut caveats = vec![
        "simulation is replay-only and does not mutate routing policy".to_string(),
        "raw child transcripts are not loaded; only compact /eval evidence is used".to_string(),
    ];
    if rows.is_empty() {
        caveats.push(
            "no /eval rows found; recommendations must fall back to deterministic MBIT/risk fit"
                .to_string(),
        );
    }
    let leader_profile_rows = performance_matrix
        .iter()
        .filter(|row| row.scope == "leader" && row.profile.is_some())
        .count();
    if leader_profile_rows == 0 && !rows.is_empty() {
        caveats.push("live eval rows exist but none have leader profile ids; record profile during completion for policy replay".to_string());
    }
    caveats
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> TachiDispatchParams {
        TachiDispatchParams {
            agent: None,
            profile: Some("claude_plan".to_string()),
            task: "Plan issue #194".to_string(),
            cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            project: None,
            stage: None,
            credential_profiles: Vec::new(),
            issue_ref: Some("kckylechen1/tachi#194".to_string()),
            pr_ref: None,
            flow_id: Some("flow-194".to_string()),
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn dispatch_profile_selects_backend_and_mcp_contract() {
        let mut params = params();
        let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
        assert_eq!(params.agent.as_deref(), Some("claude"));
        assert_eq!(params.stage.as_deref(), Some("plan"));
        assert_eq!(params.tool_profile.as_deref(), Some("delegate"));
        assert_eq!(params.inject_tachi_mcp, Some(true));
        assert_eq!(resolved.selected_profile.as_deref(), Some("claude_plan"));
        assert_eq!(resolved.mcp_access.github_read, Some(true));
        assert_eq!(
            resolved.mcp_access.issue_refs,
            vec!["kckylechen1/tachi#194".to_string()]
        );
        assert!(resolved.auto_capability_bundle);
        assert!(params
            .skills
            .iter()
            .any(|skill| skill == SUPERPOWER_WRITING_PLANS));
        assert!(params
            .skills
            .iter()
            .any(|skill| skill == CODING_ARCHITECTURE_DECISION));
        assert_eq!(
            profile_skill_loadout_json(resolve_dispatch_profile("claude_plan").unwrap())
                ["passive_traits"][0],
            json!("plan_before_execute")
        );
    }

    #[test]
    fn dispatch_profile_treats_blank_agent_as_missing() {
        let mut params = params();
        params.agent = Some("   ".to_string());
        let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
        assert_eq!(params.agent.as_deref(), Some("claude"));
        assert_eq!(resolved.agent, "claude");
    }

    #[test]
    fn credentialed_dispatch_profile_applies_default_credential_profiles() {
        let mut params = params();
        params.profile = Some("opencode_builder".to_string());
        params.issue_ref = None;
        let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
        assert_eq!(params.agent.as_deref(), Some("custom"));
        assert_eq!(params.stage.as_deref(), Some("execute"));
        assert_eq!(params.credential_profiles, vec!["opencode_shared"]);
        assert_eq!(
            resolved.credential_profiles,
            vec!["opencode_shared".to_string()]
        );
        assert_eq!(
            profile_json(resolve_dispatch_profile("opencode_builder").unwrap())
                ["credential_profiles"][0],
            json!("opencode_shared")
        );
    }

    #[test]
    fn dispatch_profile_merges_default_and_explicit_credential_profiles() {
        let mut params = params();
        params.profile = Some("opencode_builder".to_string());
        params.credential_profiles = vec!["extra_project_secret".to_string()];
        let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
        assert_eq!(
            params.credential_profiles,
            vec!["extra_project_secret", "opencode_shared"]
        );
        assert_eq!(
            resolved.credential_profiles,
            vec![
                "extra_project_secret".to_string(),
                "opencode_shared".to_string()
            ]
        );
        assert!(resolved
            .route_explanation
            .iter()
            .any(|line| line.contains("profile requires credential profile(s): opencode_shared")));
    }

    #[test]
    fn dispatch_profile_writes_fallback_mcp_access_back_to_params() {
        let mut params = params();
        params.profile = None;
        params.agent = Some("claude".to_string());
        params.inject_tachi_mcp = Some(true);
        params.allowed_mcp_servers = vec!["github".to_string()];
        let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

        assert_eq!(
            params
                .mcp_access
                .as_ref()
                .map(|access| access.allowed_mcp_servers.as_slice()),
            Some(&["github".to_string()][..])
        );
        assert_eq!(
            resolved.mcp_access.allowed_mcp_servers,
            vec!["github".to_string()]
        );
    }

    #[test]
    fn explicit_agent_can_override_profile_backend() {
        let mut params = params();
        params.agent = Some("codex".to_string());
        let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
        assert_eq!(resolved.agent, "codex");
        assert!(resolved
            .route_explanation
            .iter()
            .any(|line| line.contains("overrides profile backend")));
    }

    #[test]
    fn custom_profile_populates_opencode_command() {
        let mut params = params();
        params.profile = Some("deepseek_explore".to_string());
        let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

        assert_eq!(resolved.agent, "custom");
        assert_eq!(
            params.command,
            vec![
                "opencode".to_string(),
                "--pure".to_string(),
                "run".to_string(),
                "--model".to_string(),
                "deepseek/deepseek-v4-flash".to_string()
            ]
        );
        assert!(resolved
            .route_explanation
            .iter()
            .any(|line| line.contains("opencode custom command")));
    }

    #[test]
    fn risk_classifier_uses_touched_area_and_missing_verification_signals() {
        let risk = classify_dispatch_risk(
            "review changes in crates/memory-server/src/agent_eval.rs and dispatch_profile.rs; tests not run",
            None,
            &[],
        );

        assert_eq!(risk.risk, "high");
        assert!(risk
            .reasons
            .iter()
            .any(|reason| reason == "touched_area:eval_ledger_changes"));
        assert!(risk
            .reasons
            .iter()
            .any(|reason| reason == "touched_area:dispatch_refactor"));
        assert!(risk
            .reasons
            .iter()
            .any(|reason| reason == "missing_verification_signal"));
        assert!(risk
            .required_profiles
            .iter()
            .any(|profile| profile == "codex_55_review"));
        assert!(risk
            .blocked_profiles
            .iter()
            .any(|profile| profile == "codex_53_fast"));
    }

    #[test]
    fn risk_classifier_preserves_low_risk_research_route() {
        let risk = classify_dispatch_risk("research low-risk documentation wording", None, &[]);

        assert_eq!(risk.risk, "low");
        assert!(risk.blocked_profiles.is_empty());
    }

    #[test]
    fn risk_classifier_marks_regression_hints_high() {
        let risk = classify_dispatch_risk(
            "fix a regression where the worker got stuck in a retry loop",
            None,
            &[],
        );

        assert_eq!(risk.risk, "high");
        assert!(risk
            .reasons
            .iter()
            .any(|reason| reason == "prior_failure_or_regression_hint"));
    }

    #[test]
    fn risk_classifier_does_not_treat_plain_override_or_profile_as_failure() {
        let override_risk = classify_dispatch_risk(
            "document the config override behavior for normal settings",
            None,
            &[],
        );
        assert_ne!(override_risk.risk, "high");
        assert!(!override_risk
            .reasons
            .iter()
            .any(|reason| reason == "prior_failure_or_regression_hint"));

        let profile_risk = classify_dispatch_risk("review user profile page wording", None, &[]);
        assert_ne!(profile_risk.risk, "high");
    }

    #[test]
    fn risk_classifier_escalates_on_sensitive_file_paths() {
        let paths = vec![
            "docs/notes.md".to_string(),
            "crates/memory-server/src/vault_crypto.rs".to_string(),
        ];
        let risk = classify_dispatch_risk("plan a small docs update", None, &paths);

        assert_eq!(risk.risk, "high");
        assert!(risk
            .reasons
            .iter()
            .any(|reason| reason == "touches vault/secrets boundary"));
    }

    #[test]
    fn risk_classifier_dedupes_text_and_path_signals() {
        let paths = vec!["crates/memory-server/src/dispatch_profile.rs".to_string()];
        let risk = classify_dispatch_risk("review dispatch profile changes", None, &paths);
        let count = risk
            .reasons
            .iter()
            .filter(|reason| *reason == "touches dispatch routing")
            .count();

        assert_eq!(risk.risk, "high");
        assert_eq!(count, 1);
    }
}
