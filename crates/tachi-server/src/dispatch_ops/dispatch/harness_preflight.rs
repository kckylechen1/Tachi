use super::*;

// ─── opencode_serve harness preflight ────────────────────────────────────────

pub(super) struct HarnessPreflightInputs<'a> {
    pub(super) server: &'a MemoryServer,
    pub(super) harness_transport: &'a str,
    pub(super) harness_server_url: &'a Option<String>,
    pub(super) credential_env: &'a HashMap<String, String>,
    pub(super) dispatch_id: &'a str,
    pub(super) assignment: &'a tachi_params::ResolvedStaffAssignment,
    pub(super) task: &'a str,
    /// The semantic request's `project`, threaded through so a
    /// preflight-failure terminal outcome row lands in the same DB a later
    /// `tachi_complete` for this dispatch would resolve to (scope symmetry,
    /// #774 round 2).
    pub(super) project: Option<&'a str>,
    pub(super) trajectory_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) v2: bool,
    pub(super) plan_generated_at: Option<&'a str>,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) host_adapter: &'a Option<String>,
    pub(super) execution_backend_name: Option<&'static str>,
    pub(super) execution_backend_metadata: &'a Option<Value>,
    pub(super) acpx_enabled: bool,
    pub(super) native_acp_enabled: bool,
    pub(super) capability_bundle_card: &'a Value,
    pub(super) timeout_secs_for_status: u64,
}

/// Probe the opencode_serve harness for readiness before spawning. If the
/// attach probe fails, writes result.md + status.json with the full backend
/// error context and returns Err. Non-opencode_serve transports skip the
/// check entirely.
pub(super) fn run_harness_preflight(inputs: HarnessPreflightInputs<'_>) -> Result<(), String> {
    if !is_opencode_serve_transport(inputs.harness_transport) {
        return Ok(());
    }
    let harness_status = crate::dispatch_ops::probe_harness_server_status_with_env(
        inputs.harness_server_url.as_deref(),
        Some(inputs.credential_env),
    );
    let attach_ready = harness_status
        .get("attach_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !attach_ready {
        let err =
            opencode_serve_preflight_error(&harness_status, inputs.harness_server_url.as_deref());
        append_trajectory_event(
            inputs.trajectory_path,
            json!({
                "event": "harness_preflight_failed",
                "dispatch_id": inputs.dispatch_id,
                "agent": inputs.assignment.selected_worker,
                "harness_transport": inputs.harness_transport,
                "harness_server_url": inputs.harness_server_url,
                "harness_server_status": harness_status.clone(),
                "error": err.clone(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
        let result_written = crate::utils::write_owner_only_file_atomic(
            &inputs.workspace_dir.join("result.md"),
            err.as_bytes(),
        )
        .is_ok();
        write_status_json(
            inputs.workspace_dir,
            inputs.dispatch_id,
            inputs.v2,
            inputs.plan_generated_at,
            None,
            if inputs.v2 { "approved" } else { "n/a" },
            Some(1),
            inputs.plan_duration_ms,
            None,
            inputs.plan_duration_ms,
            Some(json!({
                "agent": inputs.assignment.selected_worker,
                "task": inputs.task,
                "state": "TASK_STATE_FAILED",
                "updated_at": Utc::now().to_rfc3339(),
                "run_dir": inputs.workspace_dir.to_string_lossy(),
                "result_written": result_written,
                "harness_transport": inputs.harness_transport,
                "harness_server_url": inputs.harness_server_url,
                "harness_server_status": harness_status,
                "host_adapter": inputs.host_adapter,
                "execution_backend": inputs.execution_backend_name,
                "acpx": if inputs.acpx_enabled { inputs.execution_backend_metadata.clone() } else { None },
                "acp_native": if inputs.native_acp_enabled { inputs.execution_backend_metadata.clone() } else { None },
                "capability_bundle": inputs.capability_bundle_card.clone(),
                "timeout_secs": inputs.timeout_secs_for_status,
                "error": err.clone(),
            })),
        );
        // #773 Layer-2 ② (hole b): preflight failure is a terminal dispatch
        // state the agent never `tachi_complete`s — record a canonical outcome
        // row (first-writer-wins on dispatch_id) so the router sees it.
        crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
            inputs.server,
            inputs.dispatch_id,
            "preflight",
            Some(&inputs.assignment.selected_worker),
            inputs.project,
        );
        return Err(err);
    }
    Ok(())
}
