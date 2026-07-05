//! Dispatch profiles describe who to ask, which context/tools to expose, and
//! what evidence a delegated agent must return. They are intentionally separate
//! from `tachi_hub::ToolProfile`, which only gates MCP tool visibility.

use memory_server_params::{DispatchMcpAccessParams, TachiDispatchParams};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;

pub const DISPATCH_POLICY_PROPOSAL_NS: &str = "dispatch_route_policy_proposals";
pub const ROUTE_POLICY_RULE_NS: &str = "dispatch_route_policy_rules";
pub const PROFILE_CARD_OVERLAY_NS: &str = "dispatch_profile_card_overlays";
pub const MIN_ROUTE_POLICY_RULE_SAMPLES: u32 = 2;
pub const MIN_LOADOUT_EVOLUTION_SAMPLES: u32 = 10;
pub const MIN_CARD_RISK_EVOLUTION_SAMPLES: u32 = 3;
pub const ROUTE_POLICY_RULE_SCORE_BONUS: f64 = 35.0;

pub const SUPERPOWER_WRITING_PLANS: &str = "skill:superpowers-writing-plans";
pub const SUPERPOWER_EXECUTING_PLANS: &str = "skill:superpowers-executing-plans";
pub const SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT: &str =
    "skill:superpowers-subagent-driven-development";
pub const SUPERPOWER_REQUESTING_CODE_REVIEW: &str = "skill:superpowers-requesting-code-review";
pub const SUPERPOWER_VERIFICATION_BEFORE_COMPLETION: &str =
    "skill:superpowers-verification-before-completion";

pub const WAZA_CHECK: &str = "skill:waza-check";
pub const WAZA_LEARN: &str = "skill:waza-learn";
pub const WAZA_READ: &str = "skill:waza-read";
pub const WAZA_TACHI: &str = "skill:waza-tachi";
pub const WAZA_THINK: &str = "skill:waza-think";

pub const CODING_REFACTOR_CHECKLIST: &str = "skill:coding-refactor-checklist";
pub const CODING_TEST_STRATEGY: &str = "skill:coding-test-strategy";
pub const CODING_ARCHITECTURE_DECISION: &str = "skill:coding-architecture-decision";

#[derive(Debug, Clone, Copy)]
pub struct DispatchProfileDef {
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

pub const DISPATCH_PROFILES: &[DispatchProfileDef] = &[
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
}

pub fn resolve_dispatch_profile(raw: &str) -> Option<&'static DispatchProfileDef> {
    let norm = raw.trim().to_ascii_lowercase();
    DISPATCH_PROFILES
        .iter()
        .find(|profile| profile.name == norm)
}

pub fn profile_host_adapter(profile: &DispatchProfileDef) -> Option<&'static str> {
    match profile.backend {
        "opencode" => Some("opencode"),
        // Compatibility path: older custom profiles with a model but no command
        // are still materialized as OpenCode CLI/serve commands by routing.
        "custom" if profile.model.is_some() => Some("opencode"),
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
        host_adapter: profile.and_then(profile_host_adapter).map(str::to_string),
        mbit_card: profile.map(profile_mbit_card).transpose()?,
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
    let model = profile
        .model
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
                model.to_string(),
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
                model.to_string(),
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
            model.to_string(),
        ];
        route_explanation.push(format!(
            "profile selected typed OpenCode CLI transport for model '{}'",
            model
        ));
    }
    Ok(())
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
        "glm_51_impl" => (78, 76, 52, 70, 72),
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
