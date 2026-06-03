use super::*;

#[tokio::test]
async fn orchestrator_todos_and_handoff_persist() {
    let server = make_server();
    let task_id = "dispatch-test-001";

    let update = server
        .tachi_orchestrator(Parameters(TachiOrchestratorParams {
            action: "todo_update".to_string(),
            task_id: task_id.to_string(),
            todo_id: Some("t1".to_string()),
            todo_content: Some("Implement registry".to_string()),
            todo_status: Some("in_progress".to_string()),
            parent_todo_id: None,
            agent: Some("claude".to_string()),
            issue_ref: Some("kckylechen1/tachi#157".to_string()),
            blocked_reason: None,
            verification: None,
            references: vec![],
            objective: None,
            current_state: None,
            completed_steps: vec![],
            remaining_steps: vec![],
            files_touched: vec![],
            commands_run: vec![],
            tests_run: vec![],
            known_blockers: vec![],
            next_action: None,
            newest_user_instruction: None,
        }))
        .await
        .expect("todo_update");
    let update_json: Value = serde_json::from_str(&update).expect("json");
    assert_eq!(update_json["ok"], json!(true));

    let list = server
        .tachi_orchestrator(Parameters(TachiOrchestratorParams {
            action: "todo_list".to_string(),
            task_id: task_id.to_string(),
            todo_id: None,
            todo_content: None,
            todo_status: None,
            parent_todo_id: None,
            agent: None,
            issue_ref: None,
            blocked_reason: None,
            verification: None,
            references: vec![],
            objective: None,
            current_state: None,
            completed_steps: vec![],
            remaining_steps: vec![],
            files_touched: vec![],
            commands_run: vec![],
            tests_run: vec![],
            known_blockers: vec![],
            next_action: None,
            newest_user_instruction: None,
        }))
        .await
        .expect("todo_list");
    let list_json: Value = serde_json::from_str(&list).expect("json");
    assert_eq!(list_json["todos"].as_array().map(|a| a.len()), Some(1));

    let handoff = server
        .tachi_orchestrator(Parameters(TachiOrchestratorParams {
            action: "handoff_write".to_string(),
            task_id: task_id.to_string(),
            todo_id: None,
            todo_content: None,
            todo_status: None,
            parent_todo_id: None,
            agent: None,
            issue_ref: None,
            blocked_reason: None,
            verification: None,
            references: vec!["kckylechen1/tachi#157".to_string()],
            objective: Some("Finish orchestrator".to_string()),
            current_state: Some("Registry done".to_string()),
            completed_steps: vec!["schema".to_string()],
            remaining_steps: vec!["tests".to_string()],
            files_touched: vec![],
            commands_run: vec![],
            tests_run: vec![],
            known_blockers: vec![],
            next_action: Some("Run CI".to_string()),
            newest_user_instruction: Some("Don't stop".to_string()),
        }))
        .await
        .expect("handoff_write");

    let recovery = server
        .tachi_orchestrator(Parameters(TachiOrchestratorParams {
            action: "recovery_briefing".to_string(),
            task_id: task_id.to_string(),
            todo_id: None,
            todo_content: None,
            todo_status: None,
            parent_todo_id: None,
            agent: None,
            issue_ref: None,
            blocked_reason: None,
            verification: None,
            references: vec![],
            objective: None,
            current_state: None,
            completed_steps: vec![],
            remaining_steps: vec![],
            files_touched: vec![],
            commands_run: vec![],
            tests_run: vec![],
            known_blockers: vec![],
            next_action: None,
            newest_user_instruction: None,
        }))
        .await
        .expect("recovery");
    let recovery_json: Value = serde_json::from_str(&recovery).expect("json");
    assert!(recovery_json["handoff"].is_object());
    assert_eq!(
        recovery_json["incomplete_todos"].as_array().map(|a| a.len()),
        Some(1)
    );
    let _ = handoff;
}