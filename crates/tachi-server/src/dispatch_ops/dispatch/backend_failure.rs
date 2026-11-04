use super::*;

pub(super) fn record_execution_backend_finalize_failure(
    server: &MemoryServer,
    ctx: ExecutionBackendPrepareFailure<'_>,
    mcp_config_path: Option<&Path>,
) -> String {
    // This phase follows credential materialization but precedes flow reservation
    // and worker spawn. Only the current run's ephemeral outputs are eligible.
    drop(McpCleanup(mcp_config_path.map(Path::to_path_buf)));
    let cleanup = server.with_global_store(|store| {
        cleanup_ephemeral_credential_materializations(store, ctx.workspace_dir, false)
    });
    let error = match cleanup {
        Ok(report) if report.errors.is_empty() => ctx.error.to_string(),
        Ok(report) => format!(
            "{}; temporary credential cleanup incomplete ({} errors)",
            ctx.error,
            report.errors.len()
        ),
        Err(_) => format!(
            "{}; temporary credential cleanup could not be confirmed",
            ctx.error
        ),
    };
    crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
        server,
        ctx.dispatch_id,
        "backend",
        Some(&ctx.assignment.selected_backend),
        ctx.request.project.as_deref(),
    );
    record_execution_backend_prepare_failure(ExecutionBackendPrepareFailure {
        error: &error,
        ..ctx
    });
    error
}

pub(super) struct ExecutionBackendPrepareFailure<'a> {
    pub(super) trajectory_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) dispatch_id: &'a str,
    pub(super) assignment: &'a tachi_params::ResolvedStaffAssignment,
    pub(super) request: &'a tachi_params::StaffAssignmentRequest,
    pub(super) backend: &'a str,
    pub(super) error: &'a str,
    pub(super) v2: bool,
    pub(super) plan_generated_at: Option<&'a str>,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) harness_transport: &'a str,
    pub(super) harness_server_url: &'a Option<String>,
    pub(super) timeout_secs_for_status: u64,
}

pub(super) fn record_execution_backend_prepare_failure(ctx: ExecutionBackendPrepareFailure<'_>) {
    append_trajectory_event(
        ctx.trajectory_path,
        json!({
            "event": "execution_backend_prepare_failed",
            "dispatch_id": ctx.dispatch_id,
            "agent": ctx.assignment.selected_backend,
            "execution_backend": ctx.backend,
            "error": ctx.error,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );
    write_status_json(
        ctx.workspace_dir,
        ctx.dispatch_id,
        ctx.v2,
        ctx.plan_generated_at,
        None,
        if ctx.v2 { "approved" } else { "n/a" },
        Some(1),
        ctx.plan_duration_ms,
        None,
        ctx.plan_duration_ms,
        Some(json!({
            "agent": ctx.assignment.selected_backend,
            "task": ctx.request.task.clone(),
            "state": "TASK_STATE_FAILED",
            "updated_at": Utc::now().to_rfc3339(),
            "run_dir": ctx.workspace_dir.to_string_lossy(),
            "result_written": false,
            "harness_transport": ctx.harness_transport,
            "harness_server_url": ctx.harness_server_url,
            "execution_backend": ctx.backend,
            "timeout_secs": ctx.timeout_secs_for_status,
            "error": ctx.error,
        })),
    );
}
