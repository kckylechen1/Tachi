use super::*;

pub(super) struct DispatchBackendContext<'a> {
    pub(super) server: &'a MemoryServer,
    pub(super) trajectory_path: &'a Path,
    pub(super) workspace_dir: &'a Path,
    pub(super) dispatch_id: &'a str,
    pub(super) request: &'a tachi_params::StaffAssignmentRequest,
    pub(super) assignment: &'a tachi_params::ResolvedStaffAssignment,
    pub(super) grant: &'a tachi_params::ExecutionGrant,
    pub(super) command: &'a [String],
    pub(super) prompt: &'a str,
    pub(super) prompt_md_path: &'a Path,
    pub(super) mcp_config_path: Option<&'a PathBuf>,
    pub(super) v2: bool,
    pub(super) plan_generated_at: Option<&'a str>,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) harness_transport: &'a str,
    pub(super) harness_server_url: &'a Option<String>,
    pub(super) capability_bundle_card: &'a Value,
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
            assignment: ctx.assignment,
            request: ctx.request,
            backend,
            error: err,
            v2: ctx.v2,
            plan_generated_at: ctx.plan_generated_at,
            plan_duration_ms: ctx.plan_duration_ms,
            harness_transport: ctx.harness_transport,
            harness_server_url: ctx.harness_server_url,
            capability_bundle_card: ctx.capability_bundle_card,
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
            Some(&ctx.assignment.selected_backend),
            ctx.request.project.as_deref(),
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
        let acpx_spec = match build_acpx_command_spec(
            ctx.request,
            ctx.assignment,
            ctx.grant,
            &acpx_prompt_path,
        ) {
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
                "agent": ctx.assignment.selected_backend,
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
            ctx.request,
            ctx.assignment,
            ctx.grant,
            ctx.command,
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
                "agent": ctx.assignment.selected_backend,
                "execution_backend": "acp_native",
                "acp_native": native_spec.metadata.clone(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
        execution_backend_metadata = Some(native_spec.metadata.clone());
        DispatchExecution::NativeAcp(native_spec)
    } else {
        let cmd = match ctx.assignment.selected_backend.as_str() {
            "claude" => build_claude_command(
                ctx.assignment,
                ctx.grant,
                ctx.command,
                ctx.prompt,
                ctx.mcp_config_path,
            )?,
            "codex" => build_codex_command(
                ctx.assignment,
                ctx.grant,
                ctx.command,
                ctx.prompt,
                ctx.mcp_config_path,
            )?,
            "grok" => build_grok_command(
                ctx.assignment,
                ctx.grant,
                ctx.command,
                ctx.prompt,
                ctx.mcp_config_path,
            )?,
            "kimi" => build_kimi_command(ctx.assignment, ctx.grant, ctx.command, ctx.prompt)?,
            "custom" => build_custom_command(ctx.assignment, ctx.grant, ctx.command, ctx.prompt)?,
            "opencode" => {
                build_opencode_command(ctx.assignment, ctx.grant, ctx.command, ctx.prompt)?
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assignment(
        worker: &str,
        backend: &str,
        model: Option<&str>,
    ) -> tachi_params::ResolvedStaffAssignment {
        let mut assignment = tachi_params::ResolvedStaffAssignment::new(
            "assignment-only-selector",
            tachi_params::TachiDispatchReason::ExplicitUserRequest,
            worker,
            backend,
        );
        if let Some(model) = model {
            assignment = assignment.with_model(model);
        }
        assignment
    }

    fn grant(cwd: &str) -> tachi_params::ExecutionGrant {
        tachi_params::ExecutionGrant {
            grant_id: "backend-selector-grant".to_string(),
            env_id: None,
            unmanaged_cwd_allowed: false,
            allowed_cwd: Some(cwd.into()),
            credential_profiles: Vec::new(),
            mcp_access: None,
            allowed_tools: Vec::new(),
            permission_profile: Some("default".to_string()),
            sandbox: None,
            max_turns: None,
            timeout_secs: 5,
        }
    }

    fn prepared_command(
        assignment: &tachi_params::ResolvedStaffAssignment,
        grant: &tachi_params::ExecutionGrant,
        command: &[String],
    ) -> tokio::process::Command {
        let temp = tempfile::tempdir().expect("backend selector tempdir");
        let request = tachi_params::StaffAssignmentRequest::new(
            tachi_params::TachiDispatchReason::ExplicitUserRequest,
            "select from assignment",
        );
        let prepared = prepare_dispatch_backend(DispatchBackendContext {
            server: &crate::tests::make_server(),
            trajectory_path: &temp.path().join("trajectory.jsonl"),
            workspace_dir: temp.path(),
            dispatch_id: "assignment-only-selector",
            request: &request,
            assignment,
            grant,
            command,
            prompt: "task",
            prompt_md_path: &temp.path().join("prompt.md"),
            mcp_config_path: None,
            v2: false,
            plan_generated_at: None,
            plan_duration_ms: None,
            harness_transport: "cli",
            harness_server_url: &None,
            capability_bundle_card: &Value::Null,
            timeout_secs_for_status: 5,
        })
        .expect("assignment-only backend selection prepares");
        match prepared.execution {
            DispatchExecution::Subprocess(command) => command,
            DispatchExecution::NativeAcp(_) => panic!("cli transport must prepare a subprocess"),
        }
    }

    #[test]
    fn assignment_only_backend_selector_controls_claude_and_custom_launch_values() {
        // Deliberately disagree: production selection must follow
        // `selected_backend`, never a stale normalized ingress agent or worker.
        let claude_assignment = assignment("custom", "claude", Some("typed-claude"));
        let claude_grant = grant("/typed/claude-cwd");
        let claude = prepared_command(
            &claude_assignment,
            &claude_grant,
            &["poisoned-command".to_string()],
        );
        assert_eq!(claude.as_std().get_program(), "claude");
        assert_eq!(
            claude.as_std().get_current_dir(),
            Some(Path::new("/typed/claude-cwd"))
        );
        assert!(claude
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair == ["--model", "typed-claude"]));

        let custom_assignment = assignment("claude", "custom", None);
        let custom_grant = grant("/typed/custom-cwd");
        let custom = prepared_command(
            &custom_assignment,
            &custom_grant,
            &[
                "python3".to_string(),
                "-m".to_string(),
                "typed_worker".to_string(),
            ],
        );
        assert_eq!(custom.as_std().get_program(), "python3");
        assert_eq!(
            custom.as_std().get_current_dir(),
            Some(Path::new("/typed/custom-cwd"))
        );
        assert_eq!(
            custom
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            vec!["-m", "typed_worker", "task"],
        );
    }
}
