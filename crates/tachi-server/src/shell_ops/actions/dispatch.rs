use super::convoy::handle_convoy_dispatch_action;
use super::*;

pub(super) async fn handle_dispatch_action(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let task = params
        .task
        .clone()
        .ok_or_else(|| "'task' is required for action='dispatch'".to_string())?;
    let (flow_id, run_dir, created) = resolve_or_create_flow(&params, &task)?;
    let injection = inject_meta_skill("dispatch", &run_dir).await;

    let instruction = build_instruction_md(
        &flow_id,
        "dispatch",
        &task,
        &injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    let instr_path = run_dir.join("instruction.md");
    tokio::fs::write(&instr_path, &instruction)
        .await
        .map_err(|e| format!("write instruction.md: {e}"))?;

    advance_stage(&run_dir, &flow_id, "dispatch", &task, &injection, created)?;
    let required_skills = crate::skill_policy::shell_stage_skills("dispatch");

    if !params.slices.is_empty() {
        return handle_convoy_dispatch_action(
            server,
            params,
            &flow_id,
            &run_dir,
            created,
            &injection,
            &instr_path,
        )
        .await;
    }

    // Phase 4 hook: optionally invoke the existing async dispatcher.
    let mut dispatch_id: Option<String> = None;
    let mut dispatch_error: Option<String> = None;
    let mut async_fired = false;
    if params.async_dispatch {
        let agent = params.agent.clone().or_else(|| {
            if params.profile.is_some() {
                None
            } else {
                Some("claude".to_string())
            }
        });
        // Prefix the subagent prompt with a pointer to the instruction packet
        // so the clanker reads from disk rather than chat context.
        let prompt = format!(
            "You are executing Tachi flow `{flow_id}`, stage `dispatch`.\n\n\
             Read and follow these injected SOP files before changing code:\n\
             - {injected}\n\n\
             Then read the full instruction packet:\n\
             - {instr}\n\n\
             Original task:\n\n{task}\n",
            flow_id = flow_id,
            injected = injection.injected_path.as_deref().unwrap_or("(none)"),
            instr = instr_path.to_string_lossy(),
            task = task,
        );
        let dp = TachiDispatchParams {
            agent,
            profile: params.profile.clone(),
            task: prompt,
            execution_level: None,
            cwd: params.cwd.clone(),
            // Shell dispatch supplies a bare cwd; declare it unmanaged so the
            // fail-safe env gate accepts it and stamps `env: unmanaged` (#894
            // S1 §1.3 escape hatch until an env_id bridge exists here).
            env_id: None,
            unmanaged_cwd: Some(true),
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 600,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: params.project.clone(),
            stage: Some("execute".to_string()),
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: Some(flow_id.to_string()),
            tool_profile: params.tool_profile.clone(),
            auto_capability_bundle: None,
            mcp_access: params.mcp_access.clone(),
            allowed_mcp_servers: params.allowed_mcp_servers.clone(),
        };
        match crate::dispatch_ops::handle_tachi_dispatch(server, dp).await {
            Ok(s) => {
                async_fired = true;
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    if let Some(d) = v.get("dispatch_id").and_then(|d| d.as_str()) {
                        dispatch_id = Some(d.to_string());
                    }
                }
                // Append dispatch_id to flow status
                if let Some(d) = dispatch_id.as_deref() {
                    let mut status = read_status_async(&run_dir).await;
                    let arr = status
                        .as_object_mut()
                        .map(|o| o.entry("dispatch_ids").or_insert_with(|| json!([])));
                    if let Some(v) = arr {
                        if let Some(a) = v.as_array_mut() {
                            a.push(json!(d));
                        }
                    }
                    if let Err(error) = crate::utils::write_run_status_file(&run_dir, &status) {
                        tracing::warn!(
                            error = %error,
                            run_dir = %run_dir.display(),
                            dispatch_id = %d,
                            "failed to persist shell dispatch id in flow status"
                        );
                    }
                    if let Err(error) = crate::utils::append_run_event(
                        &run_dir,
                        json!({
                            "event": "dispatch_spawned",
                            "flow_id": flow_id,
                            "dispatch_id": d,
                            "timestamp": Utc::now().to_rfc3339(),
                        }),
                    ) {
                        tracing::warn!(
                            error = %error,
                            run_dir = %run_dir.display(),
                            dispatch_id = %d,
                            "failed to append shell dispatch event"
                        );
                    }
                }
            }
            Err(e) => {
                dispatch_error = Some(e);
            }
        }
    }

    let resp = json!({
        "flow_id": flow_id,
        "stage": "dispatch",
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(&injection),
        "native_skill_policy": crate::skill_policy::native_policy_summary("dispatch", &required_skills),
        "async": async_fired,
        "dispatch_id": dispatch_id,
        "dispatch_error": dispatch_error,
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}
