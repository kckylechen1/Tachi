use super::*;

pub(in crate::dispatch_profile) fn score_profile_candidate(
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
        let agent_match = profile_matches_agent(profile, &sub.agent);
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
        if let Some(profile) = DISPATCH_PROFILES.iter().find(|profile| {
            profile_matches_agent(profile, agent) && !out.iter().any(|p| p == profile.name)
        }) {
            out.push(profile.name.to_string());
        }
    }
    out
}
