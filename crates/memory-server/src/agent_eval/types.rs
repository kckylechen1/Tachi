use serde::{Deserialize, Serialize};

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
    #[serde(default)]
    pub cost_tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
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

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct AgentPerformanceMatrixRow {
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

pub(crate) fn task_type_name(task_type: &TaskType) -> String {
    serde_json::to_string(task_type)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| format!("{:?}", task_type))
}
