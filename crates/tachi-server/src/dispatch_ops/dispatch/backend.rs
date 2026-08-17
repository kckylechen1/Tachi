use super::*;

pub(super) struct DispatchBackendContext<'a> {
    pub(super) server: &'a MemoryServer,
    pub(super) trajectory_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) dispatch_id: &'a str,
    pub(super) agent_norm: &'a str,
    pub(super) params: &'a TachiDispatchParams,
    pub(super) prompt: &'a str,
    pub(super) prompt_md_path: &'a Path,
    pub(super) mcp_config_path: Option<&'a PathBuf>,
    pub(super) v2: bool,
    pub(super) plan_generated_at: Option<&'a str>,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) harness_transport: &'a str,
    pub(super) harness_server_url: &'a Option<String>,
    pub(super) timeout_secs_for_status: u64,
}

pub(super) struct PreparedDispatchBackend {
    pub(super) execution: DispatchExecution,
    pub(super) execution_backend_name: Option<&'static str>,
    pub(super) execution_backend_metadata: Option<serde_json::Value>,
    pub(super) acpx_enabled: bool,
    pub(super) native_acp_enabled: bool,
}

pub(super) fn prepare_dispatch_backend(
    ctx: DispatchBackendContext<'_>,
) -> Result<PreparedDispatchBackend, String> {
    let acpx_enabled = is_acpx_transport(ctx.harness_transport);
    let native_acp_enabled = is_native_acp_transport(ctx.harness_transport);
    let execution_backend_name = if acpx_enabled {
        Some("acpx")
    } else if native_acp_enabled {
        Some("acp_native")
    } else {
        None
    };
    let mut execution_backend_metadata: Option<serde_json::Value> = None;

    let record_backend_prepare_failure = |backend: &str, err: &str| {
        record_execution_backend_prepare_failure(ExecutionBackendPrepareFailure {
            trajectory_path: ctx.trajectory_path,
            workspace_dir: ctx.workspace_dir,
            dispatch_id: ctx.dispatch_id,
            agent_norm: ctx.agent_norm,
            params: ctx.params,
            backend,
            error: err,
            v2: ctx.v2,
            plan_generated_at: ctx.plan_generated_at,
            plan_duration_ms: ctx.plan_duration_ms,
            harness_transport: ctx.harness_transport,
            harness_server_url: ctx.harness_server_url,
            timeout_secs_for_status: ctx.timeout_secs_for_status,
        });
        // #773 Layer-2 ② (hole b): backend-prep failure is a terminal dispatch
        // state the agent never `tachi_complete`s — record a canonical outcome
        // row so the router sees it. First-writer-wins on dispatch_id keeps this
        // 'backend' class from being clobbered by the generic early-exit closer.
        crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
            ctx.server,
            ctx.dispatch_id,
            "backend",
            Some(ctx.agent_norm),
            ctx.params.project.as_deref(),
        );
    };

    let execution = if acpx_enabled {
        let acpx_prompt_path = match prepare_acpx_prompt(ctx.prompt_md_path, ctx.prompt) {
            Ok(path) => path,
            Err(err) => {
                record_backend_prepare_failure("acpx", &err);
                return Err(err);
            }
        };
        let acpx_spec = match build_acpx_command_spec(ctx.params, ctx.agent_norm, &acpx_prompt_path)
        {
            Ok(spec) => spec,
            Err(err) => {
                record_backend_prepare_failure("acpx", &err);
                return Err(err);
            }
        };
        append_trajectory_event(
            ctx.trajectory_path,
            json!({
                "event": "execution_backend_prepared",
                "dispatch_id": ctx.dispatch_id,
                "agent": ctx.agent_norm,
                "execution_backend": "acpx",
                "acpx": acpx_spec.metadata.clone(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
        execution_backend_metadata = Some(acpx_spec.metadata.clone());
        DispatchExecution::Subprocess(build_acpx_command(&acpx_spec))
    } else if native_acp_enabled {
        let native_spec = match build_native_acp_run_spec(
            &ctx.server.tachi_home_dir(),
            ctx.params,
            ctx.agent_norm,
            ctx.prompt,
        ) {
            Ok(spec) => spec,
            Err(err) => {
                record_backend_prepare_failure("acp_native", &err);
                return Err(err);
            }
        };
        append_trajectory_event(
            ctx.trajectory_path,
            json!({
                "event": "execution_backend_prepared",
                "dispatch_id": ctx.dispatch_id,
                "agent": ctx.agent_norm,
                "execution_backend": "acp_native",
                "acp_native": native_spec.metadata.clone(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
        execution_backend_metadata = Some(native_spec.metadata.clone());
        DispatchExecution::NativeAcp(native_spec)
    } else {
        let cmd = match ctx.agent_norm {
            "claude" => build_claude_command(ctx.params, ctx.prompt, ctx.mcp_config_path)?,
            "codex" => build_codex_command(ctx.params, ctx.prompt, ctx.mcp_config_path)?,
            "grok" => build_grok_command(ctx.params, ctx.prompt, ctx.mcp_config_path)?,
            "kimi" => build_kimi_command(ctx.params, ctx.prompt)?,
            "custom" => build_custom_command(ctx.params, ctx.prompt)?,
            "opencode" => build_opencode_command(ctx.params, ctx.prompt)?,
            other => {
                return Err(format!(
                    "Internal error: unhandled dispatch agent '{}'. {}",
                    other,
                    dispatch_agent_help_list()
                ));
            }
        };
        DispatchExecution::Subprocess(cmd)
    };

    Ok(PreparedDispatchBackend {
        execution,
        execution_backend_name,
        execution_backend_metadata,
        acpx_enabled,
        native_acp_enabled,
    })
}
