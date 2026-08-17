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

// ─── Final dispatch JSON response ────────────────────────────────────────────

pub(super) struct DispatchResponseInputs<'a> {
    pub(super) dispatch_id: &'a str,
    pub(super) agent_norm: &'a str,
    pub(super) profile_payload: &'a Value,
    pub(super) resolved_profile: &'a ResolvedDispatchProfile,
    /// #894 S2d effective-authority receipt (compiled contract + enforcement).
    pub(super) authority: &'a Value,
    pub(super) credential_reports_json: &'a [Value],
    pub(super) capability_bundle_card: &'a Value,
    pub(super) capability_bundle_file: &'a str,
    pub(super) feedback_rules_trace: &'a Value,
    pub(super) harness_transport: &'a str,
    pub(super) harness_server_url: &'a Option<String>,
    pub(super) host_adapter: &'a Option<String>,
    pub(super) execution_backend_name: Option<&'static str>,
    pub(super) execution_backend_metadata: &'a Option<Value>,
    pub(super) acpx_enabled: bool,
    pub(super) native_acp_enabled: bool,
    pub(super) v2: bool,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) params: &'a TachiDispatchParams,
    pub(super) plan_path: &'a Path,
    pub(super) prompt_md_path: &'a Path,
    pub(super) context_md_path: &'a Path,
    pub(super) trajectory_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
}

/// tachi#1173 item 1: the dispatch state seeded by `build_dispatch_response`.
/// Every dispatch that reaches this point already returned (background work
/// runs after this response is built), so the state is always the initial
/// "accepted, working in background" value — matches the same literal used
/// by the receipt-first `write_status_json` seed in `dispatch.rs`.
const DISPATCH_RESPONSE_INITIAL_STATE: &str = "TASK_STATE_WORKING";

pub(super) fn build_dispatch_response(
    inputs: DispatchResponseInputs<'_>,
) -> Result<String, String> {
    // tachi#1173 item 1: dispatch receipt slimming. The default response is a
    // slim receipt (dispatch_id/state/run_dir/suggested_complete_command plus
    // other small metadata already useful post-dispatch); the fat routing
    // card (`profile` — the full `ResolvedDispatchProfile` — plus
    // `identity_receipt`) is selection-time information an agent needs
    // when CHOOSING a profile, not receipt information it needs after
    // dispatch already committed to one — so it moves behind verbose=true (or
    // a separate operator-only local `tachi card show` diagnostic).
    let verbose = inputs.params.verbose.unwrap_or(false);

    let mut response = json!({
        "dispatch_id": inputs.dispatch_id,
        "state": DISPATCH_RESPONSE_INITIAL_STATE,
        "task": {
            "id": inputs.dispatch_id,
            "status": { "state": DISPATCH_RESPONSE_INITIAL_STATE },
        },
        "agent": inputs.agent_norm,
        "selected_profile": inputs.resolved_profile.selected_profile,
        "authority": inputs.authority,
        "tool_access": inputs.resolved_profile.mcp_access,
        "credentials": inputs.credential_reports_json,
        "route_explanation": inputs.resolved_profile.route_explanation,
        "fallback_chain": inputs.resolved_profile.fallback_chain,
        "issue_ref": inputs.params.issue_ref,
        "pr_ref": inputs.params.pr_ref,
        "flow_id": inputs.params.flow_id,
        "auto_capability_bundle": inputs.resolved_profile.auto_capability_bundle,
        "capability_bundle": inputs.capability_bundle_card,
        "capability_bundle_file": inputs.capability_bundle_file,
        "feedback_rules": inputs.feedback_rules_trace,
        "harness_transport": inputs.harness_transport,
        "harness_server_url": inputs.harness_server_url,
        "host_adapter": inputs.host_adapter,
        "execution_backend": inputs.execution_backend_name,
        "acpx": if inputs.acpx_enabled { inputs.execution_backend_metadata.clone() } else { None },
        "acp_native": if inputs.native_acp_enabled { inputs.execution_backend_metadata.clone() } else { None },
        "v2": inputs.v2,
        "plan_review_status": if inputs.v2 { "approved" } else { "n/a" },
        "duration_ms_plan": inputs.plan_duration_ms,
        "message": "Task dispatched to background. You are unblocked. Use tachi_task(action='board') to check status.",
        "suggested_complete_command": suggested_complete_payload(inputs.dispatch_id, inputs.agent_norm, inputs.params),
        "plan_file": inputs.plan_path.to_string_lossy(),
        "prompt_file": inputs.prompt_md_path.to_string_lossy(),
        "context_file": inputs.context_md_path.to_string_lossy(),
        "trajectory_file": inputs.trajectory_path.to_string_lossy(),
        "run_dir": inputs.workspace_dir.to_string_lossy(),
        "verbose": verbose,
    });

    if verbose {
        let object = response
            .as_object_mut()
            .expect("build_dispatch_response always constructs a JSON object");
        object.insert("profile".to_string(), inputs.profile_payload.clone());
        object.insert(
            "identity_receipt".to_string(),
            serde_json::to_value(&inputs.resolved_profile.identity_receipt).unwrap_or(Value::Null),
        );
    }

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}
