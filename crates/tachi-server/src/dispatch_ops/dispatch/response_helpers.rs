use super::*;

pub(super) fn suggested_complete_payload(
    dispatch_id: &str,
    assignment: &tachi_params::ResolvedStaffAssignment,
    request: &tachi_params::StaffAssignmentRequest,
) -> serde_json::Value {
    json!({
        "tool": "tachi_task",
        "arguments": {
            "action": "complete",
            "dispatch_id": dispatch_id,
            "task": request.task,
            "agent": assignment.selected_backend,
            "outcome": "success|failure|partial|aborted",
            "profile": request.profile,
            "flow_id": request.flow_id,
            "issue_ref": request.issue_ref,
            "pr_ref": request.pr_ref,
            "evidence_refs": [],
            "tests_run": [],
            "diff_present": null,
        }
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
    pub(super) assignment: &'a tachi_params::ResolvedStaffAssignment,
    pub(super) profile_payload: &'a Value,
    pub(super) resolved_profile: &'a ResolvedDispatchProfile,
    /// #894 S2d effective-authority receipt (compiled contract + enforcement).
    pub(super) authority: &'a Value,
    pub(super) credential_reports_json: &'a [Value],
    pub(super) feedback_rules_trace: &'a Value,
    pub(super) harness_transport: &'a str,
    pub(super) harness_server_url: &'a Option<String>,
    pub(super) execution_backend_name: Option<&'static str>,
    pub(super) execution_backend_metadata: &'a Option<Value>,
    pub(super) acpx_enabled: bool,
    pub(super) native_acp_enabled: bool,
    pub(super) v2: bool,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) request: &'a tachi_params::StaffAssignmentRequest,
    pub(super) verbose: bool,
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
    // card (`profile`, identity_receipt) is selection-time information an agent needs
    // when CHOOSING a profile, not receipt information it needs after
    // dispatch already committed to one — so it moves behind verbose=true (or
    // a separate operator-only local `tachi card show` diagnostic).
    let verbose = inputs.verbose;

    let mut response = json!({
        "dispatch_id": inputs.dispatch_id,
        "state": DISPATCH_RESPONSE_INITIAL_STATE,
        "task": {
            "id": inputs.dispatch_id,
            "status": { "state": DISPATCH_RESPONSE_INITIAL_STATE },
        },
        "agent": inputs.assignment.selected_backend,
        "selected_profile": inputs.assignment.selected_profile,
        "authority": inputs.authority,
        "tool_access": inputs.resolved_profile.mcp_access,
        "credentials": inputs.credential_reports_json,
        "route_explanation": inputs.assignment.route_explanation,
        "fallback_chain": inputs.assignment.fallback_chain,
        "issue_ref": inputs.request.issue_ref,
        "pr_ref": inputs.request.pr_ref,
        "flow_id": inputs.request.flow_id,
        "feedback_rules": inputs.feedback_rules_trace,
        "harness_transport": inputs.harness_transport,
        "harness_server_url": inputs.harness_server_url,
        "host_adapter": inputs.assignment.host_adapter,
        "execution_backend": inputs.execution_backend_name,
        "acpx": if inputs.acpx_enabled { inputs.execution_backend_metadata.clone() } else { None },
        "acp_native": if inputs.native_acp_enabled { inputs.execution_backend_metadata.clone() } else { None },
        "v2": inputs.v2,
        "plan_review_status": if inputs.v2 { "approved" } else { "n/a" },
        "duration_ms_plan": inputs.plan_duration_ms,
        "message": "Task dispatched to background. You are unblocked. Use tachi_task(action='board') to check status.",
        "suggested_complete_command": suggested_complete_payload(inputs.dispatch_id, inputs.assignment, inputs.request),
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
            inputs.assignment.identity_receipt.clone(),
        );
    }

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}
