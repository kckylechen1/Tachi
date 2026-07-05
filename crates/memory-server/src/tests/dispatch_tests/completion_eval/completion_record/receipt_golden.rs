use super::*;

const ECHO_SENTINEL: &str = "ZX9-ECHO-SENTINEL";

#[tokio::test]
async fn g4_complete_default_omits_subagent_notes_sentinel() {
    let server = make_server();

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("g528-complete".to_string()),
            task: format!("Task with {ECHO_SENTINEL}"),
            agent: "claude-code".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: Some("claude_plan".to_string()),
            risk: Some("low".to_string()),
            duration_ms: Some(10),
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.8),
            notes: Some("leader notes".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: vec![TachiSubagentEvalParams {
                role: "worker".to_string(),
                agent: "kimi".to_string(),
                model: None,
                task: Some("subtask".to_string()),
                task_type: None,
                outcome: Some("useful".to_string()),
                usefulness_score: None,
                failure_mode: None,
                verification_impact: None,
                verification_present: false,
                evaluator: None,
                plan_delta: None,
                human_override: false,
                retry_count: 0,
                notes: Some(format!("notes with {ECHO_SENTINEL}")),
                latency_ms: None,
                input_tokens: None,
                output_tokens: None,
                cost_tokens: None,
                cost_usd: None,
            }],
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: None,
            scope: Some("global".to_string()),
            project: None,
            format: None,
        }))
        .await
        .expect("complete");

    assert!(
        !resp.contains(ECHO_SENTINEL),
        "complete receipt echoed subagent notes: {resp}"
    );
    let bundle: Value = serde_json::from_str(&resp).expect("bundle");
    assert_eq!(bundle["subagent_count"], json!(1));
    assert!(bundle.get("subagents").is_none());
    assert!(bundle.get("task").is_none());
    assert!(bundle.get("notes").is_none());
    assert!(bundle.get("pipeline").is_some());
    assert!(bundle.get("next_steps").is_some());
    assert!(resp.len() < 600, "complete receipt too large: {} bytes", resp.len());
}