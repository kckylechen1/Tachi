use super::*;

// ─── Legacy vault env injection + credential materialization ─────────────────

/// Inject unlocked vault secrets into the execution environment, appending a
/// trajectory event when any env vars are injected. Mutates `execution` in
/// place. Kept synchronous because there are no await points in this block.
pub(super) fn inject_legacy_vault_env(
    server: &MemoryServer,
    cwd: Option<&Path>,
    execution: &mut DispatchExecution,
    trajectory_path: &Path,
    dispatch_id: &str,
    assignment: &tachi_params::ResolvedStaffAssignment,
) {
    let legacy_vault_env = unlocked_vault_child_env_map(server, cwd);
    let legacy_vault_env_count = legacy_vault_env.len();
    if legacy_vault_env_count > 0 {
        match execution {
            DispatchExecution::Subprocess(cmd) | DispatchExecution::ManagedCustom(cmd, _) => {
                for (name, value) in &legacy_vault_env {
                    cmd.env(name, value);
                }
            }
            DispatchExecution::NativeAcp(spec) => {
                spec.env.extend(legacy_vault_env.clone());
            }
        }
    }
    if legacy_vault_env_count > 0 {
        append_trajectory_event(
            trajectory_path,
            json!({
                "event": "legacy_vault_env_injected",
                "dispatch_id": dispatch_id,
                "agent": assignment.selected_worker,
                "count": legacy_vault_env_count,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
    }
}

pub(super) struct CredentialApplyInputs<'a> {
    pub(super) server: &'a MemoryServer,
    pub(super) request: &'a tachi_params::StaffAssignmentRequest,
    pub(super) grant: &'a tachi_params::ExecutionGrant,
    pub(super) raw_credential_profiles: &'a [String],
    pub(super) assignment: &'a tachi_params::ResolvedStaffAssignment,
    pub(super) workspace_dir: &'a Path,
    pub(super) trajectory_path: &'a Path,
    pub(super) dispatch_id: &'a str,
    pub(super) v2: bool,
    pub(super) plan_generated_at: Option<&'a str>,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) harness_transport: &'a str,
    pub(super) harness_server_url: &'a Option<String>,
    pub(super) execution_backend_name: Option<&'static str>,
    pub(super) execution_backend_metadata: &'a Option<Value>,
    pub(super) acpx_enabled: bool,
    pub(super) native_acp_enabled: bool,
    pub(super) capability_bundle_card: &'a Value,
    pub(super) timeout_secs_for_status: u64,
}

pub(super) struct CredentialApplyOutcome {
    pub(super) env: HashMap<String, String>,
    pub(super) reports_json: Vec<Value>,
}

/// Materialize dispatch credentials, inject env into execution, and emit
/// trajectory events. On failure writes the full status.json error payload
/// (including backend metadata) and returns Err.
pub(super) fn apply_materialized_credentials(
    inputs: CredentialApplyInputs<'_>,
    execution: &mut DispatchExecution,
) -> Result<CredentialApplyOutcome, String> {
    let dispatch_credentials = match materialize_dispatch_credentials(
        inputs.server,
        inputs.grant,
        &inputs.assignment.selected_worker,
        inputs.assignment.selected_profile.as_deref(),
        inputs.workspace_dir,
    ) {
        Ok(materialized) => materialized,
        Err(err) => {
            append_trajectory_event(
                inputs.trajectory_path,
                json!({
                    "event": "credentials_materialization_failed",
                    "dispatch_id": inputs.dispatch_id,
                    "agent": inputs.assignment.selected_worker,
                    "host_adapter": inputs.assignment.host_adapter,
                    "credential_profiles": inputs.raw_credential_profiles,
                    "error": err.clone(),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
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
                    "task": inputs.request.task.clone(),
                    "state": "TASK_STATE_FAILED",
                    "updated_at": Utc::now().to_rfc3339(),
                    "run_dir": inputs.workspace_dir.to_string_lossy(),
                    "result_written": false,
                    "harness_transport": inputs.harness_transport,
                    "harness_server_url": inputs.harness_server_url,
                    "host_adapter": inputs.assignment.host_adapter,
                    "execution_backend": inputs.execution_backend_name,
                    "acpx": if inputs.acpx_enabled { inputs.execution_backend_metadata.clone() } else { None },
                    "acp_native": if inputs.native_acp_enabled { inputs.execution_backend_metadata.clone() } else { None },
                    "capability_bundle": inputs.capability_bundle_card.clone(),
                    "timeout_secs": inputs.timeout_secs_for_status,
                    "error": err.clone(),
                })),
            );
            return Err(err);
        }
    };
    for (name, value) in &dispatch_credentials.env {
        match execution {
            DispatchExecution::Subprocess(cmd) | DispatchExecution::ManagedCustom(cmd, _) => {
                cmd.env(name, value);
            }
            DispatchExecution::NativeAcp(spec) => {
                spec.env.insert(name.clone(), value.clone());
            }
        }
    }
    if !dispatch_credentials.reports.is_empty() {
        append_trajectory_event(
            inputs.trajectory_path,
            json!({
                "event": "credentials_materialized",
                "dispatch_id": inputs.dispatch_id,
                "agent": inputs.assignment.selected_worker,
                "host_adapter": inputs.assignment.host_adapter,
                "credential_profiles": inputs.raw_credential_profiles,
                "reports": dispatch_credentials
                    .reports
                    .iter()
                    .map(credential_materialize_report_json)
                    .collect::<Vec<_>>(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
    }
    let credential_reports_json = dispatch_credentials
        .reports
        .iter()
        .map(credential_materialize_report_json)
        .collect::<Vec<_>>();

    Ok(CredentialApplyOutcome {
        env: dispatch_credentials.env,
        reports_json: credential_reports_json,
    })
}
