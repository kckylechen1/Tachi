use super::*;

#[tokio::test]
async fn tachi_complete_failure_with_notes_saves_lesson_hook() {
    let server = make_server();

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("lesson-hook-001".to_string()),
            task: "Fix a brittle integration path".to_string(),
            agent: "codex".to_string(),
            outcome: "failure".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: Some("medium".to_string()),
            duration_ms: Some(900),
            skills_used: vec!["skill:check".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: None,
            notes: Some("The attempted fix lacked a regression test.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
        }))
        .await
        .expect("failure complete should still record eval");

    let bundle: Value = serde_json::from_str(&resp).expect("complete JSON");
    assert_eq!(
        bundle["pipeline"]["post_complete_hooks"],
        json!("lesson_saved")
    );

    let resp = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("lesson-hook-002".to_string()),
            task: "Fix".to_string(),
            agent: "codex".to_string(),
            outcome: "failure".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: Some("medium".to_string()),
            duration_ms: Some(900),
            skills_used: vec!["skill:other".to_string()],
            cost_tokens: None,
            cost_usd: None,
            quality_score: None,
            notes: Some("A short task name should not dedup against a longer task.".to_string()),
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            evidence_refs: Vec::new(),
            tests_run: Vec::new(),
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
        }))
        .await
        .expect("distinct short task should still record a lesson");

    let bundle: Value = serde_json::from_str(&resp).expect("complete JSON");
    assert_eq!(
        bundle["pipeline"]["post_complete_hooks"],
        json!("lesson_saved")
    );
}
