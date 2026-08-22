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
    pub(super) custom_launch_spec: Option<&'a tachi_params::LaunchSpec>,
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

fn build_custom_command_from_launch_spec(
    spec: &tachi_params::LaunchSpec,
) -> Result<tokio::process::Command, String> {
    if spec.backend != "custom" {
        return Err("server-minted LaunchSpec does not authorize the custom backend".to_string());
    }
    let (program, args) = spec
        .command
        .split_first()
        .ok_or_else(|| "server-minted custom LaunchSpec has no command".to_string())?;
    let mut command = tokio::process::Command::new(program);
    command.args(args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &spec.env_vars {
        command.env(key, value);
    }
    Ok(command)
}

pub(super) struct PreparedDispatchBackend {
    pub(super) execution: DispatchExecution,
    /// Control eligibility derives from the concrete prepared branch, not the
    /// assignment label. ACpx and native ACP can route a custom assignment but
    /// own their own lifecycle and must never receive a subprocess control slot.
    pub(super) managed_custom_eligible: bool,
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
            "custom" => {
                build_custom_command_from_launch_spec(ctx.custom_launch_spec.ok_or_else(|| {
                    "custom backend requires a server-minted LaunchSpec".to_string()
                })?)?
            }
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
        managed_custom_eligible: matches!(&execution, DispatchExecution::Subprocess(_))
            && ctx.custom_launch_spec.is_some()
            && !acpx_enabled
            && !native_acp_enabled,
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
        custom_launch_spec: Option<&tachi_params::LaunchSpec>,
        command: &[String],
    ) -> Result<tokio::process::Command, String> {
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
            custom_launch_spec,
            prompt_md_path: &temp.path().join("prompt.md"),
            mcp_config_path: None,
            v2: false,
            plan_generated_at: None,
            plan_duration_ms: None,
            harness_transport: "cli",
            harness_server_url: &None,
            capability_bundle_card: &Value::Null,
            timeout_secs_for_status: 5,
        })?;
        match prepared.execution {
            DispatchExecution::Subprocess(command) => Ok(command),
            DispatchExecution::NativeAcp(_) | DispatchExecution::ManagedCustom(_, _) => {
                Err("cli transport must prepare a subprocess".to_string())
            }
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
            None,
            &["poisoned-command".to_string()],
        )
        .expect("claude backend prepares without a custom spec");
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
        let custom_launch_spec = super::super::mint_custom_launch_spec(
            &custom_assignment,
            &custom_grant,
            &[
                "python3".to_string(),
                "-m".to_string(),
                "typed_worker".to_string(),
            ],
            "typed task",
            "cli",
            &None,
        )
        .expect("server mints the custom launch spec after admission");
        assert_eq!(
            custom_launch_spec.timeout_secs, custom_grant.timeout_secs,
            "the adapter spec must bind the canonical grant timeout before backend preparation"
        );
        super::super::validate_custom_launch_spec_timeout(
            &custom_launch_spec,
            custom_grant.timeout_secs,
        )
        .expect("production boundary accepts the canonical grant timeout");
        let mut timeout_mutant = custom_launch_spec.clone();
        timeout_mutant.timeout_secs += 1;
        let timeout_err = super::super::validate_custom_launch_spec_timeout(
            &timeout_mutant,
            custom_grant.timeout_secs,
        )
        .expect_err("one-sided spec timeout mutation must fail before backend preparation");
        assert!(
            timeout_err.contains("timeout diverged"),
            "timeout mismatch must have a stable fail-closed receipt: {timeout_err}"
        );
        let missing_spec = prepared_command(
            &custom_assignment,
            &grant("/legacy-bootstrap-poison"),
            None,
            &["poisoned-command".to_string()],
        )
        .expect_err("custom backend must not fall back to a flat carrier");
        assert!(
            missing_spec.contains("server-minted LaunchSpec"),
            "custom backend bypass must fail structurally: {missing_spec}"
        );
        let custom = prepared_command(
            &custom_assignment,
            &grant("/legacy-bootstrap-poison"),
            Some(&custom_launch_spec),
            &[
                "poisoned-command".to_string(),
                "--legacy-bootstrap-poison".to_string(),
            ],
        )
        .expect("custom backend consumes only the server-minted spec");
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
            vec!["-m", "typed_worker", "typed task"],
        );

        let mut no_cwd_grant = custom_grant.clone();
        no_cwd_grant.allowed_cwd = None;
        let no_cwd_spec = super::super::mint_custom_launch_spec(
            &custom_assignment,
            &no_cwd_grant,
            &["python3".to_string(), "-c".to_string(), "pass".to_string()],
            "typed task without cwd",
            "cli",
            &None,
        )
        .expect("server preserves an absent grant cwd in the custom spec");
        assert!(
            no_cwd_spec.cwd.is_none(),
            "an absent grant cwd must not be collapsed to the process cwd"
        );
        let no_cwd = prepared_command(
            &custom_assignment,
            &grant("/legacy-bootstrap-poison"),
            Some(&no_cwd_spec),
            &["poisoned-command".to_string()],
        )
        .expect("custom backend preserves the absent server-minted cwd");
        assert_eq!(
            no_cwd.as_std().get_current_dir(),
            None,
            "the adapter must not turn an absent cwd into '.'"
        );
    }

    #[test]
    fn custom_launch_spec_control_eligibility_follows_the_concrete_execution_branch() {
        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _acpx_command = crate::test_support::EnvRestore::set("TACHI_ACPX_COMMAND", "/bin/echo");
        let _acpx_agent =
            crate::test_support::EnvRestore::set("TACHI_ACPX_AGENT", "test-acp-agent");
        let _native_acp_command =
            crate::test_support::EnvRestore::set("TACHI_ACP_NATIVE_COMMAND", "/bin/echo");
        fn prepare(transport: &str) -> PreparedDispatchBackend {
            let temp = tempfile::tempdir().expect("backend branch tempdir");
            let server = crate::tests::make_server();
            let request = tachi_params::StaffAssignmentRequest::new(
                tachi_params::TachiDispatchReason::ExplicitUserRequest,
                "exercise the concrete custom execution branch",
            );
            let assignment = assignment("custom-worker", "custom", None);
            let grant = grant(temp.path().to_str().expect("UTF-8 tempdir"));
            let command = if transport == "acp-native" {
                vec!["/bin/echo".to_string()]
            } else {
                vec!["poisoned-ingress-command".to_string()]
            };
            let launch_spec = super::super::mint_custom_launch_spec(
                &assignment,
                &grant,
                &[
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "exit 0".to_string(),
                ],
                "custom branch discriminator",
                transport,
                &None,
            )
            .expect("server mints the custom launch spec");
            prepare_dispatch_backend(DispatchBackendContext {
                server: &server,
                trajectory_path: &temp.path().join("trajectory.jsonl"),
                workspace_dir: temp.path(),
                dispatch_id: "custom-branch-discriminator",
                request: &request,
                assignment: &assignment,
                grant: &grant,
                command: &command,
                prompt: "task",
                custom_launch_spec: Some(&launch_spec),
                prompt_md_path: &temp.path().join("prompt.md"),
                mcp_config_path: None,
                v2: false,
                plan_generated_at: None,
                plan_duration_ms: None,
                harness_transport: transport,
                harness_server_url: &None,
                capability_bundle_card: &Value::Null,
                timeout_secs_for_status: grant.timeout_secs,
            })
            .expect("concrete backend preparation")
        }

        let cli = prepare("cli");
        assert!(matches!(cli.execution, DispatchExecution::Subprocess(_)));
        assert!(cli.managed_custom_eligible);
        assert_eq!(cli.execution_backend_name, None);
        assert_eq!(cli.execution_backend_metadata, None);

        let acpx = prepare("acpx");
        assert!(matches!(acpx.execution, DispatchExecution::Subprocess(_)));
        assert!(!acpx.managed_custom_eligible);
        assert!(acpx.acpx_enabled);
        assert_eq!(acpx.execution_backend_name, Some("acpx"));
        assert!(acpx.execution_backend_metadata.is_some());

        let native_acp = prepare("acp-native");
        assert!(matches!(
            native_acp.execution,
            DispatchExecution::NativeAcp(_)
        ));
        assert!(!native_acp.managed_custom_eligible);
        assert!(native_acp.native_acp_enabled);
        assert_eq!(native_acp.execution_backend_name, Some("acp_native"));
        assert!(native_acp.execution_backend_metadata.is_some());
    }
}
