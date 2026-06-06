use super::*;

// ─── Kanban helpers ────────────────────────────────────────────────────────────

/// Initialize a kanban task entry in the memory DB
pub(super) async fn init_kanban_task(
    server: &MemoryServer,
    dispatch_id: &str,
    params: &TachiDispatchParams,
    plan_path: Option<&str>,
) -> Result<(), String> {
    let agent = params.agent.as_deref().unwrap_or("unknown");
    let text = format!(
        "Dispatch Task\nAgent: {}\nTask: {}\nPlan: {}",
        agent,
        params.task,
        plan_path.unwrap_or("inline"),
    );
    let metadata = json!({
        "type": "a2a_task",
        "dispatch_id": dispatch_id,
        "a2a_state": "TASK_STATE_WORKING",
        "agent": agent,
        "profile": params.profile,
        "tool_profile": params.tool_profile,
        "mcp_access": params.mcp_access,
        "allowed_mcp_servers": params.allowed_mcp_servers,
        "issue_ref": params.issue_ref,
        "pr_ref": params.pr_ref,
        "flow_id": params.flow_id,
        "auto_capability_bundle": params.auto_capability_bundle,
        "plan_file": plan_path,
        "eval_ledger_id": null,
    });

    crate::memory_search_ops::handle_save_memory(
        server,
        SaveMemoryParams {
            text,
            summary: format!(
                "Kanban: {} via {}",
                params.task.chars().take(80).collect::<String>(),
                agent
            ),
            path: format!("/kanban/tasks/{}", dispatch_id),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec![
                "kanban".to_string(),
                "dispatch".to_string(),
                agent.to_string(),
            ],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            retention_policy: Some("durable".to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(metadata),
        },
    )
    .await?;
    Ok(())
}

pub(super) async fn get_kanban_state(server: &MemoryServer, dispatch_id: &str) -> Option<String> {
    let path = format!("/kanban/tasks/{}", dispatch_id);
    // Use exact path SQL query instead of semantic search to avoid
    // Foundry inline-merge returning the wrong (merged) record.
    //
    // Scope resolution: `init_kanban_task` writes with scope="project", but
    // `handle_save_memory` falls back to global when no project DB exists.
    // We must mirror that fallback here so daemon/no-project dispatches don't
    // get stuck in TASK_STATE_WORKING.
    let entries = server
        .with_project_store(|store| {
            store
                .list_by_path(&path, 1, false)
                .map_err(|e| format!("kanban list_by_path: {e}"))
        })
        .unwrap_or_default();

    let entries = if entries.is_empty() {
        server
            .with_global_store(|store| {
                store
                    .list_by_path(&path, 1, false)
                    .map_err(|e| format!("kanban list_by_path (global): {e}"))
            })
            .unwrap_or_default()
    } else {
        entries
    };

    for entry in &entries {
        if let Some(state) = entry.metadata.get("a2a_state").and_then(|v| v.as_str()) {
            return Some(state.to_string());
        }
    }
    None
}

/// Update kanban task state.
///
/// `reviewed` flips the `metadata.reviewed` flag on the kanban row. The
/// status dashboard surfaces completed/success dispatches without this
/// flag as "unreviewed". Explicit `tachi_complete` calls should mark
/// the task reviewed; the watchdog auto-close path must NOT, so human
/// operators can still distinguish agent-closed tasks from auto-closed
/// ones.
pub(crate) async fn update_kanban_state(
    server: &MemoryServer,
    dispatch_id: &str,
    new_state: &str,
    eval_id: Option<&str>,
    reviewed: Option<bool>,
) -> Result<(), String> {
    let path = format!("/kanban/tasks/{}", dispatch_id);
    // Use exact path SQL query instead of semantic search to avoid
    // Foundry inline-merge returning the wrong (merged) record.
    //
    // Scope resolution: try project store first; fall back to global if no
    // project DB exists. We must write the update back to the same store the
    // entry was found in, otherwise we leave a stale row.
    let project_entries = server
        .with_project_store(|store| {
            store
                .list_by_path(&path, 1, false)
                .map_err(|e| format!("kanban list_by_path: {e}"))
        })
        .unwrap_or_default();

    let (entries, write_scope) = if project_entries.is_empty() {
        let global_entries = server
            .with_global_store(|store| {
                store
                    .list_by_path(&path, 1, false)
                    .map_err(|e| format!("kanban list_by_path (global): {e}"))
            })
            .unwrap_or_default();
        (global_entries, "global")
    } else {
        (project_entries, "project")
    };

    if let Some(entry) = entries.first() {
        let mut meta = entry.metadata.clone();
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("a2a_state".to_string(), json!(new_state));
            if let Some(eid) = eval_id {
                obj.insert("eval_ledger_id".to_string(), json!(eid));
            }
            if let Some(flag) = reviewed {
                obj.insert("reviewed".to_string(), json!(flag));
            }
            obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        }
        crate::memory_search_ops::handle_save_memory(
            server,
            SaveMemoryParams {
                text: entry.text.clone(),
                summary: format!("Kanban [{}]: {}", new_state, dispatch_id),
                path,
                importance: 0.7,
                category: "fact".to_string(),
                topic: "kanban".to_string(),
                keywords: vec!["kanban".to_string()],
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                scope: write_scope.to_string(),
                vector: None,
                id: Some(entry.id.clone()),
                force: true,
                auto_link: true,
                project: None,
                retention_policy: Some("durable".to_string()),
                domain: Some("system".to_string()),
                timestamp: None,
                valid_from: None,
                valid_until: None,
                metadata: Some(meta),
            },
        )
        .await?;
    }
    Ok(())
}

pub(crate) fn should_cleanup_run(exit_code: Option<i32>, kanban_state: Option<&str>) -> bool {
    exit_code == Some(0) && kanban_state == Some("TASK_STATE_COMPLETED")
}
