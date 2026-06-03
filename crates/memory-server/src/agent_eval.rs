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
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct AgentTaskScore {
    pub agent: String,
    pub task_type: String,
    pub samples: u32,
    pub success_rate: f64,
    pub verification_rate: f64,
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
            serde_json::to_string(&serde_json::json!({
                "row_count": rows.len(),
                "scores": scores,
            }))
            .map_err(|e| format!("serialize aggregate: {e}"))
        }
        _ => Err(format!(
            "Invalid eval action '{}'. Use aggregate.",
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
            },
        ];
        let scores = aggregate_scores(&rows);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].task_type, "fix_request");
        assert_eq!(scores[0].samples, 2);
        assert!((scores[0].success_rate - 0.5).abs() < f64::EPSILON);
    }
}
