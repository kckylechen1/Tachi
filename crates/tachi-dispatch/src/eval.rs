use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    FixRequest,
    ReviewRequest,
    PlanRequest,
    TestRequest,
    RefactorRequest,
    ExplainRequest,
    ResearchRequest,
    MigrationRequest,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionStatus {
    Completed,
    Blocked,
    Stalled,
    Exploratory,
    Superseded,
    InvalidRequest,
    EnvironmentFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRow {
    pub agent: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    pub task_type: TaskType,
    #[serde(default)]
    pub turns: u32,
    #[serde(default)]
    pub tool_calls: u32,
    #[serde(default)]
    pub verification_present: bool,
    #[serde(default)]
    pub failure_mode: Option<String>,
    pub completion_status: CompletionStatus,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub cost_tokens: Option<u64>,
    #[serde(default)]
    pub quality_score: Option<f64>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub subagents: Vec<SubagentEvalRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubagentEvalRow {
    pub role: String,
    pub agent: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub usefulness_score: Option<f64>,
    #[serde(default)]
    pub failure_mode: Option<String>,
    #[serde(default)]
    pub verification_impact: Option<String>,
    #[serde(default)]
    pub verification_present: bool,
    #[serde(default)]
    pub evaluator: Option<String>,
    #[serde(default)]
    pub plan_delta: Option<String>,
    #[serde(default)]
    pub human_override: bool,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cost_tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentTaskScore {
    pub agent: String,
    pub task_type: String,
    pub samples: u32,
    pub success_rate: f64,
    pub verification_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SubagentTaskScore {
    pub role: String,
    pub agent: String,
    pub model: Option<String>,
    pub task_type: String,
    pub samples: u32,
    pub useful_rate: f64,
    pub avg_usefulness_score: Option<f64>,
    pub changed_plan_count: u32,
    pub failure_count: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentPerformanceMatrixRow {
    pub scope: String,
    pub profile: Option<String>,
    pub role: Option<String>,
    pub agent: String,
    pub model: Option<String>,
    pub task_type: String,
    pub samples: u32,
    pub success_rate: Option<f64>,
    pub useful_rate: Option<f64>,
    pub verification_rate: f64,
    pub failure_count: u32,
    pub human_override_rate: f64,
    pub avg_retry_count: f64,
    pub avg_latency_ms: Option<f64>,
    pub p50_latency_ms: Option<u64>,
    pub p95_latency_ms: Option<u64>,
    pub avg_input_tokens: Option<f64>,
    pub avg_output_tokens: Option<f64>,
    pub avg_cost_tokens: Option<f64>,
    pub avg_cost_usd: Option<f64>,
    pub total_cost_usd: Option<f64>,
    pub avg_quality_score: Option<f64>,
}

pub fn task_type_name(task_type: &TaskType) -> String {
    serde_json::to_string(task_type)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| format!("{task_type:?}"))
}

pub fn aggregate_scores(rows: &[EvalRow]) -> Vec<AgentTaskScore> {
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

pub fn aggregate_subagent_scores(rows: &[EvalRow]) -> Vec<SubagentTaskScore> {
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

pub fn aggregate_performance_matrix(rows: &[EvalRow]) -> Vec<AgentPerformanceMatrixRow> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_computes_rates() {
        let rows = vec![
            EvalRow {
                agent: "claude".to_string(),
                profile: Some("claude_plan".to_string()),
                model: None,
                mode: None,
                task_type: TaskType::FixRequest,
                turns: 3,
                tool_calls: 5,
                verification_present: true,
                failure_mode: None,
                completion_status: CompletionStatus::Completed,
                cost_usd: None,
                cost_tokens: Some(1200),
                quality_score: Some(0.9),
                latency_ms: Some(1000),
                subagents: vec![SubagentEvalRow {
                    role: "architect".to_string(),
                    agent: "kimi".to_string(),
                    model: Some("kimi-for-coding".to_string()),
                    task_type: Some("plan_request".to_string()),
                    outcome: Some("useful".to_string()),
                    usefulness_score: Some(0.8),
                    failure_mode: None,
                    verification_impact: Some("changed_plan".to_string()),
                    verification_present: true,
                    evaluator: Some("leader".to_string()),
                    plan_delta: Some("modified".to_string()),
                    human_override: false,
                    retry_count: 0,
                    latency_ms: Some(1200),
                    input_tokens: Some(1000),
                    output_tokens: Some(200),
                    cost_tokens: Some(1200),
                    cost_usd: Some(0.01),
                }],
            },
            EvalRow {
                agent: "claude".to_string(),
                profile: Some("claude_plan".to_string()),
                model: None,
                mode: None,
                task_type: TaskType::FixRequest,
                turns: 8,
                tool_calls: 20,
                verification_present: false,
                failure_mode: Some("retry_loop".to_string()),
                completion_status: CompletionStatus::Stalled,
                cost_usd: None,
                cost_tokens: None,
                quality_score: Some(0.3),
                latency_ms: Some(5000),
                subagents: vec![SubagentEvalRow {
                    role: "explore".to_string(),
                    agent: "deepseek".to_string(),
                    model: Some("deepseek-v4-flash".to_string()),
                    task_type: Some("fix_request".to_string()),
                    outcome: Some("failed".to_string()),
                    usefulness_score: Some(0.2),
                    failure_mode: Some("missed_contract".to_string()),
                    verification_impact: Some("none".to_string()),
                    verification_present: false,
                    evaluator: Some("leader".to_string()),
                    plan_delta: Some("rejected".to_string()),
                    human_override: false,
                    retry_count: 1,
                    latency_ms: Some(800),
                    input_tokens: Some(500),
                    output_tokens: Some(100),
                    cost_tokens: Some(600),
                    cost_usd: Some(0.002),
                }],
            },
        ];

        let scores = aggregate_scores(&rows);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].task_type, "fix_request");
        assert_eq!(scores[0].samples, 2);
        assert!((scores[0].success_rate - 0.5).abs() < f64::EPSILON);

        let subagent_scores = aggregate_subagent_scores(&rows);
        assert_eq!(subagent_scores.len(), 2);
        let kimi = subagent_scores
            .iter()
            .find(|s| s.agent == "kimi")
            .expect("kimi subagent score");
        assert_eq!(kimi.role, "architect");
        assert_eq!(kimi.task_type, "plan_request");
        assert_eq!(kimi.samples, 1);
        assert!((kimi.useful_rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(kimi.changed_plan_count, 1);

        let performance = aggregate_performance_matrix(&rows);
        let claude = performance
            .iter()
            .find(|row| row.scope == "leader" && row.agent == "claude")
            .expect("leader performance row");
        assert_eq!(claude.samples, 2);
        assert_eq!(claude.success_rate, Some(0.5));
        assert_eq!(claude.verification_rate, 0.5);
        assert_eq!(claude.avg_latency_ms, Some(3000.0));
        assert_eq!(claude.p50_latency_ms, Some(5000));
        assert_eq!(claude.p95_latency_ms, Some(5000));
        assert_eq!(claude.avg_cost_tokens, Some(1200.0));
        assert_eq!(claude.avg_quality_score, Some(0.6));

        let deepseek = performance
            .iter()
            .find(|row| row.scope == "subagent" && row.agent == "deepseek")
            .expect("subagent performance row");
        assert_eq!(deepseek.role.as_deref(), Some("explore"));
        assert_eq!(deepseek.useful_rate, Some(0.0));
        assert_eq!(deepseek.failure_count, 1);
        assert_eq!(deepseek.avg_retry_count, 1.0);
        assert_eq!(deepseek.avg_input_tokens, Some(500.0));
        assert_eq!(deepseek.avg_cost_usd, Some(0.002));
        assert_eq!(deepseek.total_cost_usd, Some(0.002));
    }
}
