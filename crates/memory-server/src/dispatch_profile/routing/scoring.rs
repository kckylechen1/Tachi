use super::*;

pub(in crate::dispatch_profile) fn route_eval_rows(rows: &[EvalRow]) -> Vec<RouteEvalRow> {
    rows.iter()
        .map(|row| RouteEvalRow {
            profile: row.profile.clone(),
            completed: row.completion_status == CompletionStatus::Completed,
            verification_present: row.verification_present,
        })
        .collect()
}

pub(in crate::dispatch_profile) fn route_subagent_scores(
    rows: &[crate::agent_eval::SubagentTaskScore],
) -> Vec<RouteSubagentScore> {
    rows.iter()
        .map(|row| RouteSubagentScore {
            role: row.role.clone(),
            agent: row.agent.clone(),
            task_type: row.task_type.clone(),
            samples: row.samples,
            useful_rate: row.useful_rate,
            failure_count: row.failure_count,
        })
        .collect()
}

pub(in crate::dispatch_profile) fn route_performance_rows(
    rows: &[AgentPerformanceMatrixRow],
) -> Vec<RoutePerformanceRow> {
    rows.iter()
        .map(|row| RoutePerformanceRow {
            scope: row.scope.clone(),
            profile: row.profile.clone(),
            role: row.role.clone(),
            agent: row.agent.clone(),
            task_type: row.task_type.clone(),
            samples: row.samples,
            success_rate: row.success_rate,
            useful_rate: row.useful_rate,
            verification_rate: row.verification_rate,
            failure_count: row.failure_count,
            human_override_rate: row.human_override_rate,
            avg_retry_count: row.avg_retry_count,
            avg_latency_ms: row.avg_latency_ms,
            avg_cost_usd: row.avg_cost_usd,
            avg_quality_score: row.avg_quality_score,
        })
        .collect()
}
