use super::*;

pub(super) fn suggested_complete_payload(
    dispatch_id: &str,
    agent: &str,
    params: &TachiDispatchParams,
) -> serde_json::Value {
    json!({
        "tool": "tachi_task",
        "arguments": {
            "action": "complete",
            "dispatch_id": dispatch_id,
            "task": params.task,
            "agent": agent,
            "outcome": "success|failure|partial|aborted",
            "profile": params.profile,
            "flow_id": params.flow_id,
            "issue_ref": params.issue_ref,
            "pr_ref": params.pr_ref,
            "evidence_refs": [],
            "tests_run": [],
            "diff_present": null,
        }
    })
}

pub(super) fn capability_bundle_summary(
    trace: &serde_json::Value,
    artifact_file: Option<&str>,
) -> serde_json::Value {
    json!({
        "status": trace.get("status").cloned().unwrap_or(serde_json::Value::Null),
        "requested": trace.get("requested").and_then(|value| value.as_bool()).unwrap_or(false),
        "disabled": trace.get("disabled").and_then(|value| value.as_bool()).unwrap_or(false),
        "injected": trace.get("injected").and_then(|value| value.as_bool()).unwrap_or(false),
        "host": trace.get("host").cloned().unwrap_or(serde_json::Value::Null),
        "query": trace.get("query").cloned().unwrap_or(serde_json::Value::Null),
        "source": trace.get("source").cloned().unwrap_or(serde_json::Value::Null),
        "primary_skill": trace.get("primary_skill").cloned().unwrap_or(serde_json::Value::Null),
        "supporting_capabilities_count": trace
            .get("supporting_capabilities")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0),
        "packs_count": trace
            .get("packs")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0),
        "host_tools_count": trace
            .get("host_tools")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0),
        "reason": trace.get("reason").cloned().unwrap_or(serde_json::Value::Null),
        "error": trace.get("error").cloned().unwrap_or(serde_json::Value::Null),
        "artifact_file": artifact_file,
    })
}

/// Scope guard that deletes a temporary MCP config file when dropped.
/// Logs a warning if cleanup fails so leaking temp files is observable.
pub(super) struct McpCleanup(pub(super) Option<PathBuf>);

impl Drop for McpCleanup {
    fn drop(&mut self) {
        let Some(path) = self.0.take() else {
            return;
        };
        if !path.exists() {
            return;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(
                "failed to remove temporary MCP config file {}: {}",
                path.display(),
                e
            );
            return;
        }
        if path.exists() {
            tracing::warn!(
                "temporary MCP config file {} still exists after removal",
                path.display()
            );
        }
    }
}
