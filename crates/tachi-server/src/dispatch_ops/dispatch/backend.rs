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
    pub(super) managed_launch_spec: Option<&'a tachi_params::LaunchSpec>,
    pub(super) managed_backend_metadata: Option<&'a serde_json::Value>,
    pub(super) prompt_md_path: &'a Path,
    pub(super) mcp_config_path: Option<&'a PathBuf>,
    pub(super) v2: bool,
    pub(super) plan_generated_at: Option<&'a str>,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) harness_transport: &'a str,
    pub(super) harness_server_url: &'a Option<String>,
    pub(super) timeout_secs_for_status: u64,
}

fn build_command_from_launch_spec(
    spec: &tachi_params::LaunchSpec,
    expected_backend: &str,
) -> Result<tokio::process::Command, String> {
    if !matches!(expected_backend, "custom" | "codex") || spec.backend != expected_backend {
        return Err(
            "server-minted LaunchSpec does not authorize the selected managed subprocess backend"
                .to_string(),
        );
    }
    let (program, args) = spec
        .command
        .split_first()
        .ok_or_else(|| "server-minted managed LaunchSpec has no command".to_string())?;
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
    pub(super) managed_backend_eligible: bool,
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
    let mut execution_backend_metadata = ctx.managed_backend_metadata.cloned();

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
            "codex" if ctx.managed_launch_spec.is_some() => build_command_from_launch_spec(
                ctx.managed_launch_spec.ok_or_else(|| {
                    "codex managed backend requires a server-minted LaunchSpec".to_string()
                })?,
                "codex",
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
            "custom" => build_command_from_launch_spec(
                ctx.managed_launch_spec.ok_or_else(|| {
                    "custom backend requires a server-minted LaunchSpec".to_string()
                })?,
                "custom",
            )?,
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
        managed_backend_eligible: matches!(&execution, DispatchExecution::Subprocess(_))
            && ctx.managed_launch_spec.is_some()
            && !is_opencode_serve_transport(ctx.harness_transport)
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
        managed_launch_spec: Option<&tachi_params::LaunchSpec>,
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
            managed_launch_spec,
            managed_backend_metadata: None,
            prompt_md_path: &temp.path().join("prompt.md"),
            mcp_config_path: None,
            v2: false,
            plan_generated_at: None,
            plan_duration_ms: None,
            harness_transport: "cli",
            harness_server_url: &None,
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
        let custom_launch_spec = super::super::mint_managed_launch_spec(
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
        super::super::validate_managed_launch_spec_timeout(
            &custom_launch_spec,
            custom_grant.timeout_secs,
        )
        .expect("production boundary accepts the canonical grant timeout");
        let mut timeout_mutant = custom_launch_spec.clone();
        timeout_mutant.timeout_secs += 1;
        let timeout_err = super::super::validate_managed_launch_spec_timeout(
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
        let mut cross_backend_spec = custom_launch_spec.clone();
        cross_backend_spec.backend = "codex".to_string();
        let cross_backend = prepared_command(
            &custom_assignment,
            &custom_grant,
            Some(&cross_backend_spec),
            &["poisoned-command".to_string()],
        )
        .expect_err("one managed adapter must not consume another adapter's LaunchSpec");
        assert!(
            cross_backend.contains("does not authorize the selected"),
            "LaunchSpec backend identity is an authority fence: {cross_backend}"
        );
        let codex_assignment = assignment("codex", "codex", None);
        let reverse_cross_backend = prepared_command(
            &codex_assignment,
            &custom_grant,
            Some(&custom_launch_spec),
            &["poisoned-command".to_string()],
        )
        .expect_err("Codex must not consume a custom adapter LaunchSpec");
        assert!(
            reverse_cross_backend.contains("does not authorize the selected"),
            "the backend identity fence must reject both mismatch directions: {reverse_cross_backend}"
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
        let no_cwd_spec = super::super::mint_managed_launch_spec(
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
            let launch_spec = super::super::mint_managed_launch_spec(
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
                managed_launch_spec: Some(&launch_spec),
                managed_backend_metadata: None,
                prompt_md_path: &temp.path().join("prompt.md"),
                mcp_config_path: None,
                v2: false,
                plan_generated_at: None,
                plan_duration_ms: None,
                harness_transport: transport,
                harness_server_url: &None,
                timeout_secs_for_status: grant.timeout_secs,
            })
            .expect("concrete backend preparation")
        }

        let cli = prepare("cli");
        assert!(matches!(cli.execution, DispatchExecution::Subprocess(_)));
        assert!(cli.managed_backend_eligible);
        assert_eq!(cli.execution_backend_name, None);
        assert_eq!(cli.execution_backend_metadata, None);

        let serve = prepare("serve");
        assert!(matches!(serve.execution, DispatchExecution::Subprocess(_)));
        assert!(
            !serve.managed_backend_eligible,
            "typed OpenCode serve transport owns an attached client lifecycle"
        );
        assert_eq!(serve.execution_backend_name, None);
        assert_eq!(serve.execution_backend_metadata, None);

        let acpx = prepare("acpx");
        assert!(matches!(acpx.execution, DispatchExecution::Subprocess(_)));
        assert!(!acpx.managed_backend_eligible);
        assert!(acpx.acpx_enabled);
        assert_eq!(acpx.execution_backend_name, Some("acpx"));
        assert!(acpx.execution_backend_metadata.is_some());

        let native_acp = prepare("acp-native");
        assert!(matches!(
            native_acp.execution,
            DispatchExecution::NativeAcp(_)
        ));
        assert!(!native_acp.managed_backend_eligible);
        assert!(native_acp.native_acp_enabled);
        assert_eq!(native_acp.execution_backend_name, Some("acp_native"));
        assert!(native_acp.execution_backend_metadata.is_some());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // serializes the process-global PATH fixture
    async fn managed_codex_prerequisites_refuse_executable_and_account_without_disclosure() {
        use std::os::unix::fs::PermissionsExt;

        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let empty_bin = tempfile::tempdir().expect("empty PATH fixture");
        let _path = crate::test_support::EnvRestore::set_path("PATH", empty_bin.path());
        let codex = assignment("codex", "codex", None);
        let missing = super::super::managed_backend_metadata(
            super::super::ManagedControlOrigin::StaffFacade,
            &codex,
        )
        .await
        .expect_err("a missing Codex executable must refuse before worker spawn");
        assert_eq!(
            missing,
            "managed_backend_executable_unavailable: codex executable is unavailable"
        );

        let codex_path = empty_bin.path().join("codex");
        std::fs::write(
            &codex_path,
            "#!/bin/sh\nif [ \"$1\" = login ]; then printf 'fixture-account-secret' >&2; exit 1; fi\nprintf 'codex-cli 0.144.1\\n'\n",
        )
        .expect("write unauthenticated Codex fixture");
        let mut permissions = std::fs::metadata(&codex_path)
            .expect("Codex fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&codex_path, permissions).expect("make Codex fixture executable");

        let unavailable = super::super::managed_backend_metadata(
            super::super::ManagedControlOrigin::StaffFacade,
            &codex,
        )
        .await
        .expect_err("an unavailable Codex account must refuse before worker spawn");
        assert_eq!(
            unavailable,
            "managed_backend_account_unavailable: codex account is unavailable"
        );
        assert!(
            !unavailable.contains("fixture-account-secret"),
            "account probe output must never enter the refusal receipt"
        );

        std::fs::write(
            &codex_path,
            "#!/bin/sh\nif [ \"$1\" = login ]; then if IFS= read -r daemon_input; then exit 91; fi; exit 0; fi\nprintf 'codex-cli 0.144.1\\n'\n",
        )
        .expect("write stdin-isolation account fixture");
        assert_eq!(
            tachi_dispatch::probe_codex_account(std::time::Duration::from_millis(500)),
            tachi_dispatch::BackendAccountProbe::Available,
            "account probe must observe EOF instead of daemon stdin"
        );

        std::fs::write(
            &codex_path,
            "#!/bin/sh\nif [ \"$1\" = login ]; then printf '%s\\n' \"$$\" > \"$0.pid\"; /bin/sh -c 'printf \"%s\\n\" \"$$\" > \"$1\"; while :; do :; done' sh \"$0.descendant.pid\" & while [ ! -s \"$0.descendant.pid\" ]; do :; done; while :; do :; done; fi\nprintf 'codex-cli 0.144.1\\n'\n",
        )
        .expect("write hanging account fixture");
        let timed_out =
            match tachi_dispatch::probe_codex_account(std::time::Duration::from_millis(500)) {
                tachi_dispatch::BackendAccountProbe::TimedOut => {
                    "managed_backend_account_unavailable: codex account probe timed out".to_string()
                }
                outcome => panic!("a hanging account probe must fail closed: {outcome:?}"),
            };
        assert_eq!(
            timed_out,
            "managed_backend_account_unavailable: codex account probe timed out"
        );
        let account_pid: libc::pid_t = std::fs::read_to_string(empty_bin.path().join("codex.pid"))
            .expect("hanging account fixture published its PID")
            .trim()
            .parse()
            .expect("numeric account probe PID");
        let descendant_pid: libc::pid_t =
            std::fs::read_to_string(empty_bin.path().join("codex.descendant.pid"))
                .expect("hanging account fixture published its descendant PID")
                .trim()
                .parse()
                .expect("numeric account descendant PID");
        for pid in [account_pid, descendant_pid] {
            // SAFETY: signal 0 is a non-mutating liveness probe for a fixture
            // process that the canonical group cleanup must have reaped.
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        // SAFETY: the fixture root was launched as its owned process-group
        // leader; ESRCH is the authoritative group-absence proof.
        assert_eq!(unsafe { libc::kill(-account_pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );

        std::fs::write(
            &codex_path,
            "#!/bin/sh\nif [ \"$1\" = login ]; then printf 'fixture-account-secret' >&2; exit 0; fi\nprintf 'fixture-version-stdout-secret\\n'\nprintf 'codex-cli 0.144.1+fixture-version-stderr-secret\\n' >&2\n",
        )
        .expect("write hostile successful stderr-only version fixture");
        let metadata = super::super::managed_backend_metadata(
            super::super::ManagedControlOrigin::StaffFacade,
            &codex,
        )
        .await
        .expect("successful prerequisites")
        .expect("managed Codex metadata");
        assert_eq!(metadata["adapter_version"], "0.144.1");
        let serialized = serde_json::to_string(&metadata).expect("metadata JSON");
        for secret_marker in [
            "fixture-account-secret",
            "fixture-version-stdout-secret",
            "fixture-version-stderr-secret",
        ] {
            assert!(
                !serialized.contains(secret_marker),
                "raw probe output must not enter managed metadata: {serialized}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // serializes the process-global PATH fixture
    async fn managed_codex_refuses_missing_containment_before_account_spawn() {
        use std::os::unix::fs::PermissionsExt;

        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let bin = tempfile::tempdir().expect("owned Codex PATH fixture");
        let marker = bin.path().join("codex-started");
        let _path = crate::test_support::EnvRestore::set_path("PATH", bin.path());
        let _marker = crate::test_support::EnvRestore::set_path("TACHI_FAKE_CODEX_MARKER", &marker);
        let codex = bin.path().join("codex");
        std::fs::write(
            &codex,
            "#!/bin/sh\n/usr/bin/touch \"$TACHI_FAKE_CODEX_MARKER\"\n",
        )
        .expect("write account fixture");
        let mut permissions = std::fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&codex, permissions).unwrap();

        let assignment = assignment("codex", "codex", None);
        let error = super::super::managed_backend_metadata(
            super::super::ManagedControlOrigin::StaffFacade,
            &assignment,
        )
        .await
        .expect_err("uncontained prerequisite must refuse before spawn");
        assert_eq!(
            error,
            "managed_backend_containment_unavailable: codex prerequisite process containment is unavailable"
        );
        assert!(!marker.exists(), "Codex prerequisite spawned");
    }
}
