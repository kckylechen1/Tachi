use crate::MemoryServer;

#[derive(Debug)]
pub(super) struct KanbanSnapshot {
    pub(super) scope: &'static str,
    pub(super) state: Option<String>,
    pub(super) eval_ledger_id: Option<String>,
    pub(super) reviewed: Option<bool>,
}

pub(super) fn read_kanban_snapshot(
    server: &MemoryServer,
    dispatch_id: &str,
) -> Result<Option<KanbanSnapshot>, String> {
    let path = format!("/kanban/tasks/{dispatch_id}");
    let project_entries = if server.has_project_db() {
        server.with_project_store_read(|store| {
            store
                .list_by_path(&path, 1, false)
                .map_err(|e| format!("kanban list_by_path: {e}"))
        })?
    } else {
        Vec::new()
    };
    if let Some(entry) = project_entries.first() {
        return Ok(Some(KanbanSnapshot {
            scope: "project",
            state: entry
                .metadata
                .get("a2a_state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            eval_ledger_id: entry
                .metadata
                .get("eval_ledger_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            reviewed: entry
                .metadata
                .get("reviewed")
                .and_then(serde_json::Value::as_bool),
        }));
    }

    let global_entries = server.with_global_store_read(|store| {
        store
            .list_by_path(&path, 1, false)
            .map_err(|e| format!("kanban list_by_path (global): {e}"))
    })?;
    let Some(entry) = global_entries.first() else {
        return Ok(None);
    };
    Ok(Some(KanbanSnapshot {
        scope: "global",
        state: entry
            .metadata
            .get("a2a_state")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        eval_ledger_id: entry
            .metadata
            .get("eval_ledger_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        reviewed: entry
            .metadata
            .get("reviewed")
            .and_then(serde_json::Value::as_bool),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_kanban_snapshot_surfaces_project_read_failures() {
        let dir = tempfile::tempdir().expect("temp dir");
        let global_db = dir.path().join("global").join("memory.db");
        let project_db = dir.path().join("project").join("memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent"))
            .expect("create global parent");
        std::fs::create_dir_all(project_db.parent().expect("project parent"))
            .expect("create project parent");
        let server =
            MemoryServer::new(global_db, Some(project_db)).expect("server with project db");

        let project_db = server
            .project_db_path_buf()
            .expect("server should bind project database");
        crate::test_support::with_unrestricted_fixture_connection(&project_db, |connection| {
            connection.execute("DROP TABLE memories", []).map(|_| ())
        })
        .expect("break project memories table");

        let err = read_kanban_snapshot(&server, "dispatch-readback-failure")
            .expect_err("project read failure should not be treated as a missing card");
        assert!(err.contains("kanban list_by_path"), "{err}");
    }
}
