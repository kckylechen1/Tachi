use super::risk::classify_dispatch_risk;
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

    let candidates = tachi_dispatch::recommend_dispatch_profile_candidates(
        &risk,
        &tachi_dispatch::route_eval_rows(&rows),
        &tachi_dispatch::route_subagent_scores(&subagent_scores),
        &tachi_dispatch::route_performance_rows(&performance_matrix),
        &route_policy_rules,
        |profile| profile_weak_against_for_server(server, profile),
    )?;

    let best = candidates
        .first()
        .ok_or_else(|| "no dispatch profiles configured".to_string())?;
    let best_profile = resolve_dispatch_profile(&best.profile)
        .ok_or_else(|| format!("internal missing profile {}", best.profile))?;
    let (recommended_transport, transport_readiness) =
        recommended_transport_for_profile(best_profile);

    let profile_json = profile_json_for_server(server, best_profile)?;
    let payload = tachi_dispatch::build_dispatch_recommendation_response(
        task,
        &risk,
        best_profile,
        &candidates,
        &route_policy_rules,
        rows.len(),
        tachi_dispatch::RecommendationProfilePayload {
            recommended_transport,
            transport_readiness,
            evidence_required: json!(profile_evidence_required_for_server(server, best_profile)?),
            evidence_contract: profile_evidence_contract_json_for_server(server, best_profile)?,
            resolved_skills: profile_required_skill_ids_for_server(server, best_profile)?,
            resolved_skill_loadout: profile_skill_loadout_json_for_server(server, best_profile)?,
            mbit_card: profile_json
                .get("mbit_card")
                .cloned()
                .unwrap_or(Value::Null),
        },
    )?;

    serde_json::to_string(&payload).map_err(|e| format!("serialize recommendation: {e}"))
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
