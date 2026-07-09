//! Agent benchmark / eval harness schema (#158).

use crate::server_state::MemoryServer;
use crate::tool_params::TachiAgentEvalParams;
use serde_json::Value;
use std::path::Path;

mod fixture;
mod live;

pub(crate) use self::fixture::*;
pub(crate) use self::live::*;
pub(crate) use tachi_dispatch::eval::{
    aggregate_performance_matrix, aggregate_scores, aggregate_subagent_scores,
    AgentPerformanceMatrixRow, CompletionStatus, EvalRow, SubagentEvalRow, TaskType,
};

pub(crate) async fn handle_agent_eval(
    _server: &MemoryServer,
    params: TachiAgentEvalParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    match action.as_str() {
        "aggregate" => {
            if !eval_fixture_replay_allowed() {
                return Err(format!(
                    "fixture replay is disabled by default; set {AGENT_EVAL_FIXTURE_ENV}=1 for local eval replay, or use aggregate_live"
                ));
            }
            let path = params
                .fixture_path
                .as_deref()
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| "fixture_path is required for aggregate".to_string())?;
            let rows = load_eval_jsonl(Path::new(path))?;
            let scores = aggregate_scores(&rows);
            let subagent_scores = aggregate_subagent_scores(&rows);
            let performance_matrix = aggregate_performance_matrix(&rows);
            serde_json::to_string(&serde_json::json!({
                "row_count": rows.len(),
                "source": "fixture",
                "scores": scores,
                "subagent_scores": subagent_scores,
                "performance_matrix": performance_matrix,
            }))
            .map_err(|e| format!("serialize aggregate: {e}"))
        }
        "aggregate_live" => {
            let rows = load_live_eval_rows(_server, capped_eval_limit(params.limit))?;
            let scores = aggregate_scores(&rows);
            let subagent_scores = aggregate_subagent_scores(&rows);
            let performance_matrix = aggregate_performance_matrix(&rows);
            serde_json::to_string(&serde_json::json!({
                "row_count": rows.len(),
                "source": "live_memory",
                "scores": scores,
                "subagent_scores": subagent_scores,
                "performance_matrix": performance_matrix,
            }))
            .map_err(|e| format!("serialize aggregate_live: {e}"))
        }
        "telemetry" | "perf" => {
            let rows = load_live_eval_rows(_server, capped_eval_limit(params.limit))?;
            let scores = aggregate_scores(&rows);
            let subagent_scores = aggregate_subagent_scores(&rows);
            let performance_matrix = aggregate_performance_matrix(&rows);
            serde_json::to_string(&serde_json::json!({
                "row_count": rows.len(),
                "source": "live_memory",
                "scores": scores,
                "subagent_scores": subagent_scores,
                "performance_matrix": performance_matrix,
            }))
            .map_err(|e| format!("serialize telemetry: {e}"))
        }
        _ => Err(format!(
            "Invalid eval action '{}'. Use aggregate, aggregate_live, telemetry, or perf.",
            params.action
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvRestore;

    #[test]
    fn fixture_replay_requires_explicit_env_opt_in() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let unset = EnvRestore::remove(AGENT_EVAL_FIXTURE_ENV);
        assert!(!eval_fixture_replay_allowed());
        drop(unset);

        let _set = EnvRestore::set(AGENT_EVAL_FIXTURE_ENV, "1");
        assert!(eval_fixture_replay_allowed());
    }

    #[test]
    fn eval_limit_is_capped_for_live_queries() {
        assert_eq!(capped_eval_limit(None), 500);
        assert_eq!(capped_eval_limit(Some(0)), 1);
        assert_eq!(capped_eval_limit(Some(42)), 42);
        assert_eq!(
            capped_eval_limit(Some(MAX_LIVE_EVAL_LIMIT + 1)),
            MAX_LIVE_EVAL_LIMIT
        );
    }

    #[test]
    fn load_eval_jsonl_rejects_non_regular_and_oversized_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir_err = load_eval_jsonl(tmp.path()).expect_err("directory should be rejected");
        assert!(
            dir_err.contains("regular file"),
            "expected regular-file error, got: {dir_err}"
        );

        let oversized = tmp.path().join("oversized.jsonl");
        let file = std::fs::File::create(&oversized).expect("create oversized fixture");
        file.set_len(MAX_EVAL_FIXTURE_BYTES + 1)
            .expect("resize oversized fixture");
        let size_err = load_eval_jsonl(&oversized).expect_err("oversized file should be rejected");
        assert!(
            size_err.contains("too large"),
            "expected size cap error, got: {size_err}"
        );
    }

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
                    ..Default::default()
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
                    ..Default::default()
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
