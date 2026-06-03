use super::*;

fn orchestrator_params(action: &str, task_id: &str) -> TachiOrchestratorParams {
    TachiOrchestratorParams {
        action: action.to_string(),
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
    }
}

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
        recovery_json["incomplete_todos"]
            .as_array()
            .map(|a| a.len()),
        Some(1)
    );
    let _ = handoff;
}

#[tokio::test]
async fn todo_update_preserves_status_and_clears_stale_state() {
    let server = make_server();
    let task_id = "dispatch-test-status";

    let mut create = orchestrator_params("todo_update", task_id);
    create.todo_id = Some("t1".to_string());
    create.todo_content = Some("Finish merge".to_string());
    create.todo_status = Some("done".to_string());
    server
        .tachi_orchestrator(Parameters(create))
        .await
        .expect("create done todo");

    let mut content_only = orchestrator_params("todo_update", task_id);
    content_only.todo_id = Some("t1".to_string());
    content_only.todo_content = Some("Finish merge after CI".to_string());
    server
        .tachi_orchestrator(Parameters(content_only))
        .await
        .expect("content-only update");

    let list = server
        .tachi_orchestrator(Parameters(orchestrator_params("todo_list", task_id)))
        .await
        .expect("todo_list");
    let list_json: Value = serde_json::from_str(&list).expect("json");
    let todo = &list_json["todos"][0];
    assert_eq!(todo["status"], json!("done"));
    assert!(todo["completed_at"].as_str().is_some());

    let mut blocked = orchestrator_params("todo_update", task_id);
    blocked.todo_id = Some("t1".to_string());
    blocked.todo_status = Some("blocked".to_string());
    blocked.blocked_reason = Some("waiting for CI".to_string());
    server
        .tachi_orchestrator(Parameters(blocked))
        .await
        .expect("block todo");

    let mut reopen = orchestrator_params("todo_update", task_id);
    reopen.todo_id = Some("t1".to_string());
    reopen.todo_status = Some("in_progress".to_string());
    server
        .tachi_orchestrator(Parameters(reopen))
        .await
        .expect("reopen todo");

    let list = server
        .tachi_orchestrator(Parameters(orchestrator_params("todo_list", task_id)))
        .await
        .expect("todo_list");
    let list_json: Value = serde_json::from_str(&list).expect("json");
    let todo = &list_json["todos"][0];
    assert_eq!(todo["status"], json!("in_progress"));
    assert!(todo["completed_at"].is_null());
    assert!(todo["blocked_reason"].is_null());
}
