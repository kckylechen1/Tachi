//! Explicit inputs for retained subprocess kernel regressions. Compiled only
//! in tests; no production profile lookup or public Staff admission is changed.

use super::*;

pub(crate) async fn launch_staff_kernel_fixture(
    server: &MemoryServer,
    mut request: StaffAssignmentRequest,
) -> Result<StaffLaunchOutcome, String> {
    if request.submission_id.is_some() {
        return Err("retained kernel fixtures do not implement submission recovery".into());
    }
    let label = request.profile.as_deref().unwrap_or_default();
    let verbose = label == "kernel_opencode_credentials";
    let (backend, selected_profile, role, stage, model, credentials, sandbox, github_read) =
        match label {
            "kernel_codex_review" => (
                "codex",
                label.to_string(),
                "senior_reviewer",
                "review",
                None,
                Vec::new(),
                Some("read-only".to_string()),
                true,
            ),
            "kernel_opencode_write" | "kernel_opencode_credentials" => (
                "custom",
                label.to_string(),
                "executor",
                "execute",
                tachi_dispatch::resolve_dispatch_model(tachi_dispatch::GLM_CODING_MODEL_ALIAS),
                if label == "kernel_opencode_credentials" {
                    vec!["opencode_shared".to_string()]
                } else {
                    Vec::new()
                },
                None,
                false,
            ),
            // Keep the real read-only authority refusal, not a synthetic error.
            "kernel_uncertified_read" => (
                "custom",
                "deepseek_explore".to_string(),
                "explore",
                "explore",
                Some("deepseek/deepseek-v4-flash".to_string()),
                Vec::new(),
                None,
                false,
            ),
            _ => return Err("select an explicit retained kernel fixture label".into()),
        };
    if request.worker.as_deref() != Some(backend) {
        return Err("retained kernel fixture worker does not match its fixed lane".into());
    }
    start::bind_staff_project(server, &mut request)?;
    enforce_dispatch_depth(server)?;
    let (_, execution_level) = crate::host_profile::authorize_dispatch(request.execution_level)?;
    crate::task_lifecycle::ownership::producer_preflight(server, request.flow_id.as_deref())?;
    let recommendation = request
        .recommendation_ref
        .as_deref()
        .map(|reference| {
            server
                .with_global_store_read(|store| {
                    memcore::get_route_recommendation(store.connection(), reference)
                        .map_err(|error| error.to_string())
                })?
                .ok_or_else(|| format!("Unknown or stale recommendation_ref '{reference}'"))
        })
        .transpose()?;
    if request.stage.is_none() {
        request.stage = Some(stage.to_string());
    }
    let dispatch_id = new_dispatch_id(Utc::now(), backend);
    let command = if backend == "custom" {
        vec![
            "opencode".into(),
            "--standalone".into(),
            "run".into(),
            "--model".into(),
            model.clone().expect("fixed custom fixture model"),
        ]
    } else {
        Vec::new()
    };
    let transport = if backend == "custom" {
        "opencode_cli"
    } else {
        "cli"
    };
    let host_adapter = (backend == "custom").then(|| "opencode".to_string());
    let writable = role == "executor";
    let access = tachi_params::DispatchMcpAccessParams {
        inject_tachi_mcp: Some(false),
        allowed_facades: if writable {
            vec![
                "tachi_memory".into(),
                "tachi_event".into(),
                "tachi_task".into(),
            ]
        } else {
            vec![
                "tachi_memory".into(),
                "tachi_event".into(),
                "tachi_wiki".into(),
                "tachi_task".into(),
            ]
        },
        allowed_mcp_servers: Vec::new(),
        github_read: Some(github_read),
        write_actions: Some(writable),
        issue_refs: request.issue_ref.iter().cloned().collect(),
        pr_refs: request.pr_ref.iter().cloned().collect(),
        fallback: None,
    };
    let (release, provider_model, version) = tachi_dispatch::provider_model_parts(model.as_deref());
    let identity = tachi_dispatch::DispatchIdentityReceipt::planned(
        tachi_dispatch::DispatchIdentityRequest {
            profile: request.profile.clone(),
            model: None,
            agent: request.worker.clone(),
            harness: None,
        },
        tachi_dispatch::DispatchIdentityEffective {
            profile: Some(selected_profile.clone()),
            model: model.clone(),
            model_lineage_id: tachi_dispatch::model_lineage_id(model.as_deref(), backend),
            concrete_model_release: release,
            provider_model,
            provider_model_version: version,
            backend: backend.into(),
            harness: host_adapter.clone().unwrap_or_else(|| backend.into()),
            role: role.into(),
            seat: tachi_dispatch::UNKNOWN_IDENTITY.into(),
            transport: transport.into(),
            adapter_version: tachi_dispatch::UNKNOWN_IDENTITY.into(),
            carrier_version: tachi_dispatch::UNKNOWN_IDENTITY.into(),
        },
        "explicit retained kernel fixture; not public Staff admission".into(),
        false,
    );
    let (skills, evidence): (Vec<String>, Vec<String>) = if writable {
        (
            [
                tachi_dispatch::SUPERPOWER_EXECUTING_PLANS,
                tachi_dispatch::WAZA_TACHI,
                tachi_dispatch::CODING_TEST_STRATEGY,
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            ["diff", "tests_run", "files_changed"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        )
    } else {
        (
            [
                tachi_dispatch::SUPERPOWER_REQUESTING_CODE_REVIEW,
                tachi_dispatch::SUPERPOWER_VERIFICATION_BEFORE_COMPLETION,
                tachi_dispatch::WAZA_CHECK,
                tachi_dispatch::CODING_REFACTOR_CHECKLIST,
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            ["findings_by_severity", "file_refs", "verification_advice"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        )
    };
    let resolved_profile = ResolvedDispatchProfile {
        selected_profile: Some(selected_profile.clone()),
        agent: backend.into(),
        selected_model: model.clone(),
        launch_command: command.clone(),
        harness_transport: Some(transport.into()),
        harness_server_url: None,
        role: Some(role.into()),
        tool_profile: Some(if writable { "delegate" } else { "standard" }.into()),
        mcp_access: access.clone(),
        required_skills: skills.clone(),
        evidence_required: evidence.clone(),
        fallback_chain: Vec::new(),
        credential_profiles: credentials.clone(),
        route_explanation: vec!["retained kernel regression fixture".into()],
        host_adapter: host_adapter.clone(),
        identity_receipt: identity,
    };
    let assignment = tachi_params::ResolvedStaffAssignment {
        assignment_id: dispatch_id.clone(),
        staffing_reason: request.staffing_reason,
        selected_worker: backend.into(),
        selected_profile: Some(selected_profile),
        selected_backend: backend.into(),
        selected_model: model.clone(),
        execution_level: Some(execution_level),
        recommendation_ref: request.recommendation_ref.clone(),
        host_adapter,
        evidence_required: evidence,
        fallback_chain: Vec::new(),
        route_explanation: resolved_profile.route_explanation.clone(),
        identity_receipt: serde_json::to_value(&resolved_profile.identity_receipt).unwrap(),
    };
    let mechanics = DispatchLaunchMechanics {
        cwd: None,
        env_id: None,
        unmanaged_cwd: false,
        skills: skills.clone(),
        model,
        permission_profile: None,
        allowed_tools: Vec::new(),
        max_turns: None,
        sandbox,
        command: command.clone(),
        credential_profiles: credentials.clone(),
        mcp_access: Some(access),
        inject_tachi_mcp: Some(false),
        allowed_mcp_servers: Vec::new(),
        timeout_secs: 600,
        declared_file_scope: request.declared_file_scope.clone(),
    };
    let mut start = DispatchStart {
        dispatch_id: dispatch_id.clone(),
        submission: None,
        request,
        requested_skills: skills,
        context_query: None,
        tool_profile: resolved_profile.tool_profile.clone(),
        command,
        harness_transport: Some(transport.into()),
        harness_server_url: None,
        raw_cwd: None,
        raw_credential_profiles: credentials,
        agent_norm: backend.into(),
        profile_payload: serde_json::to_value(&resolved_profile).unwrap(),
        resolved_profile,
        resolved_assignment: assignment.clone(),
        resolved_recommendation: recommendation.clone(),
        workspace_dir: dispatch_runs_root().join(dispatch_id),
        inject_card: true,
        verbose,
        mechanics,
        auto_staff_exec_env: None,
    };
    provision_staff_exec_env(server, &mut start)?;
    let raw = launch_canonical_dispatch(server, start, ManagedControlOrigin::StaffFacade).await?;
    Ok(StaffLaunchOutcome {
        raw,
        assignment: Some(assignment),
        recommendation,
    })
}
