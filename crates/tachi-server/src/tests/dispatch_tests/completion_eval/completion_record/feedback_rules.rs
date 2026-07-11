use super::*;

#[tokio::test]
async fn tachi_complete_records_applied_feedback_rules_for_eval_aggregation() {
    let server = make_server();
    let rule_id = save_grep_evidence_feedback_rule(&server).await;
    let raw = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("feedback-rule-complete".to_string()),
            task: "Review unused code with grep evidence".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("code_audit".to_string()),
            profile: Some("codex_55_review".to_string()),
            risk: Some("medium".to_string()),
            duration_ms: Some(42),
            skills_used: vec!["skill:waza-check".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: Some("Applied grep evidence feedback rule.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: vec![rule_id.clone()],
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: vec!["grep output".to_string()],
            tests_run: vec!["cargo test feedback rules".to_string()],
            diff_present: Some(false),
            scope: None,
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
        }))
        .await
        .expect("completion should succeed");
    let completed: Value = serde_json::from_str(&raw).expect("complete JSON");
    let memory_id = completed["eval_entry"]["id"].as_str().expect("memory id");
    let fetched = crate::memory_ops::handle_get_memory(
        &server,
        GetMemoryParams {
            id: memory_id.to_string(),
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("eval memory should be readable");
    let eval: Value = serde_json::from_str(&fetched).expect("eval JSON");
    assert_eq!(eval["metadata"]["feedback_rules_applied"], json!([rule_id]));
}
