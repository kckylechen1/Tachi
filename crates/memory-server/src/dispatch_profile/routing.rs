use super::*;

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
            score_profile_candidate(
                server,
                profile,
                &risk,
                &rows,
                &subagent_scores,
                &performance_matrix,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    apply_route_policy_rules_to_candidates(&mut candidates, &route_policy_rules, &risk);
    candidates
        .sort_by(|a, b| compare_scores_desc(a.score, b.score).then(a.profile.cmp(&b.profile)));

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
    let (recommended_transport, transport_readiness) =
        recommended_transport_for_profile(best_profile);

    serde_json::to_string(&json!({
        "task": task,
        "task_type": risk.task_type,
        "risk": risk.risk,
        "risk_reasons": risk.reasons,
        "required_profiles": risk.required_profiles,
        "blocked_profiles": risk.blocked_profiles,
        "recommended_profile": best.profile,
        "recommended_agent": best.agent,
        "recommended_model": best_profile.model,
        "recommended_transport": recommended_transport,
        "transport_readiness": transport_readiness,
        "role": best.role,
        "tool_profile": best_profile.tool_profile,
        "evidence_required": profile_evidence_required_for_server(server, best_profile)?,
        "evidence_contract": profile_evidence_contract_json_for_server(server, best_profile)?,
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

pub(super) fn recommended_transport_for_profile(profile: &DispatchProfileDef) -> (String, Value) {
    if profile.backend != "custom" {
        return (
            "native_cli".to_string(),
            json!({ "requested": "native_cli", "readiness": "not_applicable" }),
        );
    }

    let requested = std::env::var("TACHI_OPENCODE_TRANSPORT")
        .unwrap_or_else(|_| "cli".to_string())
        .to_ascii_lowercase();
    if matches!(requested.as_str(), "serve" | "opencode_serve" | "server") {
        let server_url = std::env::var("TACHI_OPENCODE_SERVER_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4321".to_string());
        let status = crate::dispatch_ops::probe_harness_server_status(Some(&server_url));
        let attach_ready = status
            .get("attach_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let transport = if attach_ready {
            "opencode_serve"
        } else {
            "opencode_cli"
        };
        return (
            transport.to_string(),
            json!({
                "requested": "opencode_serve",
                "server_url": server_url,
                "fallback": if attach_ready { Value::Null } else { json!("opencode_cli") },
                "harness_server_status": status,
            }),
        );
    }

    (
        "opencode_cli".to_string(),
        json!({ "requested": "opencode_cli", "readiness": "cli" }),
    )
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

pub(super) fn resolve_and_apply_dispatch_profile_inner(
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
                    if crate::dispatch_ops::harness_server_attach_ready(&server_url) {
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
                            "profile selected opencode serve transport for model '{}'",
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
    let evidence_required = match (server, profile) {
        (Some(server), Some(profile)) => profile_evidence_required_for_server(server, profile)?,
        (_, Some(profile)) => profile_evidence_required(profile),
        _ => Vec::new(),
    };
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

pub(super) fn classify_dispatch_risk(
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

pub(super) fn dispatch_risk_needles() -> &'static [(&'static str, &'static str)] {
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
        ("ux_matrix", "touches workflow_ux"),
        ("agent-facing ux", "touches agent_experience"),
        ("agent facing ux", "touches agent_experience"),
        ("user experience", "touches agent_experience"),
        ("tool surface", "touches tool_surface_friction"),
        ("体验", "touches agent_experience"),
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

pub(super) fn contains_high_risk_surface(lower: &str) -> bool {
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

pub(super) fn indicates_missing_verification(lower: &str) -> bool {
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

pub(super) fn indicates_prior_failure(lower: &str) -> bool {
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

pub(super) fn score_profile_candidate(
    server: &MemoryServer,
    profile: &DispatchProfileDef,
    risk: &DispatchRisk,
    rows: &[EvalRow],
    subagent_scores: &[crate::agent_eval::SubagentTaskScore],
    performance_matrix: &[AgentPerformanceMatrixRow],
) -> Result<ProfileCandidate, String> {
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
        "research_request" | "explain_request"
            if matches!(
                profile.role,
                "explore" | "planner" | "architect" | "ux_researcher"
            ) =>
        {
            score += 30.0;
            reasons.push("role_matches_read_only_request".to_string());
        }
        "research_request" | "explain_request" if profile.role == "executor" => {
            // A read-only research/explain task produces no diff/tests_run/files_changed,
            // so an executor's evidence contract is structurally unsatisfiable here.
            // Deprioritize write-executors for read-only work so role/task fit, not
            // sparse eval history, decides the route.
            score -= 20.0;
            reasons.push("executor_deprioritized_for_read_only_request".to_string());
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
    if profile.role == "ux_researcher"
        && risk.reasons.iter().any(|reason| {
            reason.contains("agent_experience")
                || reason.contains("workflow_ux")
                || reason.contains("tool_surface_friction")
        })
    {
        score += 30.0;
        reasons.push("role_matches_agent_facing_ux".to_string());
    }
    let weak_against = profile_weak_against_for_server(server, profile)?;
    for weakness in &weak_against {
        if weakness == &risk.task_type
            || risk
                .reasons
                .iter()
                .any(|signal| signal.contains(weakness) || weakness.contains(signal))
        {
            score -= 12.0;
            reasons.push(format!("weak_against_signal:{weakness}"));
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
    // The performance matrix already aggregates a `leader`-scope bucket per
    // profile+task_type from these same eval rows, and the perf-matrix loop
    // below penalizes that bucket's *failures* — it never rewards successes.
    // So only the raw-row *failure* penalty can double-count against the
    // perf-matrix leader path; that one is gated under `!has_perf_leader_row`
    // below. The success rewards are applied unconditionally because the
    // perf-matrix path scores no successes — gating them would leave a leader
    // profile with perf data penalized for failures but never credited for
    // wins. Sample bookkeeping (live_samples/useful_rate/failure_count) is
    // always accumulated so the candidate's reported stats stay complete.
    let has_perf_leader_row = performance_matrix.iter().any(|perf| {
        perf.scope == "leader"
            && perf.profile.as_deref() == Some(profile.name)
            && perf.task_type == risk.task_type
    });
    let mut raw_failure_count = 0u32;
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
                raw_failure_count += 1;
                failure_count += 1;
            }
            useful_count += 1;
        }
    }
    if !has_perf_leader_row && raw_failure_count > 0 {
        // Sample-weight and clamp the aggregate failure contribution to mirror
        // the perf-matrix path so sparse failures can no longer alone push the
        // score down without bound (previously a bare -8.0 per failed row that
        // could remove -24 unweighted and unclamped).
        let weight = (raw_failure_count as f64).min(20.0) / 20.0;
        let raw_failure_penalty = (raw_failure_count as f64 * 4.0).min(16.0) * weight;
        score -= raw_failure_penalty;
        reasons.push(format!("live_eval_failures={raw_failure_count}"));
    }
    for sub in subagent_scores {
        let role_match = profile_role_matches(profile, &sub.role);
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
            || perf
                .role
                .as_deref()
                .is_some_and(|role| profile_role_matches(profile, role));
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

    Ok(ProfileCandidate {
        profile: profile.name.to_string(),
        agent: profile.backend.to_string(),
        role: profile.role.to_string(),
        model: profile.model.map(str::to_string),
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
    })
}

pub(super) fn build_profile_fallback_chain(
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
