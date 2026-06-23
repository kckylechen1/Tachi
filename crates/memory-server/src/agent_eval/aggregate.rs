use super::*;

pub(crate) fn aggregate_scores(rows: &[EvalRow]) -> Vec<AgentTaskScore> {
    use std::collections::HashMap;
    let mut buckets: HashMap<(String, String), (u32, u32, u32)> = HashMap::new();
    for row in rows {
        let task = task_type_name(&row.task_type);
        let key = (row.agent.clone(), task.clone());
        let entry = buckets.entry(key).or_insert((0, 0, 0));
        entry.0 += 1;
        if row.completion_status == CompletionStatus::Completed {
            entry.1 += 1;
        }
        if row.verification_present {
            entry.2 += 1;
        }
    }
    let mut out = Vec::new();
    for ((agent, task_type), (samples, ok, verified)) in buckets {
        let samples_f = samples as f64;
        out.push(AgentTaskScore {
            agent,
            task_type,
            samples,
            success_rate: ok as f64 / samples_f,
            verification_rate: verified as f64 / samples_f,
        });
    }
    out.sort_by(|a, b| a.agent.cmp(&b.agent).then(a.task_type.cmp(&b.task_type)));
    out
}

pub(crate) fn aggregate_subagent_scores(rows: &[EvalRow]) -> Vec<SubagentTaskScore> {
    use std::collections::HashMap;

    #[derive(Default)]
    struct Bucket {
        samples: u32,
        useful: u32,
        score_sum: f64,
        score_count: u32,
        changed_plan: u32,
        failures: u32,
    }

    let mut buckets: HashMap<(String, String, Option<String>, String), Bucket> = HashMap::new();
    for row in rows {
        let task_type = task_type_name(&row.task_type);
        for subagent in &row.subagents {
            let subagent_task_type = subagent
                .task_type
                .clone()
                .unwrap_or_else(|| task_type.clone());
            let key = (
                subagent.role.clone(),
                subagent.agent.clone(),
                subagent.model.clone(),
                subagent_task_type,
            );
            let entry = buckets.entry(key).or_default();
            entry.samples += 1;

            let outcome = subagent
                .outcome
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(outcome.as_str(), "useful" | "success" | "completed") {
                entry.useful += 1;
            }
            if matches!(outcome.as_str(), "failed" | "failure")
                || subagent
                    .failure_mode
                    .as_deref()
                    .is_some_and(|s| !s.is_empty())
            {
                entry.failures += 1;
            }
            if let Some(score) = subagent.usefulness_score {
                entry.score_sum += score;
                entry.score_count += 1;
            }
            let impact = subagent
                .verification_impact
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            if impact.contains("changed_plan") || impact.contains("changed plan") {
                entry.changed_plan += 1;
            }
        }
    }

    let mut out = Vec::new();
    for ((role, agent, model, task_type), bucket) in buckets {
        let samples_f = bucket.samples as f64;
        out.push(SubagentTaskScore {
            role,
            agent,
            model,
            task_type,
            samples: bucket.samples,
            useful_rate: bucket.useful as f64 / samples_f,
            avg_usefulness_score: (bucket.score_count > 0)
                .then(|| bucket.score_sum / bucket.score_count as f64),
            changed_plan_count: bucket.changed_plan,
            failure_count: bucket.failures,
        });
    }
    out.sort_by(|a, b| {
        a.role
            .cmp(&b.role)
            .then(a.agent.cmp(&b.agent))
            .then(a.model.cmp(&b.model))
            .then(a.task_type.cmp(&b.task_type))
    });
    out
}

pub(crate) fn aggregate_performance_matrix(rows: &[EvalRow]) -> Vec<AgentPerformanceMatrixRow> {
    use std::collections::HashMap;

    #[derive(Default)]
    struct Bucket {
        samples: u32,
        success: u32,
        useful: u32,
        verified: u32,
        failures: u32,
        human_overrides: u32,
        retry_sum: u64,
        latency_sum: u128,
        latency_count: u32,
        input_tokens_sum: u128,
        input_tokens_count: u32,
        output_tokens_sum: u128,
        output_tokens_count: u32,
        cost_tokens_sum: u128,
        cost_tokens_count: u32,
        cost_usd_sum: f64,
        cost_usd_count: u32,
        quality_score_sum: f64,
        quality_score_count: u32,
        latencies: Vec<u64>,
    }

    type Key = (
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        String,
    );

    fn add_u64(sum: &mut u128, count: &mut u32, value: Option<u64>) {
        if let Some(value) = value {
            *sum += value as u128;
            *count += 1;
        }
    }

    fn add_f64(sum: &mut f64, count: &mut u32, value: Option<f64>) {
        if let Some(value) = value {
            *sum += value;
            *count += 1;
        }
    }

    fn avg_u128(sum: u128, count: u32) -> Option<f64> {
        (count > 0).then(|| round2(sum as f64 / count as f64))
    }

    fn avg_f64(sum: f64, count: u32) -> Option<f64> {
        (count > 0).then(|| round4(sum / count as f64))
    }

    fn total_f64(sum: f64, count: u32) -> Option<f64> {
        (count > 0).then(|| round4(sum))
    }

    fn percentile(values: &[u64], percentile: f64) -> Option<u64> {
        if values.is_empty() {
            return None;
        }
        let idx = ((values.len() - 1) as f64 * percentile).ceil() as usize;
        values.get(idx).copied()
    }

    fn round2(value: f64) -> f64 {
        (value * 100.0).round() / 100.0
    }

    fn round4(value: f64) -> f64 {
        (value * 10_000.0).round() / 10_000.0
    }

    fn outcome_is_useful(outcome: Option<&str>) -> bool {
        matches!(
            outcome.unwrap_or("").to_ascii_lowercase().as_str(),
            "useful" | "success" | "completed"
        )
    }

    fn outcome_is_failure(outcome: Option<&str>) -> bool {
        matches!(
            outcome.unwrap_or("").to_ascii_lowercase().as_str(),
            "failed" | "failure"
        )
    }

    let mut buckets: HashMap<Key, Bucket> = HashMap::new();
    for row in rows {
        let task_type = task_type_name(&row.task_type);
        let leader_key = (
            "leader".to_string(),
            row.profile.clone(),
            None,
            row.agent.clone(),
            row.model.clone(),
            task_type.clone(),
        );
        let entry = buckets.entry(leader_key).or_default();
        entry.samples += 1;
        if row.completion_status == CompletionStatus::Completed {
            entry.success += 1;
        }
        if row.verification_present {
            entry.verified += 1;
        }
        if row.completion_status != CompletionStatus::Completed
            || row
                .failure_mode
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
        {
            entry.failures += 1;
        }
        add_u64(
            &mut entry.latency_sum,
            &mut entry.latency_count,
            row.latency_ms,
        );
        if let Some(latency) = row.latency_ms {
            entry.latencies.push(latency);
        }
        add_u64(
            &mut entry.cost_tokens_sum,
            &mut entry.cost_tokens_count,
            row.cost_tokens,
        );
        add_f64(
            &mut entry.cost_usd_sum,
            &mut entry.cost_usd_count,
            row.cost_usd,
        );
        add_f64(
            &mut entry.quality_score_sum,
            &mut entry.quality_score_count,
            row.quality_score,
        );

        for subagent in &row.subagents {
            let subagent_task_type = subagent
                .task_type
                .clone()
                .unwrap_or_else(|| task_type.clone());
            let sub_key = (
                "subagent".to_string(),
                row.profile.clone(),
                Some(subagent.role.clone()),
                subagent.agent.clone(),
                subagent.model.clone(),
                subagent_task_type,
            );
            let entry = buckets.entry(sub_key).or_default();
            entry.samples += 1;
            if outcome_is_useful(subagent.outcome.as_deref()) {
                entry.useful += 1;
            }
            if outcome_is_failure(subagent.outcome.as_deref())
                || subagent
                    .failure_mode
                    .as_deref()
                    .is_some_and(|s| !s.trim().is_empty())
            {
                entry.failures += 1;
            }
            if subagent.verification_present {
                entry.verified += 1;
            }
            if subagent.human_override {
                entry.human_overrides += 1;
            }
            entry.retry_sum += subagent.retry_count as u64;
            add_u64(
                &mut entry.latency_sum,
                &mut entry.latency_count,
                subagent.latency_ms,
            );
            if let Some(latency) = subagent.latency_ms {
                entry.latencies.push(latency);
            }
            add_u64(
                &mut entry.input_tokens_sum,
                &mut entry.input_tokens_count,
                subagent.input_tokens,
            );
            add_u64(
                &mut entry.output_tokens_sum,
                &mut entry.output_tokens_count,
                subagent.output_tokens,
            );
            add_u64(
                &mut entry.cost_tokens_sum,
                &mut entry.cost_tokens_count,
                subagent.cost_tokens,
            );
            add_f64(
                &mut entry.cost_usd_sum,
                &mut entry.cost_usd_count,
                subagent.cost_usd,
            );
        }
    }

    let mut out = Vec::new();
    for ((scope, profile, role, agent, model, task_type), bucket) in buckets {
        let samples_f = bucket.samples as f64;
        let mut latencies = bucket.latencies;
        latencies.sort_unstable();
        out.push(AgentPerformanceMatrixRow {
            success_rate: (scope == "leader").then(|| round4(bucket.success as f64 / samples_f)),
            useful_rate: (scope == "subagent").then(|| round4(bucket.useful as f64 / samples_f)),
            verification_rate: round4(bucket.verified as f64 / samples_f),
            failure_count: bucket.failures,
            human_override_rate: round4(bucket.human_overrides as f64 / samples_f),
            avg_retry_count: round4(bucket.retry_sum as f64 / samples_f),
            avg_latency_ms: avg_u128(bucket.latency_sum, bucket.latency_count),
            p50_latency_ms: percentile(&latencies, 0.50),
            p95_latency_ms: percentile(&latencies, 0.95),
            avg_input_tokens: avg_u128(bucket.input_tokens_sum, bucket.input_tokens_count),
            avg_output_tokens: avg_u128(bucket.output_tokens_sum, bucket.output_tokens_count),
            avg_cost_tokens: avg_u128(bucket.cost_tokens_sum, bucket.cost_tokens_count),
            avg_cost_usd: avg_f64(bucket.cost_usd_sum, bucket.cost_usd_count),
            total_cost_usd: total_f64(bucket.cost_usd_sum, bucket.cost_usd_count),
            avg_quality_score: avg_f64(bucket.quality_score_sum, bucket.quality_score_count),
            scope,
            profile,
            role,
            agent,
            model,
            task_type,
            samples: bucket.samples,
        });
    }
    out.sort_by(|a, b| {
        a.scope
            .cmp(&b.scope)
            .then(a.profile.cmp(&b.profile))
            .then(a.role.cmp(&b.role))
            .then(a.agent.cmp(&b.agent))
            .then(a.model.cmp(&b.model))
            .then(a.task_type.cmp(&b.task_type))
    });
    out
}
