use super::risk::classify_dispatch_risk;
use super::scoring::{build_profile_fallback_chain, score_profile_candidate};
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

pub(in crate::dispatch_profile) fn recommended_transport_for_profile(
    profile: &DispatchProfileDef,
) -> (String, Value) {
    if !profile_uses_opencode_adapter(profile) {
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
