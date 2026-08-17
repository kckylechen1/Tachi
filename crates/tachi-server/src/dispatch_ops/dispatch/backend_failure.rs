use super::*;

pub(super) struct ExecutionBackendPrepareFailure<'a> {
    pub(super) trajectory_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) dispatch_id: &'a str,
    pub(super) agent_norm: &'a str,
    pub(super) params: &'a TachiDispatchParams,
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
            "agent": ctx.agent_norm,
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
            "agent": ctx.agent_norm,
            "task": ctx.params.task.clone(),
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
