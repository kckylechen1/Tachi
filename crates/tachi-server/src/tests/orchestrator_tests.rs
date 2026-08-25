use super::make_server;
use crate::orchestrator_ops::set_todo_update_snapshot_hook;
use crate::tool_params::TachiOrchestratorParams;
use crate::MemoryServer;
use serde_json::{json, Value};

async fn call_orchestrator(
    server: &MemoryServer,
    params: TachiOrchestratorParams,
) -> Result<String, String> {
    crate::orchestrator_ops::handle_orchestrator(server, params).await
}

fn orchestrator_params(action: &str, task_id: &str) -> TachiOrchestratorParams {
    TachiOrchestratorParams {
        action: action.to_string(),
        task_id: Some(task_id.to_string()),
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
async fn orchestrator_recovery_briefing_infers_active_task() {
    let server = make_server();
    let mut update = orchestrator_params("todo_update", "active-task-001");
    update.todo_id = Some("t1".to_string());
    update.todo_content = Some("Continue the active task".to_string());
    update.todo_status = Some("in_progress".to_string());
    call_orchestrator(&server, update)
        .await
        .expect("todo_update");

    let mut recovery = orchestrator_params("recovery_briefing", "unused");
    recovery.task_id = None;
    let raw = call_orchestrator(&server, recovery)
        .await
        .expect("recovery without task_id");
    let json: Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(json["task_id"], json!("active-task-001"));
    assert_eq!(json["incomplete_todos"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn orchestrator_recovery_briefing_ignores_completed_todo_without_handoff() {
    let server = make_server();
    let mut update = orchestrator_params("todo_update", "completed-task-001");
    update.todo_id = Some("t1".to_string());
    update.todo_content = Some("Already done".to_string());
    update.todo_status = Some("done".to_string());
    call_orchestrator(&server, update)
        .await
        .expect("todo_update");

    let mut recovery = orchestrator_params("recovery_briefing", "unused");
    recovery.task_id = None;
    let err = call_orchestrator(&server, recovery)
        .await
        .expect("completed-only task should return empty recovery, not an MCP error");
    let json: Value = serde_json::from_str(&err).expect("json");
    assert!(json["task_id"].is_null());
    assert_eq!(json["incomplete_todos"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn orchestrator_todos_and_handoff_persist() {
    let server = make_server();
    let task_id = "dispatch-test-001";

    let update = call_orchestrator(
        &server,
        TachiOrchestratorParams {
            action: "todo_update".to_string(),
            task_id: Some(task_id.to_string()),
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
        },
    )
    .await
    .expect("todo_update");
    let update_json: Value = serde_json::from_str(&update).expect("json");
    assert_eq!(update_json["ok"], json!(true));

    let list = call_orchestrator(
        &server,
        TachiOrchestratorParams {
            action: "todo_list".to_string(),
            task_id: Some(task_id.to_string()),
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
        },
    )
    .await
    .expect("todo_list");
    let list_json: Value = serde_json::from_str(&list).expect("json");
    assert_eq!(list_json["todos"].as_array().map(|a| a.len()), Some(1));

    let handoff = call_orchestrator(
        &server,
        TachiOrchestratorParams {
            action: "handoff_write".to_string(),
            task_id: Some(task_id.to_string()),
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
        },
    )
    .await
    .expect("handoff_write");

    let recovery = call_orchestrator(
        &server,
        TachiOrchestratorParams {
            action: "recovery_briefing".to_string(),
            task_id: Some(task_id.to_string()),
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
        },
    )
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
    call_orchestrator(&server, create)
        .await
        .expect("create done todo");

    let mut content_only = orchestrator_params("todo_update", task_id);
    content_only.todo_id = Some("t1".to_string());
    content_only.todo_content = Some("Finish merge after CI".to_string());
    call_orchestrator(&server, content_only)
        .await
        .expect("content-only update");

    let list = call_orchestrator(&server, orchestrator_params("todo_list", task_id))
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
    call_orchestrator(&server, blocked)
        .await
        .expect("block todo");

    let mut reopen = orchestrator_params("todo_update", task_id);
    reopen.todo_id = Some("t1".to_string());
    reopen.todo_status = Some("in_progress".to_string());
    call_orchestrator(&server, reopen)
        .await
        .expect("reopen todo");

    let list = call_orchestrator(&server, orchestrator_params("todo_list", task_id))
        .await
        .expect("todo_list");
    let list_json: Value = serde_json::from_str(&list).expect("json");
    let todo = &list_json["todos"][0];
    assert_eq!(todo["status"], json!("in_progress"));
    assert!(todo["completed_at"].is_null());
    assert!(todo["blocked_reason"].is_null());
}

#[tokio::test]
async fn todo_update_retries_after_a_competing_first_insert() {
    let server = make_server();
    let task_id = "dispatch-test-cas-first-insert";
    let key = format!("todos:{task_id}");
    set_todo_update_snapshot_hook(key.clone(), move |server, key| {
        let competing_list = json!({
            "task_id": task_id,
            "todos": [{
                "id": "competing",
                "issue_ref": null,
                "parent_id": null,
                "agent": "other-session",
                "status": "in_progress",
                "content": "Keep this independent update",
                "blocked_reason": null,
                "verification": null,
                "references": [],
                "created_at": "2026-07-11T00:00:00Z",
                "updated_at": "2026-07-11T00:00:00Z",
                "completed_at": null,
            }],
            "updated_at": "2026-07-11T00:00:00Z",
        });
        server
            .with_global_store(|store| {
                store
                    .set_state("orchestrator", key, &competing_list.to_string())
                    .map(|_| ())
                    .map_err(|error| format!("seed competing todo update: {error}"))
            })
            .expect("seed competing todo update");
    });

    let mut update = orchestrator_params("todo_update", task_id);
    update.todo_id = Some("ours".to_string());
    update.todo_content = Some("Keep this update too".to_string());
    update.todo_status = Some("in_progress".to_string());
    let raw = call_orchestrator(&server, update)
        .await
        .expect("todo update retries after competing insert");
    let receipt: Value = serde_json::from_str(&raw).expect("receipt JSON");
    assert_eq!(receipt["todo_count"], json!(2));

    let raw = call_orchestrator(&server, orchestrator_params("todo_list", task_id))
        .await
        .expect("todo list");
    let list: Value = serde_json::from_str(&raw).expect("todo list JSON");
    let ids = list["todos"]
        .as_array()
        .expect("todos array")
        .iter()
        .filter_map(|todo| todo["id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        ids.contains(&"competing"),
        "competing update was lost: {list}"
    );
    assert!(
        ids.contains(&"ours"),
        "caller update was not persisted: {list}"
    );
}
