use super::*;

pub(in crate::dispatch_ops::dispatch) fn spawn_background_dispatch(ctx: BackgroundDispatchContext) {
    let server_clone = ctx.server;
    let d_id = ctx.dispatch_id;
    let agent_for_watchdog = ctx.agent;
    let env_id_for_watchdog = ctx.env_id;
    let project_for_watchdog = ctx.project;
    let stage_for_traj = ctx.stage;
    let traj_path_for_spawn = ctx.trajectory_path;
    let workspace_dir = ctx.workspace_dir.clone();
    let workspace_dir_for_spawn = ctx.workspace_dir;
    let v2_for_spawn = ctx.v2;
    let plan_generated_at_for_spawn = ctx.plan_generated_at;
    let plan_duration_ms_for_spawn = ctx.plan_duration_ms;
    let (timeout_secs_for_spawn, timeout) = {
        #[cfg(test)]
        {
            managed_timeout_override()
                .map(|override_timeout| (override_timeout.as_secs().max(1), override_timeout))
                .unwrap_or((ctx.timeout_secs, ctx.timeout))
        }
        #[cfg(not(test))]
        {
            (ctx.timeout_secs, ctx.timeout)
        }
    };
    let feedback_rules_trace_for_spawn = ctx.feedback_rules_trace;
    let harness_transport_for_spawn = ctx.harness_transport;
    let harness_server_url_for_spawn = ctx.harness_server_url;
    let host_adapter_for_spawn = ctx.host_adapter;
    let opencode_sop_label_for_spawn = ctx.opencode_sop_label;
    let execution_backend_metadata_for_spawn = ctx.execution_backend_metadata;
    let execution_for_spawn = ctx.execution;
    let cwd_authority_for_spawn = ctx.cwd_authority;
    let flow_dispatch_slot_for_spawn = ctx.flow_dispatch_slot;
    let mcp_config_path = ctx.mcp_config_path;
    let managed_run_guard = ctx.managed_run_guard;
    let managed_ephemeral_credential_cleanup = ctx.managed_ephemeral_credential_cleanup;
    let native_staff_sandbox = ctx.native_staff_sandbox;
    let postflight_gate_for_spawn = ctx.postflight_gate;
    let postflight_dispatch_lease = ctx.postflight_dispatch_lease;
    let auto_staff_exec_env = ctx.auto_staff_exec_env;

    let presence = crate::staffing_ops::presence::register_worker(
        &server_clone,
        &d_id,
        &agent_for_watchdog,
        project_for_watchdog.as_deref(),
        ctx.presence_assignment,
    );
    let observer = presence.as_ref().map(|(observer, _)| observer.clone());
    let background = async move {
        let _presence_guard = presence.map(|(_, guard)| guard);
        // Keep the registry entry and its sender alive for the entire
        // background lifecycle. Binding this outside the async move drops the
        // guard as soon as scheduling returns and makes the child observe a
        // closed cancellation receiver before it can spawn.
        let _managed_run_guard = managed_run_guard;
        let _mcp_cleanup = McpCleanup(mcp_config_path);
        let mut early_exit_cleanup = BackgroundEarlyExitCleanup::new(
            server_clone.clone(),
            workspace_dir_for_spawn.clone(),
            flow_dispatch_slot_for_spawn,
            postflight_dispatch_lease,
        );

        // execute_started — Stage 2 (or, in V1, the only stage).
        let execute_started_at = Utc::now();
        let execute_started_instant = std::time::Instant::now();
        append_trajectory_event(
            &traj_path_for_spawn,
            json!({
                "event": "execute_started",
                "dispatch_id": d_id,
                "agent": agent_for_watchdog,
                "stage": stage_for_traj,
                "v2": v2_for_spawn,
                "harness_transport": harness_transport_for_spawn.clone(),
                "harness_server_url": harness_server_url_for_spawn.clone(),
                "host_adapter": host_adapter_for_spawn.clone(),
                "timestamp": execute_started_at.to_rfc3339(),
            }),
        );

        let require_postflight_containment = postflight_gate_for_spawn.is_some();
        let output_projection =
            crate::dispatch_ops::subprocess::SubprocessOutputProjection::for_backend(
                &agent_for_watchdog,
                &harness_transport_for_spawn,
            );
        let (runner_outcome, mut managed_cancellation, managed_termination_proof) =
            match execution_for_spawn {
                DispatchExecution::Subprocess(cmd) if agent_for_watchdog == "opencode" => (
                    run_opencode_sop_subprocess_with_liveness(
                        cmd,
                        timeout,
                        opencode_sop_label_for_spawn
                            .as_deref()
                            .unwrap_or("opencode_sop"),
                        require_postflight_containment,
                        cwd_authority_for_spawn.clone(),
                        output_projection,
                    )
                    .await,
                    None,
                    None,
                ),
                DispatchExecution::Subprocess(cmd) => (
                    run_agent_subprocess_with_liveness(
                        cmd,
                        timeout,
                        require_postflight_containment,
                        cwd_authority_for_spawn.clone(),
                        output_projection,
                    )
                    .await,
                    None,
                    None,
                ),
                DispatchExecution::ManagedCustom(cmd, receiver) => {
                    let managed_run_dir = workspace_dir_for_spawn.clone();
                    let managed_cwd_authority = cwd_authority_for_spawn.clone();
                    let observer = crate::staffing_ops::presence::current_worker();
                    let outcome = match tokio::spawn(crate::staffing_ops::presence::observe_runner(
                        observer,
                        async move {
                            run_managed_custom_subprocess_outcome(
                                cmd,
                                timeout,
                                receiver,
                                &managed_run_dir,
                                require_postflight_containment,
                                managed_cwd_authority,
                                output_projection,
                            )
                            .await
                        },
                    ))
                    .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            crate::dispatch_ops::subprocess::ManagedSubprocessOutcome::indeterminate(
                                Err(format!("managed subprocess panicked: {error}")),
                                "managed runner task panicked after spawn state became unknown"
                                    .to_string(),
                            )
                        }
                    };
                    (
                        crate::dispatch_ops::dispatch::DispatchRunOutcome {
                            result: outcome.result,
                            liveness: outcome.liveness,
                            deferred_native_acp: None,
                            credential_cleanup_error: None,
                        },
                        outcome.cancellation,
                        outcome.termination_proof,
                    )
                }
                DispatchExecution::ManagedNative(spec, control) => {
                    crate::dispatch_ops::acp_native::run_managed_native_acp(
                        *spec,
                        &workspace_dir_for_spawn,
                        &traj_path_for_spawn,
                        &d_id,
                        &agent_for_watchdog,
                        timeout,
                        require_postflight_containment,
                        *control,
                    )
                    .await
                }
                DispatchExecution::NativeAcp(spec) => (
                    crate::dispatch_ops::acp_native::run_native_acp_dispatch_with_liveness(
                        *spec,
                        &workspace_dir_for_spawn,
                        &traj_path_for_spawn,
                        &d_id,
                        &agent_for_watchdog,
                        timeout,
                        require_postflight_containment,
                    )
                    .await,
                    None,
                    None,
                ),
            };
        crate::staffing_ops::presence::worker_event("disconnected", "runner_returned");
        let native_credential_cleanup_error = runner_outcome.credential_cleanup_error;
        let runner_liveness = runner_outcome.liveness;
        let mut pending_native_acp_artifacts = runner_outcome.deferred_native_acp;
        let mut result = runner_outcome.result;
        let server_confined_native = execution_backend_metadata_for_spawn
            .as_ref()
            .is_some_and(|metadata| metadata.get("profile_contract").is_some());
        let native_descendants_reaped = server_confined_native
            && matches!(
                &runner_liveness,
                crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned
                    | crate::exec_env_postflight::RunnerLivenessEvidence::ConfirmedReaped { .. }
            );
        let mut native_staff_lifecycle_failed = false;
        let mut native_staff_lifecycle = if native_staff_sandbox {
            let retained = match (
                auto_staff_exec_env.as_ref(),
                early_exit_cleanup.postflight_dispatch_lease_mut(),
            ) {
                (Some(_), Some(lease)) if native_descendants_reaped => lease.begin_publication(),
                (Some(owned), Some(lease)) => {
                    owned.server.with_global_store(|store| {
                        crate::exec_env_ops::quarantine_lease_resources(
                            store.connection_mut(),
                            &owned.env_id,
                            "native harness completed; retain workspace pending descendant cleanup verification",
                        ).map(|_| ())
                    }).and_then(|()| lease.release_after_fence())
                }
                _ => Err("native Staff lost its owned workspace or lifecycle lease".to_string()),
            };
            let error = retained.err();
            if let Some(error) = &error {
                native_staff_lifecycle_failed = true;
                result = Err(format!("native Staff workspace retention failed: {error}"));
            }
            Some(json!({
                "gate": "exec_env_postflight",
                "status": if server_confined_native { "server_filesystem_boundary" } else { "delegated_to_harness" },
                "workspace_cleanup": if native_descendants_reaped { "eligible_after_terminal_publication" } else { "retained" },
                "resource_fence": if error.is_none() { "persisted" } else { "failed" },
                "error": error,
                "descendant_liveness": crate::exec_env_postflight::DescendantLiveness::describe(&runner_liveness),
            }))
        } else {
            None
        };
        if let Some(receipt) = &native_staff_lifecycle {
            append_trajectory_event(
                &traj_path_for_spawn,
                json!({
                    "event": "native_staff_lifecycle",
                    "dispatch_id": d_id,
                    "receipt": receipt,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
        }

        let mut pending_dsh_output = if agent_for_watchdog == "dsh" {
            result.as_ref().ok().map(|outcome| outcome.output.clone())
        } else {
            None
        };
        if agent_for_watchdog == "dsh" {
            let child_exit_code = result.as_ref().ok().and_then(|outcome| outcome.exit_code);
            let mapped_answer = match &result {
                Ok(outcome) if outcome.exit_code == Some(0) => {
                    Some(crate::dispatch_ops::dsh::completed_answer(&outcome.output))
                }
                _ => None,
            };
            let protocol_error = mapped_answer
                .as_ref()
                .and_then(|answer| answer.as_ref().err());
            append_trajectory_event(
                &traj_path_for_spawn,
                json!({
                    "event": "dsh_headless_result",
                    "dispatch_id": d_id,
                    "child_exit_code": child_exit_code,
                    "completed": mapped_answer.as_ref().is_some_and(|answer| answer.is_ok()),
                    "protocol_error": protocol_error,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            match mapped_answer {
                Some(Ok(answer)) => {
                    if let Ok(outcome) = result.as_mut() {
                        outcome.output = answer;
                    }
                }
                Some(Err(error)) => result = Err(format!("dsh headless protocol failed: {error}")),
                None => {}
            }
        }
        let execute_duration_ms = execute_started_instant.elapsed().as_millis() as u64;

        // CLI subprocesses intentionally never acknowledge a receipt: they
        // have no protocol field that reports an executed model. Native ACP is
        // the sole carrier with that evidence surface.
        if is_native_acp_transport(&harness_transport_for_spawn) {
            let observed_model = result
                .as_ref()
                .ok()
                .and_then(|outcome| outcome.observed_model.as_deref());
            if let Err(error) =
                persist_acp_model_acknowledgement(&workspace_dir_for_spawn, observed_model)
            {
                append_trajectory_event(
                    &traj_path_for_spawn,
                    json!({
                        "event": "acp_identity_acknowledgement_persist_failed",
                        "dispatch_id": d_id,
                        "agent": agent_for_watchdog,
                        "error": error,
                        "timestamp": Utc::now().to_rfc3339(),
                    }),
                );
            }
        }

        // Append subprocess_finished event to trajectory.jsonl
        let full_output = match &result {
            Ok(r) => r.output.clone(),
            Err(e) => e.clone(),
        };
        {
            let exit_code = match &result {
                Err(_) => None,
                Ok(r) => r.exit_code,
            };
            let output_tail = if postflight_gate_for_spawn.is_some() {
                "[withheld pending exec_env_postflight]".to_string()
            } else {
                match &result {
                    Err(e) => e.chars().take(200).collect::<String>(),
                    Ok(r) => tail_chars(&r.output, 500),
                }
            };
            let finished_event = json!({
                "event": "subprocess_finished",
                "dispatch_id": d_id,
                "agent": agent_for_watchdog,
                "stage": stage_for_traj,
                "exit_code": exit_code,
                "timestamp": Utc::now().to_rfc3339(),
                "output_tail": output_tail,
            });
            if let Ok(line) = serde_json::to_string(&finished_event) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&traj_path_for_spawn)
                {
                    let _ = writeln!(f, "{}", line);
                }
                let progress_path = workspace_dir_for_spawn.join("progress.jsonl");
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(progress_path)
                {
                    let _ = writeln!(f, "{}", line);
                }
            }
        }

        // --- POSTFLIGHT GATE: verify write-contract and liveness (#894 S2e, #1322) ---
        let mut postflight_outcome = if let Some(gate) = &postflight_gate_for_spawn {
            // The runner owns termination and reap. Postflight receives typed
            // terminal evidence only; no numeric PID crosses this handoff and
            // this layer has no signalling capability.
            //
            // Audit E4: the re-walk + BLAKE2s re-hash takes seconds on a large
            // lease, so the worker hands its executor core to another thread
            // (executor_offload::block_off_core) while it blocks. Adding no
            // `.await` point is load-bearing: this detached task is the sole
            // owner of the armed early-exit cleanup for the credential
            // materializations, the flow dispatch slot and the exclusive
            // postflight lease. A `spawn_blocking(..).await` here would let an
            // abort drop that cleanup while the gate is still running —
            // releasing creds/slot/lease before resolution and losing the
            // resource fence and terminal trajectory event — so
            // run -> apply_verdict -> trajectory must stay one
            // non-cancellable synchronous section.
            let outcome = crate::executor_offload::block_off_core(|| gate.run(&runner_liveness));
            match outcome {
                Ok(mut outcome) => {
                    let quarantine_sink = crate::exec_env_postflight::DaemonQuarantineSink {
                        server: server_clone.clone(),
                    };
                    match crate::exec_env_postflight::apply_verdict(&mut outcome, &quarantine_sink)
                    {
                        Ok(_) => {
                            append_trajectory_event(
                                &traj_path_for_spawn,
                                outcome.trajectory_event(),
                            );
                            Some(outcome)
                        }
                        Err(error) => {
                            outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                                detail: format!("resource fence persistence failed: {error}"),
                            };
                            append_trajectory_event(
                                &traj_path_for_spawn,
                                outcome.trajectory_event(),
                            );
                            Some(outcome)
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(
                        dispatch_id = %d_id,
                        error = %err,
                        "postflight gate execution failed"
                    );
                    let mut error_outcome = gate.execution_error(
                        &runner_liveness,
                        format!("required gate execution failed: {err}"),
                    );
                    let quarantine_sink = crate::exec_env_postflight::DaemonQuarantineSink {
                        server: server_clone.clone(),
                    };
                    if let Err(fence_error) = crate::exec_env_postflight::apply_verdict(
                        &mut error_outcome,
                        &quarantine_sink,
                    ) {
                        error_outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                            detail: format!(
                                "required gate execution failed: {err}; resource fence persistence failed: {fence_error}"
                            ),
                        };
                    }
                    append_trajectory_event(&traj_path_for_spawn, error_outcome.trajectory_event());
                    Some(error_outcome)
                }
            }
        } else {
            None
        };

        // Required postflight publishes `result.md` while it still holds the
        // publication lease, so a result failure fences the lease instead of
        // returning it for reuse after the carrier artifacts went public.
        let mut result_published_under_lease = false;
        if let Some(outcome) = postflight_outcome.as_mut() {
            // Persist the certified outcome before publishing any carrier
            // artifact. Clean dispatches move into an exclusive `publishing`
            // state; they do not become reusable until publication succeeds.
            let lease_release = match early_exit_cleanup.postflight_dispatch_lease_mut() {
                Some(lease) if outcome.artifacts_released() => lease.begin_publication(),
                Some(lease) if outcome.lease_fenced() => lease.release_after_fence(),
                Some(_) => Err(
                    "postflight lease remains exclusively admitted because its resource fence was not persisted"
                        .to_string(),
                ),
                None => Err("required postflight gate lost its dispatch lease guard".to_string()),
            };
            let publication_admission_held = match lease_release {
                Ok(()) => outcome.artifacts_released(),
                Err(error) => {
                    outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                        detail: format!("postflight lease finalization failed closed: {error}"),
                    };
                    append_trajectory_event(&traj_path_for_spawn, outcome.trajectory_event());
                    false
                }
            };

            if publication_admission_held {
                let mut publication_error = None;
                if publication_error.is_none() {
                    if let Some(deferred) = pending_native_acp_artifacts.take() {
                        if let Err(error) =
                            crate::dispatch_ops::acp_native::publish_native_acp_artifacts(
                                deferred,
                                &workspace_dir_for_spawn,
                                &traj_path_for_spawn,
                                &d_id,
                                &agent_for_watchdog,
                                false,
                            )
                        {
                            publication_error = Some(format!(
                                "postflight approved output but native ACP artifact publication failed: {error}"
                            ));
                        }
                    }
                }
                if publication_error.is_none() {
                    let raw_output = pending_dsh_output.take();
                    let carrier = if raw_output.is_some() {
                        "DSH artifact"
                    } else {
                        "result"
                    };
                    let complete_publication = || {
                        early_exit_cleanup
                            .postflight_dispatch_lease_mut()
                            .ok_or_else(|| {
                                "required postflight publication lost its dispatch lease guard"
                                    .to_string()
                            })?
                            .complete_publication()
                            .map_err(|error| {
                                format!(
                                    "postflight publication finalization failed closed: {error}"
                                )
                            })
                    };
                    match crate::dispatch_ops::dsh::publish_result(
                        &workspace_dir_for_spawn,
                        raw_output.as_deref(),
                        full_output.as_bytes(),
                        managed_ephemeral_credential_cleanup.is_some(),
                        complete_publication,
                    ) {
                        Ok(()) => result_published_under_lease = true,
                        Err(error) => {
                            publication_error = Some(format!(
                            "postflight approved output but {carrier} publication failed: {error}"
                        ))
                        }
                    }
                }
                if let Some(error) = publication_error {
                    outcome.verdict =
                        crate::exec_env_postflight::GateVerdict::Error { detail: error };
                    let quarantine_sink = crate::exec_env_postflight::DaemonQuarantineSink {
                        server: server_clone.clone(),
                    };
                    if let Err(fence_error) =
                        crate::exec_env_postflight::apply_verdict(outcome, &quarantine_sink)
                    {
                        outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                            detail: format!(
                                "carrier artifact publication failed and resource fence persistence failed: {fence_error}"
                            ),
                        };
                    }
                    append_trajectory_event(&traj_path_for_spawn, outcome.trajectory_event());
                }
            }

            if publication_admission_held && !result_published_under_lease {
                let publication_release = match early_exit_cleanup
                    .postflight_dispatch_lease_mut()
                {
                    Some(lease) if outcome.lease_fenced() => lease.release_after_fence(),
                    Some(_) => Err(
                        "postflight publication failed without a persisted resource fence; keeping the lease exclusive"
                            .to_string(),
                    ),
                    None => Err(
                        "required postflight publication lost its dispatch lease guard".to_string(),
                    ),
                };
                if let Err(error) = publication_release {
                    outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                        detail: format!(
                            "postflight publication finalization failed closed: {error}"
                        ),
                    };
                    append_trajectory_event(&traj_path_for_spawn, outcome.trajectory_event());
                }
            }

            if outcome.artifacts_released() {
                append_trajectory_event(
                    &traj_path_for_spawn,
                    json!({
                        "event": "subprocess_output_released",
                        "dispatch_id": d_id,
                        "agent": agent_for_watchdog,
                        "output_tail": tail_chars(&full_output, 500),
                        "timestamp": Utc::now().to_rfc3339(),
                    }),
                );
            }
        }

        let postflight_withheld_error = if postflight_gate_for_spawn.is_some() {
            match &postflight_outcome {
                Some(outcome) if !outcome.artifacts_released() => {
                    Some(outcome.failure_message().unwrap_or_else(|| {
                        "postflight gate withheld dispatch artifacts".to_string()
                    }))
                }
                None => Some("postflight gate execution failed to resolve".to_string()),
                _ => None,
            }
        } else {
            None
        };
        let postflight_artifacts_withheld = postflight_withheld_error.is_some();

        // Save full output to result.md for orchestrator eval (only if postflight permitted release)
        let result_persist_error = if let Some(err) = postflight_withheld_error {
            tracing::warn!(
                dispatch_id = %d_id,
                reason = %err,
                "postflight gate withheld result artifact"
            );
            append_trajectory_event(
                &traj_path_for_spawn,
                json!({
                    "event": "result_withheld_by_postflight",
                    "dispatch_id": d_id,
                    "timestamp": Utc::now().to_rfc3339(),
                    "reason": err,
                }),
            );
            Some(err)
        } else {
            let result_path = workspace_dir.join("result.md");
            // Required postflight consumed the carrier output and published
            // the result while holding the publication lease. Only ungated
            // dispatches publish them here.
            let events_error = pending_dsh_output.take().and_then(|output| {
                crate::dispatch_ops::dsh::publish_output(
                    &workspace_dir,
                    &output,
                    managed_ephemeral_credential_cleanup.is_some(),
                )
                .err()
            });
            let error = if result_published_under_lease {
                None
            } else {
                crate::dispatch_ops::dispatch::persist_dispatch_result_artifact(
                    &result_path,
                    full_output.as_bytes(),
                    managed_ephemeral_credential_cleanup.is_some(),
                )
                .err()
            }
            .or(events_error);
            if let Some(err) = error.as_ref() {
                tracing::warn!(
                    dispatch_id = %d_id,
                    path = %result_path.display(),
                    error = %err,
                    "failed to persist dispatch result artifact"
                );
                append_trajectory_event(
                    &traj_path_for_spawn,
                    json!({
                        "event": "result_persist_failed",
                        "dispatch_id": d_id,
                        "timestamp": Utc::now().to_rfc3339(),
                        "path": result_path.to_string_lossy(),
                        "error": err,
                    }),
                );
            }
            error
        };

        // Keep the publication lease until the untruncated result is durable.
        // Any failure leaves the armed guard to quarantine its exact resources.
        if native_staff_sandbox && native_descendants_reaped && !native_staff_lifecycle_failed {
            let publication = if result_persist_error.is_none() {
                early_exit_cleanup
                    .postflight_dispatch_lease_mut()
                    .ok_or_else(|| "native Session lost its publication lease".to_string())
                    .and_then(|lease| lease.complete_publication())
            } else {
                Err("native Session result publication failed".to_string())
            };
            if let Err(error) = publication {
                native_staff_lifecycle_failed = true;
                result = Err(error.clone());
                if let Some(receipt) = native_staff_lifecycle.as_mut() {
                    receipt["resource_fence"] = json!("failed");
                    receipt["workspace_cleanup"] = json!("retained");
                    receipt["error"] = json!(error);
                }
            }
        }

        // --- WATCHDOG: check if sub-agent properly closed the loop ---
        // Poll for kanban state instead of a fixed sleep to avoid race conditions
        let (watchdog_polls, watchdog_interval) = watchdog_poll_config();
        let mut receipt_terminal_state = None;
        let mut pending_completion_recovery = false;
        let mut receipt_read_error = None;
        let mut kanban_state = None;
        // A runner that has reaped the managed group and returned its
        // cancellation proof already owns the terminal classification.  Do
        // not feed that result through the ordinary watchdog path: it would
        // synthesize a failure eval/outcome before the terminal writer has
        // persisted the cancellation receipt.
        let managed_cancellation_confirmed =
            matches!(&result, Err(error) if error == "managed_cancelled");
        if managed_cancellation_confirmed {
            receipt_terminal_state = Some("TASK_STATE_CANCELED");
        }
        if !managed_cancellation_confirmed {
            match completion_receipt_state_after_admission(
                &workspace_dir_for_spawn,
                watchdog_interval,
                &server_clone.managed_run_controls,
                &d_id,
                managed_cancellation.as_ref(),
            )
            .await
            {
                Ok(CompletionReceiptState::Terminal(receipt_state)) => {
                    receipt_terminal_state = Some(receipt_state);
                }
                Ok(CompletionReceiptState::PendingRecovery) => pending_completion_recovery = true,
                Ok(CompletionReceiptState::Open) => {}
                Ok(CompletionReceiptState::AdmissionInProgress) => {
                    unreachable!("admission wait resolves")
                }
                Err(error) => receipt_read_error = Some(error),
            }
        }
        for _ in 0..watchdog_polls {
            if managed_cancellation_confirmed
                || receipt_terminal_state.is_some()
                || pending_completion_recovery
                || receipt_read_error.is_some()
            {
                break;
            }
            tokio::time::sleep(watchdog_interval).await;
            match completion_receipt_state_after_admission(
                &workspace_dir_for_spawn,
                watchdog_interval,
                &server_clone.managed_run_controls,
                &d_id,
                managed_cancellation.as_ref(),
            )
            .await
            {
                Ok(CompletionReceiptState::Terminal(receipt_state)) => {
                    receipt_terminal_state = Some(receipt_state);
                    break;
                }
                Ok(CompletionReceiptState::PendingRecovery) => {
                    pending_completion_recovery = true;
                    break;
                }
                Ok(CompletionReceiptState::AdmissionInProgress) => {
                    unreachable!("admission wait resolves")
                }
                Ok(CompletionReceiptState::Open) => {}
                Err(error) => {
                    receipt_read_error = Some(error);
                    break;
                }
            }
            let state = get_kanban_state(&server_clone, &d_id).await;
            if canonical_terminal_state(state.as_deref()).is_some() {
                kanban_state = state;
                break;
            }
        }
        let kanban_state = match kanban_state {
            Some(s) => Some(s),
            None => get_kanban_state(&server_clone, &d_id).await,
        };
        if receipt_terminal_state.is_none()
            && !pending_completion_recovery
            && receipt_read_error.is_none()
        {
            match completion_receipt_state_after_admission(
                &workspace_dir_for_spawn,
                watchdog_interval,
                &server_clone.managed_run_controls,
                &d_id,
                managed_cancellation.as_ref(),
            )
            .await
            {
                Ok(CompletionReceiptState::Terminal(receipt_state)) => {
                    receipt_terminal_state = Some(receipt_state)
                }
                Ok(CompletionReceiptState::PendingRecovery) => pending_completion_recovery = true,
                Ok(CompletionReceiptState::AdmissionInProgress) => {
                    unreachable!("admission wait resolves")
                }
                Ok(CompletionReceiptState::Open) => {}
                Err(error) => receipt_read_error = Some(error),
            }
        }
        if native_staff_lifecycle_failed {
            pending_completion_recovery = false;
            receipt_terminal_state = Some("TASK_STATE_FAILED");
            if let Err(error) =
                update_kanban_state(&server_clone, &d_id, "TASK_STATE_FAILED", None, Some(false))
                    .await
            {
                tracing::warn!(dispatch_id = %d_id, %error, "native Staff lifecycle failure kanban update failed");
            }
            crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                &server_clone,
                &d_id,
                "native_staff_retention_failed",
                Some(agent_for_watchdog.as_str()),
                project_for_watchdog.as_deref(),
            );
        }
        let polled_terminal_state = canonical_terminal_state(kanban_state.as_deref());
        let is_closed = !pending_completion_recovery
            && (receipt_terminal_state.is_some() || polled_terminal_state.is_some());
        if receipt_terminal_state == Some("TASK_STATE_FAILED") && !native_staff_lifecycle_failed {
            crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                &server_clone,
                &d_id,
                "termination_unconfirmed",
                Some(agent_for_watchdog.as_str()),
                project_for_watchdog.as_deref(),
            );
        }
        // #1250: terminal accounting in the final `status.json` rewrite must
        // reflect the resolved predicate verdict, NOT the raw process exit
        // code. The watchdog branch below is the only path that actually
        // evaluates the predicate (via the same `resolve_completion_state`
        // machinery used at the success seam) — when it runs and resolves a
        // state, capture it here so the status write prefers it over the
        // exit-code derivation. Stays `None` when the watchdog branch does
        // not run (run already closed via `tachi_complete`) or runs the
        // crash/timeout sub-branch, preserving the existing exit-code
        // semantics for those non-predicate paths.
        let mut watchdog_resolved_state = receipt_read_error.as_ref().map(|error| {
            eprintln!(
                "[watchdog] refusing completion status read for dispatch {}: {}",
                d_id, error
            );
            "TASK_STATE_FAILED"
        });
        if pending_completion_recovery {
            eprintln!(
                "[watchdog] dispatch {} has a pending canonical-outcome recovery receipt; \
                 skipping terminalization until tachi_complete reconciles it",
                d_id
            );
        }
        if !is_closed && !pending_completion_recovery && watchdog_resolved_state.is_none() {
            let exited_ok = matches!(&result, Ok(r) if r.exit_code == Some(0));

            if exited_ok {
                // exit_code=0 but no tachi_complete: sub-agent forgot to
                // close the loop. Apply the #878-A completion predicate when
                // declared — an unsatisfied artifact/output contract is a
                // FALSE SUCCESS and must land FAILED, not COMPLETED. When no
                // predicate is declared, keep the conservative unreviewed
                // COMPLETED (no synthesized success eval for routing stats).
                let tail = if postflight_artifacts_withheld {
                    "[withheld by exec_env_postflight]".to_string()
                } else {
                    tail_chars(&full_output, 500)
                };
                let verdict = match crate::dispatch_ops::evaluate_completion_predicate_for_dispatch(
                    &server_clone.tachi_home_dir(),
                    &d_id,
                    &full_output,
                ) {
                    Ok((_, verdict)) => verdict,
                    Err(error) => {
                        eprintln!(
                            "[watchdog] refusing completion artifact read for dispatch {}: {}",
                            d_id, error
                        );
                        crate::dispatch_ops::PredicateVerdict::Fail(format!(
                            "completion artifact read refused: {error}"
                        ))
                    }
                };
                let (kanban_state, reviewed, override_reason) =
                    crate::dispatch_ops::resolve_completion_state("success", &verdict);
                // #1250: thread the predicate-resolved state into the final
                // `status.json` rewrite so an exit-0 run whose predicate
                // verdict is FAILURE lands FAILED there too — not collapsed
                // back to COMPLETED via the raw exit code.
                watchdog_resolved_state = Some(kanban_state);
                eprintln!(
                    "[watchdog] dispatch {} exited 0 without tachi_complete; predicate={} → {} reviewed={} tail={}",
                    d_id,
                    verdict.tag(),
                    kanban_state,
                    reviewed,
                    tail
                );
                if let Some(reason) = override_reason {
                    eprintln!("[watchdog] false-success intercepted: {reason}");
                }
                if let Err(error) =
                    update_kanban_state(&server_clone, &d_id, kanban_state, None, Some(reviewed))
                        .await
                {
                    eprintln!(
                        "[watchdog] failed to mark dispatch {} {} in kanban: {}",
                        d_id, kanban_state, error
                    );
                }
                // #773 Layer-2 ② (hole b): exit-0-without-tachi_complete that
                // the predicate intercepts as a FALSE SUCCESS is a terminal
                // FAILED the agent never `tachi_complete`d — record a canonical
                // outcome row (reported_outcome NULL — no self-report reached us).
                if kanban_state == "TASK_STATE_FAILED" {
                    crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                        &server_clone,
                        &d_id,
                        "watchdog",
                        Some(agent_for_watchdog.as_str()),
                        project_for_watchdog.as_deref(),
                    );
                }
            } else {
                // Crash / timeout / error: record a failure eval so the
                // failure is still visible in the ledger, but tag it as
                // `auto_synthesized=true` so the daily routing analysis can
                // exclude synthesized records from agent success-rate stats.
                let note = match &result {
                    Err(e) => format!("Watchdog: {}", e),
                    Ok(r) => {
                        let tail = tail_chars(&r.output, 500);
                        format!(
                            "Watchdog: Agent crashed (exit {:?}). Stderr tail: {}",
                            r.exit_code, tail
                        )
                    }
                };

                let ts = Utc::now();
                let eval_id = format!(
                    "eval_ws_{}_{}",
                    ts.format("%Y%m%dT%H%M%SZ"),
                    d_id.chars().take(16).collect::<String>()
                );
                let metadata = json!({
                    "task_id": eval_id,
                    "agent": format!("watchdog/{}", agent_for_watchdog),
                    "outcome": "failure",
                    "dispatch_id": d_id,
                    // Nightly routing analysis must exclude these so "fake"
                    // failures attributed to the watchdog agent don't pollute
                    // the real backend's success-rate.
                    "auto_synthesized": true,
                });
                let save_params = SaveMemoryParams {
                    text: note.clone(),
                    summary: format!("Watchdog auto-close FAILURE: {}", d_id),
                    path: format!("/eval/{}/{}", ts.format("%Y%m%d"), eval_id),
                    importance: 0.4,
                    category: "experience".to_string(),
                    topic: "eval".to_string(),
                    keywords: vec![
                        "eval".to_string(),
                        "watchdog".to_string(),
                        "failure".to_string(),
                        "auto_synthesized".to_string(),
                    ],
                    persons: Vec::new(),
                    entities: Vec::new(),
                    location: String::new(),
                    scope: "project".to_string(),
                    vector: None,
                    id: Some(eval_id.clone()),
                    force: true,
                    auto_link: false,
                    project: None,
                    project_explicit: false,
                    retention_policy: Some("durable".to_string()),
                    domain: Some("system".to_string()),
                    timestamp: None,
                    valid_from: None,
                    valid_until: None,
                    metadata: Some(metadata),
                    emit_continuity: false,
                    supersedes: Vec::new(),
                };
                if let Err(error) =
                    crate::memory_search_ops::handle_save_memory(&server_clone, save_params).await
                {
                    eprintln!(
                        "[watchdog] failed to persist synthesized failure eval for dispatch {}: {}",
                        d_id, error
                    );
                }

                if let Err(error) = update_kanban_state(
                    &server_clone,
                    &d_id,
                    "TASK_STATE_FAILED",
                    Some(&eval_id),
                    Some(false),
                )
                .await
                {
                    eprintln!(
                        "[watchdog] failed to mark dispatch {} FAILED in kanban: {}",
                        d_id, error
                    );
                }
                // #773 Layer-2 ② (hole b): crash/timeout is a terminal FAILED
                // the agent never `tachi_complete`d — record a canonical outcome
                // row so the router learns from the failure (not just successes).
                crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                    &server_clone,
                    &d_id,
                    "watchdog",
                    Some(agent_for_watchdog.as_str()),
                    project_for_watchdog.as_deref(),
                );
            }
        }

        let should_cleanup = managed_ephemeral_credential_cleanup.is_some()
            || match &result {
                Ok(r) if !pending_completion_recovery => {
                    should_cleanup_run(r.exit_code, kanban_state.as_deref())
                }
                Err(_) => false,
                Ok(_) => false,
            };
        // A managed cancellation is confirmed only if its ephemeral
        // credentials are already gone.  The cleanup result is therefore an
        // input to the one canonical terminal write below, never a later
        // downgrade of a committed confirmation.
        let credential_cleanup_failed = native_credential_cleanup_error.is_some();
        let credential_cleanup_failed = (if should_cleanup {
            let credential_cleanup = {
                #[cfg(test)]
                if managed_credential_cleanup_failure_injected() {
                    Err("injected managed credential cleanup failure".to_string())
                } else {
                    server_clone.with_global_store(|store| {
                        cleanup_ephemeral_credential_materializations(
                            store,
                            &workspace_dir_for_spawn,
                            false,
                        )
                    })
                }
                #[cfg(not(test))]
                {
                    server_clone.with_global_store(|store| {
                        cleanup_ephemeral_credential_materializations(
                            store,
                            &workspace_dir_for_spawn,
                            false,
                        )
                    })
                }
            };
            let failed = match &credential_cleanup {
                Ok(report) => !report.errors.is_empty(),
                Err(_) => true,
            };
            append_trajectory_event(
                &traj_path_for_spawn,
                json!({
                    "event": "credentials_cleanup",
                    "dispatch_id": d_id,
                    "agent": agent_for_watchdog,
                    "report": credential_cleanup
                        .as_ref()
                        .map(|report| serde_json::to_value(report).unwrap_or_else(|_| json!({"error": "serialize cleanup report"})))
                        .unwrap_or_else(|err| json!({"errors": [err]})),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            append_trajectory_event(
                &traj_path_for_spawn,
                json!({
                    "event": "workspace_retained",
                    "dispatch_id": d_id,
                    "reason": "run_dir is retained so board/status links remain valid",
                    "run_dir": workspace_dir.to_string_lossy(),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            failed
        } else {
            false
        }) || credential_cleanup_failed;

        // Final audit: dispatch_finished + status.json refresh.
        let final_exit_code = match &result {
            Ok(r) => r.exit_code,
            Err(_) => None,
        };
        let total_duration_ms = plan_duration_ms_for_spawn.unwrap_or(0) + execute_duration_ms;

        append_trajectory_event(
            &traj_path_for_spawn,
            json!({
                "event": "dispatch_finished",
                "dispatch_id": d_id,
                "agent": agent_for_watchdog,
                "stage": stage_for_traj,
                "v2": v2_for_spawn,
                "exit_code": final_exit_code,
                "duration_ms_execute": execute_duration_ms,
                "duration_ms_plan": plan_duration_ms_for_spawn,
                "total_duration_ms": total_duration_ms,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );

        // Preserve dispatch-time contract fields across the final status rewrite
        // so complete/watchdog can still evaluate the #878-A predicate after exit.
        let status_path = workspace_dir_for_spawn.join("status.json");
        let managed_terminal_anchor = managed_terminal_status_anchor(
            &server_clone.managed_run_controls,
            &d_id,
            managed_cancellation.as_ref(),
        );
        let (prev_status, final_status_read_error) = match read_status_json_for_terminal(
            &workspace_dir_for_spawn,
            &status_path,
            &managed_terminal_anchor,
        ) {
            Ok(status) => (status, None),
            Err(error) => (None, Some(error)),
        };
        let preserved_predicate = prev_status
            .as_ref()
            .and_then(|v| v.get("completion_predicate").cloned())
            .unwrap_or(Value::Null);
        let preserved_cwd = prev_status
            .as_ref()
            .and_then(|v| v.get("cwd").cloned())
            .unwrap_or(Value::Null);
        // #894 S2d: the effective-authority receipt is a dispatch-time fact —
        // it must survive the terminal rewrite, or "who was actually stopping
        // this agent from writing?" becomes unanswerable the moment the run
        // finishes, which is exactly when someone asks.
        let preserved_authority = prev_status
            .as_ref()
            .and_then(|v| v.get("authority").cloned())
            .unwrap_or(Value::Null);

        let artifact_read_error = final_status_read_error.or(receipt_read_error);
        if let Some(error) = artifact_read_error.as_deref() {
            eprintln!(
                "[watchdog] completion artifact refusal persisted for dispatch {}: {}",
                d_id, error
            );
        }
        let final_status_state = if native_staff_lifecycle_failed || result_persist_error.is_some()
        {
            "TASK_STATE_FAILED"
        } else if matches!(&result, Err(error) if error == "managed_cancelled")
            && credential_cleanup_failed
        {
            "TASK_STATE_FAILED"
        } else if matches!(&result, Err(error) if error == "managed_cancelled") {
            "TASK_STATE_CANCELED"
        } else if matches!(&result, Err(error) if error == "termination_unconfirmed" || error.starts_with("managed cancellation child probe failed"))
        {
            "TASK_STATE_FAILED"
        } else if pending_completion_recovery {
            pending_recovery_status_state(prev_status.as_ref())
        } else if artifact_read_error.is_some() {
            "TASK_STATE_FAILED"
        } else {
            terminal_status_state(
                receipt_terminal_state,
                watchdog_resolved_state,
                polled_terminal_state,
                final_exit_code,
            )
        };
        // A dequeued managed cancellation is finalized by this background
        // owner, after the result artifact and all terminal accounting have
        // been chosen. The runner only owns process lifetime; it never wakes
        // the caller with a speculative receipt.
        let managed_finalization = managed_cancellation.as_ref().map(|command| {
            json!({
                "expected_status_revision": command.expected_status_revision,
                "runner_error": result.as_ref().err(),
                "termination_proof": managed_termination_proof,
                "credential_cleanup_failed": credential_cleanup_failed,
                "result_persist_failed": result_persist_error.is_some(),
            })
        });

        let managed_cancel_completion = write_status_json_for_terminal(
            &workspace_dir_for_spawn,
            &d_id,
            v2_for_spawn,
            plan_generated_at_for_spawn.as_deref(),
            Some(&execute_started_at.to_rfc3339()),
            if v2_for_spawn { "approved" } else { "n/a" },
            final_exit_code,
            plan_duration_ms_for_spawn,
            Some(execute_duration_ms),
            Some(total_duration_ms),
            Some(json!({
                "agent": agent_for_watchdog.clone(),
                "state": final_status_state,
                "closure_kind": terminal_closure_kind(final_status_state),
                "updated_at": Utc::now().to_rfc3339(),
                "run_dir": workspace_dir_for_spawn.to_string_lossy(),
                "result_written": result_persist_error.is_none(),
                "result_persist_error": result_persist_error,
                "native_credential_cleanup_error": native_credential_cleanup_error,
                "artifact_read_error": artifact_read_error,
                "completion_predicate": preserved_predicate,
                "cwd": preserved_cwd,
                "env_id": env_id_for_watchdog,
                "project": project_for_watchdog.clone(),
                "authority": preserved_authority,
                "harness_transport": harness_transport_for_spawn.clone(),
                "harness_server_url": harness_server_url_for_spawn.clone(),
                "host_adapter": host_adapter_for_spawn.clone(),
                "execution_backend": crate::dispatch_ops::dispatch::backend::execution_backend_name(
                    &agent_for_watchdog,
                    &harness_transport_for_spawn,
                ),
                "acpx": if is_acpx_transport(&harness_transport_for_spawn) {
                    execution_backend_metadata_for_spawn.clone()
                } else {
                    None
                },
                "acp_native": if is_native_acp_transport(&harness_transport_for_spawn) {
                    execution_backend_metadata_for_spawn.clone()
                } else {
                    None
                },
                "acpx_events": null,
                "feedback_rules": feedback_rules_trace_for_spawn,
                "timeout_secs": timeout_secs_for_spawn,
                "managed_cancellation_finalization": managed_finalization,
                "exec_env_postflight": postflight_outcome
                    .as_ref()
                    .map(|o| o.receipt())
                    .or_else(|| native_staff_lifecycle.clone())
                    .unwrap_or_else(|| {
                        json!({
                            "gate": "exec_env_postflight",
                            "status": "not_applicable"
                        })
                    }),
            })),
            managed_terminal_anchor,
        );

        if matches!(
            managed_cancel_completion,
            Some(crate::managed_run_control::CancelCompletion::Confirmed { .. })
        ) {
            if let Err(error) = update_kanban_state(
                &server_clone,
                &d_id,
                "TASK_STATE_CANCELED",
                None,
                Some(false),
            )
            .await
            {
                eprintln!(
                    "[watchdog] failed to mark cancelled dispatch {}: {}",
                    d_id, error
                );
            }
        }
        if let Some(crate::managed_run_control::CancelCompletion::Unavailable(reason)) =
            managed_cancel_completion.as_ref()
        {
            if *reason == "credential_cleanup_failed"
                || *reason == "persist_failed"
                || *reason == "result_persist_failed"
            {
                if let Err(error) = update_kanban_state(
                    &server_clone,
                    &d_id,
                    "TASK_STATE_FAILED",
                    None,
                    Some(false),
                )
                .await
                {
                    eprintln!(
                        "[watchdog] failed to mark managed terminal failure {}: {}",
                        d_id, error
                    );
                }
                crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                    &server_clone,
                    &d_id,
                    reason,
                    Some(agent_for_watchdog.as_str()),
                    project_for_watchdog.as_deref(),
                );
            }
        }
        // The gate and runner cwd authority pin the exact worktree directory
        // with open descriptors so pathname replacement cannot fool either
        // boundary. Release those parent-held pins before certified cleanup
        // probes for live holders; otherwise the daemon blocks removal on its
        // own safety descriptors.
        drop(postflight_gate_for_spawn);
        drop(cwd_authority_for_spawn);
        if (native_descendants_reaped && native_staff_sandbox && !native_staff_lifecycle_failed)
            || postflight_outcome
                .as_ref()
                .is_some_and(crate::exec_env_postflight::GateOutcome::artifacts_released)
        {
            if let Some(owned) = auto_staff_exec_env {
                owned.cleanup("clean automatic Staff postflight completion");
            }
        }
        early_exit_cleanup.complete();
        // The response itself is a lifecycle receipt: it is released only
        // once credential cleanup, slot release, and registry removal are all
        // complete. No detached responder can outlive this owner.
        drop(_managed_run_guard);
        if let Some(command) = managed_cancellation.take() {
            #[cfg(test)]
            let mut command = command;
            #[cfg(test)]
            if let Some(observation) = command.test_observation.take() {
                observation.complete(&workspace_dir_for_spawn, managed_cancel_completion.as_ref());
            }
            if let Some(completion) = managed_cancel_completion {
                let _ = command.response.send(completion);
            }
        }
        #[cfg(test)]
        mark_background_dispatch_cleanup_complete(&d_id);
    };
    let background_dispatch = tokio::task::spawn(crate::staffing_ops::presence::observe_runner(
        observer, background,
    ));
    // Production detaches the dispatch: nobody joins it, and dropping the
    // handle keeps it running. A test-only capture seam can take the handle so
    // a regression test can abort the detached owner at a chosen lifecycle
    // boundary (a paused postflight gate); without a capture installed the
    // handle is dropped here exactly as before.
    #[cfg(test)]
    hand_over_background_dispatch_handle_to_capture(background_dispatch);
    #[cfg(not(test))]
    drop(background_dispatch);
}
