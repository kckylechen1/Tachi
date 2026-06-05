//! Agent benchmark / eval harness schema (#158).

use super::*;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskType {
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
pub(crate) enum CompletionStatus {
    Completed,
    Blocked,
    Stalled,
    Exploratory,
    Superseded,
    InvalidRequest,
    EnvironmentFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EvalRow {
    pub agent: String,
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
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub subagents: Vec<SubagentEvalRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SubagentEvalRow {
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
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct AgentTaskScore {
    pub agent: String,
    pub task_type: String,
    pub samples: u32,
    pub success_rate: f64,
    pub verification_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct SubagentTaskScore {
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

pub(crate) fn load_eval_jsonl(path: &Path) -> Result<Vec<EvalRow>, String> {
    let file = File::open(path).map_err(|e| format!("open eval file {}: {e}", path.display()))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("read line {}: {e}", line_no + 1))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row: EvalRow = serde_json::from_str(trimmed)
            .map_err(|e| format!("parse eval line {}: {e}", line_no + 1))?;
        rows.push(row);
    }
    Ok(rows)
}

pub(crate) fn aggregate_scores(rows: &[EvalRow]) -> Vec<AgentTaskScore> {
    use std::collections::HashMap;
    let mut buckets: HashMap<(String, String), (u32, u32, u32)> = HashMap::new();
    for row in rows {
        let task = serde_json::to_string(&row.task_type)
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_else(|_| format!("{:?}", row.task_type));
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
        let task_type = serde_json::to_string(&row.task_type)
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_else(|_| format!("{:?}", row.task_type));
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

fn task_type_from_str(value: Option<&str>) -> TaskType {
    value
        .and_then(|s| serde_json::from_value(serde_json::json!(s)).ok())
        .unwrap_or(TaskType::Other)
}

fn completion_status_from_outcome(value: Option<&str>) -> CompletionStatus {
    match value.unwrap_or("").to_ascii_lowercase().as_str() {
        "success" | "completed" => CompletionStatus::Completed,
        "partial" => CompletionStatus::Stalled,
        "aborted" => CompletionStatus::Blocked,
        "failure" | "failed" => CompletionStatus::Stalled,
        _ => CompletionStatus::Exploratory,
    }
}

fn eval_row_from_memory(entry: &memory_core::MemoryEntry) -> Option<EvalRow> {
    let meta = entry.metadata.as_object()?;
    let agent = meta.get("agent")?.as_str()?.to_string();
    let outcome = meta.get("outcome").and_then(|v| v.as_str());
    let task_type = task_type_from_str(meta.get("task_type").and_then(|v| v.as_str()));
    let subagents: Vec<SubagentEvalRow> = meta
        .get("subagents")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    Some(EvalRow {
        agent,
        model: meta
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        mode: meta
            .get("mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        task_type,
        turns: meta.get("turns").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        tool_calls: meta.get("tool_calls").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        verification_present: meta
            .get("verification_present")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        failure_mode: meta
            .get("failure_mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        completion_status: completion_status_from_outcome(outcome),
        cost_usd: meta.get("cost_usd").and_then(|v| v.as_f64()),
        latency_ms: meta.get("duration_ms").and_then(|v| v.as_u64()),
        subagents,
    })
}

fn load_live_eval_rows(server: &MemoryServer, limit: usize) -> Result<Vec<EvalRow>, String> {
    let mut entries = server.with_global_store_read(|store| {
        store
            .list_by_path("/eval", limit, false)
            .map_err(|e| format!("list global eval rows: {e}"))
    })?;
    if server.has_project_db() {
        let mut project_entries = server.with_project_store_read(|store| {
            store
                .list_by_path("/eval", limit, false)
                .map_err(|e| format!("list project eval rows: {e}"))
        })?;
        entries.append(&mut project_entries);
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit);
    Ok(entries.iter().filter_map(eval_row_from_memory).collect())
}

pub(crate) async fn handle_agent_eval(
    _server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    match action.as_str() {
        "aggregate" => {
            let path = params
                .fixture_path
                .as_deref()
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| "fixture_path is required for aggregate".to_string())?;
            let rows = load_eval_jsonl(Path::new(path))?;
            let scores = aggregate_scores(&rows);
            let subagent_scores = aggregate_subagent_scores(&rows);
            serde_json::to_string(&serde_json::json!({
                "row_count": rows.len(),
                "source": "fixture",
                "scores": scores,
                "subagent_scores": subagent_scores,
            }))
            .map_err(|e| format!("serialize aggregate: {e}"))
        }
        "aggregate_live" => {
            let rows = load_live_eval_rows(_server, params.limit.unwrap_or(500).max(1))?;
            let scores = aggregate_scores(&rows);
            let subagent_scores = aggregate_subagent_scores(&rows);
            serde_json::to_string(&serde_json::json!({
                "row_count": rows.len(),
                "source": "live_memory",
                "scores": scores,
                "subagent_scores": subagent_scores,
            }))
            .map_err(|e| format!("serialize aggregate_live: {e}"))
        }
        _ => Err(format!(
            "Invalid eval action '{}'. Use aggregate or aggregate_live.",
            params.action
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_computes_rates() {
        let rows = vec![
            EvalRow {
                agent: "claude".to_string(),
                model: None,
                mode: None,
                task_type: TaskType::FixRequest,
                turns: 3,
                tool_calls: 5,
                verification_present: true,
                failure_mode: None,
                completion_status: CompletionStatus::Completed,
                cost_usd: None,
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
                }],
            },
            EvalRow {
                agent: "claude".to_string(),
                model: None,
                mode: None,
                task_type: TaskType::FixRequest,
                turns: 8,
                tool_calls: 20,
                verification_present: false,
                failure_mode: Some("retry_loop".to_string()),
                completion_status: CompletionStatus::Stalled,
                cost_usd: None,
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
    }
}
