use crate::MemoryServer;
use memcore::MemoryStore;

pub(super) fn resolve_flow_id_for_dispatch(
    server: &MemoryServer,
    dispatch_id: &str,
    explicit_flow_id: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(flow_id) = explicit_flow_id.map(str::trim).filter(|id| !id.is_empty()) {
        return Ok(Some(flow_id.to_string()));
    }
    if let Some(flow_id) = flow_id_from_kanban(server, dispatch_id) {
        return Ok(Some(flow_id));
    }
    flow_id_from_run_ledger(server, dispatch_id)
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

fn flow_id_from_run_ledger(
    server: &MemoryServer,
    dispatch_id: &str,
) -> Result<Option<String>, String> {
    let Some(task) = crate::dispatch_ops::collect_run_task_for_server(server, dispatch_id)? else {
        return Ok(None);
    };
    Ok(task
        .get("flow_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|flow_id| !flow_id.trim().is_empty()))
}

/// #773 (S2 prep, sol-terminal-review-certified): auto-inject `issue_ref` at
/// completion time when the caller supplied `dispatch_id` but not
/// `issue_ref`. The dispatch's own kanban card already carries `issue_ref`
/// verbatim from launch (`init_kanban_task` writes `params.issue_ref` into
/// `/kanban/tasks/{dispatch_id}` metadata unconditionally — see
/// `dispatch_ops::kanban_helpers`), so this closes the propagation gap
/// without requiring the calling agent to manually re-supply it. Mirrors
/// `resolve_flow_id_for_dispatch`'s explicit-value-wins-then-lookup shape.
/// Fail-safe: any lookup miss (no card, no project/global store, no
/// `issue_ref` on file) returns `None` — callers must never fail completion
/// over a missing provenance value.
pub(super) fn resolve_issue_ref_for_dispatch(
    server: &MemoryServer,
    dispatch_id: &str,
    explicit_issue_ref: Option<&str>,
) -> Option<String> {
    if let Some(issue_ref) = explicit_issue_ref.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(issue_ref.to_string());
    }
    issue_ref_from_kanban(server, dispatch_id)
}

fn issue_ref_from_kanban(server: &MemoryServer, dispatch_id: &str) -> Option<String> {
    let path = format!("/kanban/tasks/{dispatch_id}");
    let read = |store: &mut MemoryStore| -> Result<Option<String>, String> {
        let entries = store
            .list_by_path(&path, 1, false)
            .map_err(|e| format!("kanban issue_ref lookup: {e}"))?;
        Ok(entries.into_iter().next().and_then(|entry| {
            entry
                .metadata
                .get("issue_ref")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .filter(|issue_ref| !issue_ref.trim().is_empty())
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

/// tachi#1200 item 1: eval/subagent rows written by `tachi_complete` without a
/// leader `profile` id can never be matched by policy replay
/// (`tachi_tune(action='route_simulate')`/`tachi_task(action='recommend')`) — both filter live
/// `/eval` rows on an exact `EvalRow.profile` match against a known dispatch
/// profile name (see `tachi_dispatch::routing::simulate_route_policy` and
/// `build_profile_candidate`'s `live_samples` count), so a `None` profile
/// silently drops the row out of every replay computation rather than merely
/// degrading it.
///
/// The dispatch's own kanban card already has `profile` on file from launch
/// (`init_kanban_task` writes `params.profile` into
/// `/kanban/tasks/{dispatch_id}` metadata unconditionally — see
/// `dispatch_ops::kanban_helpers`), so auto-inject it here the same
/// explicit-value-wins-then-lookup shape as `resolve_issue_ref_for_dispatch`.
/// Fail-safe: any lookup miss (no card, no project/global store, no `profile`
/// on file) returns `None` — a manual `tachi_complete` call with no
/// `dispatch_id` has nothing to look up and must NEVER have a profile
/// fabricated for it.
pub(super) fn resolve_profile_for_dispatch(
    server: &MemoryServer,
    dispatch_id: &str,
    explicit_profile: Option<&str>,
) -> Option<String> {
    if let Some(profile) = explicit_profile.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(profile.to_string());
    }
    profile_from_kanban(server, dispatch_id)
}

fn profile_from_kanban(server: &MemoryServer, dispatch_id: &str) -> Option<String> {
    let path = format!("/kanban/tasks/{dispatch_id}");
    let read = |store: &mut MemoryStore| -> Result<Option<String>, String> {
        let entries = store
            .list_by_path(&path, 1, false)
            .map_err(|e| format!("kanban profile lookup: {e}"))?;
        Ok(entries.into_iter().next().and_then(|entry| {
            entry
                .metadata
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .filter(|profile| !profile.trim().is_empty())
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
            project_explicit: false,
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

        let resolved = resolve_flow_id_for_dispatch(&server, dispatch_id, None)
            .expect("flow id lookup")
            .expect("flow id from kanban");
        assert_eq!(resolved, flow_id);
    }

    #[test]
    fn resolve_issue_ref_reads_kanban_metadata_when_param_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let project_db = dir.path().join("project.sqlite");
        let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
        let dispatch_id = "20260702T011640Z-custom-issueref";
        let issue_ref = "kckylechen1/tachi#773";

        let save = SaveMemoryParams {
            text: "kanban card for issue_ref auto-inject resolution".to_string(),
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
            project_explicit: false,
            retention_policy: Some("pinned".to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "issue_ref": issue_ref,
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
            })),
            emit_continuity: false,
        };
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(handle_save_memory(&server, save))
            .expect("seed kanban");

        let resolved = resolve_issue_ref_for_dispatch(&server, dispatch_id, None)
            .expect("issue_ref from kanban");
        assert_eq!(resolved, issue_ref);
    }

    #[test]
    fn resolve_issue_ref_prefers_explicit_value_over_kanban() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let project_db = dir.path().join("project.sqlite");
        let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
        let dispatch_id = "20260702T011640Z-explicit-issueref";

        let save = SaveMemoryParams {
            text: "kanban card".to_string(),
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
            project_explicit: false,
            retention_policy: Some("pinned".to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "issue_ref": "kckylechen1/tachi#000-stale",
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
            })),
            emit_continuity: false,
        };
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(handle_save_memory(&server, save))
            .expect("seed kanban");

        let resolved =
            resolve_issue_ref_for_dispatch(&server, dispatch_id, Some("kckylechen1/tachi#773"))
                .expect("explicit issue_ref wins");
        assert_eq!(resolved, "kckylechen1/tachi#773");
    }

    #[test]
    fn resolve_issue_ref_returns_none_when_no_dispatch_record_or_issue_ref() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let project_db = dir.path().join("project.sqlite");
        let server = MemoryServer::new(global_db, Some(project_db)).expect("server");

        let resolved =
            resolve_issue_ref_for_dispatch(&server, "20260702T011640Z-no-such-dispatch", None);
        assert_eq!(resolved, None);
    }

    #[test]
    fn resolve_profile_reads_kanban_metadata_when_param_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let project_db = dir.path().join("project.sqlite");
        let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
        let dispatch_id = "20260717T000001Z-custom-profilelink";
        let profile = "opencode_builder";

        let save = SaveMemoryParams {
            text: "kanban card for profile linkage resolution".to_string(),
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
            project_explicit: false,
            retention_policy: Some("pinned".to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "profile": profile,
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
            })),
            emit_continuity: false,
        };
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(handle_save_memory(&server, save))
            .expect("seed kanban");

        let resolved =
            resolve_profile_for_dispatch(&server, dispatch_id, None).expect("profile from kanban");
        assert_eq!(resolved, profile);
    }

    #[test]
    fn resolve_profile_prefers_explicit_value_over_kanban() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let project_db = dir.path().join("project.sqlite");
        let server = MemoryServer::new(global_db, Some(project_db)).expect("server");
        let dispatch_id = "20260717T000002Z-explicit-profilelink";

        let save = SaveMemoryParams {
            text: "kanban card".to_string(),
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
            project_explicit: false,
            retention_policy: Some("pinned".to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "profile": "stale_profile",
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
            })),
            emit_continuity: false,
        };
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(handle_save_memory(&server, save))
            .expect("seed kanban");

        let resolved = resolve_profile_for_dispatch(&server, dispatch_id, Some("glm_impl"))
            .expect("explicit profile wins");
        assert_eq!(resolved, "glm_impl");
    }

    #[test]
    fn resolve_profile_returns_none_when_no_dispatch_record_or_profile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let project_db = dir.path().join("project.sqlite");
        let server = MemoryServer::new(global_db, Some(project_db)).expect("server");

        let resolved =
            resolve_profile_for_dispatch(&server, "20260717T000003Z-no-such-dispatch", None);
        assert_eq!(resolved, None);
    }
}
