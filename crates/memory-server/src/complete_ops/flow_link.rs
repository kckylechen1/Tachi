use crate::MemoryServer;
use memory_core::MemoryStore;

pub(super) fn resolve_flow_id_for_dispatch(
    server: &MemoryServer,
    dispatch_id: &str,
    explicit_flow_id: Option<&str>,
) -> Option<String> {
    if let Some(flow_id) = explicit_flow_id.map(str::trim).filter(|id| !id.is_empty()) {
        return Some(flow_id.to_string());
    }
    flow_id_from_kanban(server, dispatch_id)
        .or_else(|| flow_id_from_run_ledger(server, dispatch_id))
}

fn flow_id_from_kanban(server: &MemoryServer, dispatch_id: &str) -> Option<String> {
    let path = format!("/kanban/tasks/{dispatch_id}");
    let read = |store: &mut MemoryStore| -> Result<Option<String>, String> {
        let entries = store
            .list_by_path(&path, 1, false)
            .map_err(|e| format!("kanban flow_id lookup: {e}"))?;
        Ok(entries.into_iter().next().and_then(|entry| {
            entry
                .metadata
                .get("flow_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .filter(|flow_id| !flow_id.trim().is_empty())
        }))
    };
    if server.has_project_db() {
        if let Ok(value) = server.with_project_store_read(read) {
            if value.is_some() {
                return value;
            }
        }
    }
    server.with_global_store_read(read).ok().flatten()
}

fn flow_id_from_run_ledger(server: &MemoryServer, dispatch_id: &str) -> Option<String> {
    let task = crate::dispatch_ops::collect_run_task_for_server(server, dispatch_id)?;
    let run_dir = task.get("run_dir").and_then(serde_json::Value::as_str)?;
    let status_path = std::path::Path::new(run_dir).join("status.json");
    let status = crate::task_lifecycle::read_json_file(&status_path)
        .ok()
        .flatten()?;
    status
        .get("flow_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|flow_id| !flow_id.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_search_ops::handle_save_memory;
    use crate::tool_params::SaveMemoryParams;
    use serde_json::json;

    #[test]
    fn resolve_flow_id_reads_kanban_metadata_when_param_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let project_db = dir.path().join("project.sqlite");
        let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
        let dispatch_id = "20260702T011640Z-custom-flowlink";
        let flow_id = "flow_20260702T011640Z_flowlink_test";

        let save = SaveMemoryParams {
            text: "kanban card for flow link resolution".to_string(),
            summary: "kanban".to_string(),
            path: format!("/kanban/tasks/{dispatch_id}"),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec!["kanban".to_string()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "global".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: None,
            retention_policy: Some("pinned".to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "flow_id": flow_id,
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
            })),
            emit_continuity: false,
        };
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(handle_save_memory(&server, save))
            .expect("seed kanban");

        let resolved =
            resolve_flow_id_for_dispatch(&server, dispatch_id, None).expect("flow id from kanban");
        assert_eq!(resolved, flow_id);
    }
}
