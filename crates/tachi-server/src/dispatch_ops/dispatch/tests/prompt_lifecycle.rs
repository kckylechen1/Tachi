use super::*;
use serde_json::{json, Value};

fn mcp_access(servers: &[&str], inject_tachi_mcp: bool) -> tachi_params::DispatchMcpAccessParams {
    tachi_params::DispatchMcpAccessParams {
        inject_tachi_mcp: Some(inject_tachi_mcp),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["typed-facade".to_string()],
        allowed_mcp_servers: servers.iter().map(|server| (*server).to_string()).collect(),
        github_read: Some(true),
        write_actions: Some(false),
        issue_refs: vec!["#1817".to_string()],
        pr_refs: vec!["#1817".to_string()],
        fallback: Some("typed-fallback".to_string()),
    }
}

pub(super) fn typed_context() -> (
    tachi_params::StaffAssignmentRequest,
    tachi_params::ResolvedStaffAssignment,
    tachi_params::ExecutionGrant,
    crate::dispatch_profile::ResolvedDispatchProfile,
    Vec<String>,
) {
    let request = tachi_params::StaffAssignmentRequest::new(
        tachi_params::TachiDispatchReason::ExplicitUserRequest,
        "typed prompt lifecycle task",
    )
    .with_profile("typed-profile")
    .with_worker("codex")
    .with_stage("implementation")
    .with_issue_ref("#1817")
    .with_pr_ref("#1817")
    .with_flow_id("missing-flow")
    .with_project("typed-project");

    let assignment = tachi_params::ResolvedStaffAssignment::new(
        "typed-assignment",
        tachi_params::TachiDispatchReason::ExplicitUserRequest,
        "codex",
        "codex",
    )
    .with_profile("typed-profile")
    .with_model("gpt-5.6-terra")
    .with_execution_level(tachi_params::ExecutionLevel::L1)
    .with_evidence_required(vec!["focused test".to_string()])
    .with_fallback_chain(vec!["typed-fallback".to_string()])
    .with_route_explanation(vec!["typed assignment route".to_string()])
    .with_identity_receipt(json!({"planned": "typed"}));

    let grant = tachi_params::ExecutionGrant::new("typed-grant")
        .with_mcp_access(mcp_access(&["launch-mcp"], true))
        .with_allowed_tools(vec!["Read".to_string()])
        .with_timeout_secs(9);

    let identity = tachi_dispatch::identity::DispatchIdentityReceipt::planned(
        tachi_dispatch::identity::DispatchIdentityRequest {
            profile: Some("typed-profile".to_string()),
            model: Some("gpt-5.6-terra".to_string()),
            agent: Some("codex".to_string()),
            harness: Some("cli".to_string()),
        },
        tachi_dispatch::identity::DispatchIdentityEffective {
            profile: Some("typed-profile".to_string()),
            model: Some("gpt-5.6-terra".to_string()),
            backend: "codex".to_string(),
            harness: "cli".to_string(),
            model_lineage_id: "openai/gpt-5.6-terra".to_string(),
            concrete_model_release: "gpt-5.6-terra".to_string(),
            provider_model: "gpt-5.6-terra".to_string(),
            provider_model_version: "test".to_string(),
            role: "implementer".to_string(),
            seat: "typed-seat".to_string(),
            transport: "cli".to_string(),
            adapter_version: "test".to_string(),
            carrier_version: "test".to_string(),
        },
        "typed fixture".to_string(),
        false,
    );
    let profile = crate::dispatch_profile::ResolvedDispatchProfile {
        selected_profile: Some("typed-profile".to_string()),
        agent: "codex".to_string(),
        role: Some("implementer".to_string()),
        tool_profile: Some("typed-tool-profile".to_string()),
        auto_capability_bundle: true,
        mcp_access: mcp_access(&["profile-mcp"], false),
        evidence_required: vec!["profile evidence".to_string()],
        fallback_chain: vec!["profile fallback".to_string()],
        credential_profiles: Vec::new(),
        route_explanation: vec!["profile route".to_string()],
        host_adapter: None,
        profile_card: Some(json!({"card": "typed-profile"})),
        identity_receipt: identity,
    };

    (
        request,
        assignment,
        grant,
        profile,
        vec!["typed-skill".to_string()],
    )
}

pub(super) async fn typed_prompt(
    server: &MemoryServer,
    request: &tachi_params::StaffAssignmentRequest,
    assignment: &tachi_params::ResolvedStaffAssignment,
    grant: &tachi_params::ExecutionGrant,
    profile: &crate::dispatch_profile::ResolvedDispatchProfile,
    effective_skills: &[String],
) -> crate::dispatch_ops::prompt::PromptAssembly {
    crate::dispatch_ops::assemble_resolved_prompt_with_trace(
        server,
        request,
        assignment,
        grant,
        profile,
        effective_skills,
        None,
        Some(profile.auto_capability_bundle),
        Some("typed context query"),
        true,
    )
    .await
}

async fn typed_prompt_from_pre_projection_snapshot(
    server: &MemoryServer,
    params: &crate::tool_params::TachiDispatchParams,
) -> crate::dispatch_ops::prompt::PromptAssembly {
    let request = tachi_params::StaffAssignmentRequest::from_dispatch_params(params);
    let mut resolved_params = params.clone();
    let raw_profile = resolved_params.profile.clone();
    let profile = crate::dispatch_profile::resolve_and_apply_dispatch_profile_for_server(
        server,
        &mut resolved_params,
    )
    .or_else(|_| {
        resolved_params.profile = None;
        crate::dispatch_profile::resolve_and_apply_dispatch_profile_for_server(
            server,
            &mut resolved_params,
        )
        .map(|mut resolved| {
            resolved.selected_profile = raw_profile;
            resolved
        })
    })
    .expect("pre-projection snapshot resolves");
    let backend = params
        .agent
        .clone()
        .unwrap_or_else(|| profile.agent.clone());
    let mut assignment = tachi_params::ResolvedStaffAssignment::new(
        "test-assignment",
        params.staffing_reason,
        backend.clone(),
        backend,
    );
    if let Some(profile_name) = params.profile.clone() {
        assignment = assignment.with_profile(profile_name);
    }
    if let Some(model) = params.model.clone() {
        assignment = assignment.with_model(model);
    }
    let mut grant = tachi_params::ExecutionGrant::from_dispatch_params(params, "test-grant");
    if let (Some(inject_tachi_mcp), Some(access)) =
        (params.inject_tachi_mcp, grant.mcp_access.as_mut())
    {
        access.inject_tachi_mcp = Some(inject_tachi_mcp);
    }
    let (skills, stage_instruction) = resolve_assignment_skills(&request, &params.skills);
    crate::dispatch_ops::assemble_resolved_prompt_with_trace(
        server,
        &request,
        &assignment,
        &grant,
        &profile,
        &skills,
        stage_instruction.as_deref(),
        assignment
            .selected_profile
            .as_ref()
            .map(|_| profile.auto_capability_bundle)
            .or(params.auto_capability_bundle),
        params.context_query.as_deref(),
        params.inject_card != Some(false),
    )
    .await
}

#[tokio::test]
async fn p2_unpoisoned_legacy_adapter_matches_typed_snapshot_then_diverges() {
    let server = crate::tests::make_server();
    let mut legacy = test_dispatch_params(Some("custom"), "pre-projection parity task");
    legacy.stage = Some("implementation".to_string());
    legacy.project = Some("parity-project".to_string());
    legacy.issue_ref = Some("#1817".to_string());
    legacy.pr_ref = Some("#1817".to_string());
    legacy.flow_id = Some("parity-flow".to_string());
    legacy.auto_capability_bundle = None;
    let snapshot = legacy.clone();

    let legacy_baseline = crate::dispatch_ops::assemble_prompt_with_trace(&server, &legacy).await;
    let typed_baseline = typed_prompt_from_pre_projection_snapshot(&server, &snapshot).await;
    assert_eq!(legacy_baseline.prompt, typed_baseline.prompt);
    assert_eq!(
        legacy_baseline.capability_bundle,
        typed_baseline.capability_bundle
    );
    assert_eq!(
        legacy_baseline.feedback_rules,
        typed_baseline.feedback_rules
    );

    // Only the legacy projection changes after the typed contexts have frozen.
    legacy.task = "legacy-only poisoned task".to_string();
    let legacy_after = crate::dispatch_ops::assemble_prompt_with_trace(&server, &legacy).await;
    let typed_after = typed_prompt_from_pre_projection_snapshot(&server, &snapshot).await;
    assert_ne!(legacy_baseline.prompt, legacy_after.prompt);
    assert!(legacy_after.prompt.contains("legacy-only poisoned task"));
    assert_eq!(typed_baseline.prompt, typed_after.prompt);
    assert_eq!(
        typed_baseline.capability_bundle,
        typed_after.capability_bundle
    );
    assert_eq!(typed_baseline.feedback_rules, typed_after.feedback_rules);
}

#[tokio::test]
async fn p2_typed_prompt_contexts_ignore_poisoned_legacy_projection() {
    let server = crate::tests::make_server();
    let (request, assignment, grant, profile, skills) = typed_context();
    let baseline = typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;

    // Contexts are fully typed and frozen before this deliberately poisoned
    // P1 carrier is created. New P2 coverage never calls the cfg(test) prompt
    // adapters, so a future legacy read cannot silently affect this output.
    let mut poisoned_legacy = test_dispatch_params(Some("custom"), "poisoned legacy task");
    poisoned_legacy.profile = Some("poisoned-profile".to_string());
    poisoned_legacy.model = Some("poisoned-model".to_string());
    poisoned_legacy.stage = Some("poisoned-stage".to_string());
    poisoned_legacy.allowed_mcp_servers = vec!["poisoned-mcp".to_string()];
    poisoned_legacy.verbose = Some(true);
    poisoned_legacy.inject_card = Some(false);
    let after_poison =
        typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;

    let legacy_after =
        crate::dispatch_ops::assemble_prompt_with_trace(&server, &poisoned_legacy).await;
    assert!(legacy_after.prompt.contains("poisoned legacy task"));
    assert_ne!(baseline.prompt, legacy_after.prompt);

    assert_eq!(baseline.prompt, after_poison.prompt);
    assert_eq!(baseline.capability_bundle, after_poison.capability_bundle);
    assert_eq!(baseline.feedback_rules, after_poison.feedback_rules);
    assert!(baseline.prompt.contains("typed prompt lifecycle task"));
    assert!(baseline.prompt.contains("launch-mcp"));
    assert!(baseline.prompt.contains("tachi_task(action=\"complete\")"));

    let mut semantic_request = request.clone();
    semantic_request.task = "semantic request mutant".to_string();
    semantic_request.stage = Some("review".to_string());
    let semantic_prompt = typed_prompt(
        &server,
        &semantic_request,
        &assignment,
        &grant,
        &profile,
        &skills,
    )
    .await;
    assert!(semantic_prompt.prompt.contains("semantic request mutant"));
    assert_ne!(baseline.prompt, semantic_prompt.prompt);

    let mut assignment_mutant = assignment.clone();
    assignment_mutant.selected_backend = "assignment-mutant-backend".to_string();
    assignment_mutant.selected_profile = Some("assignment-mutant-profile".to_string());
    assignment_mutant.selected_model = Some("assignment-mutant-model".to_string());
    let assignment_prompt = typed_prompt(
        &server,
        &request,
        &assignment_mutant,
        &grant,
        &profile,
        &skills,
    )
    .await;
    assert!(assignment_prompt
        .prompt
        .contains("assignment-mutant-backend"));
    assert!(assignment_prompt
        .prompt
        .contains("assignment-mutant-profile"));
    assert_ne!(baseline.prompt, assignment_prompt.prompt);

    let mut grant_mutant = grant.clone();
    grant_mutant.mcp_access = Some(mcp_access(&["mutant-launch-mcp"], false));
    let grant_prompt = typed_prompt(
        &server,
        &request,
        &assignment,
        &grant_mutant,
        &profile,
        &skills,
    )
    .await;
    assert!(grant_prompt.prompt.contains("mutant-launch-mcp"));
    assert!(grant_prompt
        .prompt
        .contains("not available in this worker lane"));

    let mut private_profile = profile.clone();
    private_profile.selected_profile = Some("ignored-private-profile-alias".to_string());
    private_profile.tool_profile = Some("private-mutant-tool-profile".to_string());
    private_profile.mcp_access = mcp_access(&["private-mutant-mcp"], false);
    private_profile.auto_capability_bundle = false;
    let private_prompt = typed_prompt(
        &server,
        &request,
        &assignment,
        &grant,
        &private_profile,
        &skills,
    )
    .await;
    assert!(private_prompt
        .prompt
        .contains("private-mutant-tool-profile"));
    assert!(private_prompt.prompt.contains("private-mutant-mcp"));
    assert!(!private_prompt
        .prompt
        .contains("ignored-private-profile-alias"));
    assert_ne!(baseline.prompt, private_prompt.prompt);
}

#[tokio::test]
async fn p2_admitted_empty_skills_and_raw_bundle_ingress_stay_distinct() {
    let server = crate::tests::make_server();
    let (request, assignment, grant, mut profile, skills) = typed_context();
    profile.auto_capability_bundle = false;

    // This models the authority compiler admitting none of the explicit or
    // default candidates. Prompt assembly must consume that empty mount as-is,
    // rather than resolving the original defaults a second time.
    let empty_mount = crate::dispatch_ops::assemble_resolved_prompt_with_trace(
        &server,
        &request,
        &assignment,
        &grant,
        &profile,
        &[],
        Some("exact pre-resolved stage instruction"),
        None,
        Some("typed context query"),
        true,
    )
    .await;
    assert!(!empty_mount.prompt.contains(&skills[0]));
    assert!(empty_mount
        .prompt
        .contains("exact pre-resolved stage instruction"));
    assert_eq!(empty_mount.capability_bundle["source"], json!("unset"));
    assert_eq!(empty_mount.capability_bundle["requested_raw"], Value::Null);
    assert_eq!(empty_mount.capability_bundle["requested"], json!(false));

    // The typed carrier records whether it received an effective capability
    // decision without serializing a second raw diagnostic projection.
    let explicit_false = crate::dispatch_ops::assemble_resolved_prompt_with_trace(
        &server,
        &request,
        &assignment,
        &grant,
        &profile,
        &[],
        None,
        Some(false),
        Some("typed context query"),
        true,
    )
    .await;
    assert_eq!(explicit_false.capability_bundle["source"], json!("params"));
    assert_eq!(explicit_false.capability_bundle["requested"], json!(false));
}

#[tokio::test]
async fn p2_typed_artifacts_keep_receipt_first_and_flow_failure_observable() {
    let server = crate::tests::make_server();
    let (request, assignment, grant, profile, skills) = typed_context();
    let assembly = typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;
    let workspace = tempfile::tempdir().expect("typed lifecycle workspace");
    let trajectory = workspace.path().join("trajectory.jsonl");
    append_trajectory_event(
        &trajectory,
        json!({"event": "dispatch_received", "dispatch_id": "typed-dispatch", "timestamp": "fixed"}),
    );
    let artifacts = write_dispatch_artifacts(DispatchArtifactInputs {
        workspace_dir: workspace.path(),
        dispatch_id: "typed-dispatch",
        request: &request,
        assignment: &assignment,
        grant: &grant,
        profile: &profile,
        base_prompt: &assembly.prompt,
        prompt_assembly: &assembly,
        effective_skills_for_files: &skills,
        v2: false,
    })
    .await
    .expect("typed artifacts write");

    assert_eq!(
        std::fs::read_to_string(&artifacts.prompt_md_path).expect("prompt bytes"),
        assembly.prompt
    );
    assert_eq!(
        std::fs::read_to_string(&artifacts.plan_path).expect("plan bytes"),
        assembly.prompt
    );
    let context = std::fs::read_to_string(&artifacts.context_md_path).expect("context bytes");
    assert!(context.contains("Agent: codex"));
    assert!(context.contains("Dispatch profile: typed-profile"));
    assert!(context.contains("typed prompt lifecycle task"));
    assert!(context.contains("missing-flow"));
    let events = std::fs::read_to_string(&artifacts.trajectory_path).expect("trajectory");
    let events = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("trajectory JSON"))
        .collect::<Vec<_>>();
    assert_eq!(events[0]["event"], "dispatch_received");
    assert_eq!(events[1]["event"], "dispatch_started");

    init_kanban_and_flow(FlowSetupInputs {
        server: &server,
        dispatch_id: "typed-dispatch",
        request: &request,
        assignment: &assignment,
        grant: &grant,
        resolved_profile: &profile,
        plan_path: &artifacts.plan_path,
        workspace_dir: workspace.path(),
        prompt_md_path: &artifacts.prompt_md_path,
        context_md_path: &artifacts.context_md_path,
        trajectory_path: &artifacts.trajectory_path,
        capability_bundle_card: &artifacts.capability_bundle_card,
        capability_bundle_file: &artifacts.capability_bundle_file,
    })
    .await
    .expect("typed kanban initialization");
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, "typed-dispatch")
            .await
            .as_deref(),
        Some("TASK_STATE_WORKING")
    );
    let flow_trace = std::fs::read_to_string(&artifacts.trajectory_path).expect("flow trajectory");
    assert!(flow_trace.contains("flow_dispatch_marker_failed"));
}

#[test]
fn p2_typed_response_keeps_profile_mcp_and_slim_verbose_shape() {
    let (request, assignment, _grant, profile, _skills) = typed_context();
    let authority = json!({"authority": "typed"});
    let reports = Vec::new();
    let empty = Value::Null;
    let no_server_url = None;
    let no_backend_metadata = None;
    let profile_payload = json!({"profile": "typed-profile"});
    let response = |verbose| {
        build_dispatch_response(DispatchResponseInputs {
            dispatch_id: "typed-dispatch",
            assignment: &assignment,
            profile_payload: &profile_payload,
            resolved_profile: &profile,
            authority: &authority,
            credential_reports_json: &reports,
            capability_bundle_card: &empty,
            capability_bundle_file: "bundle.json",
            feedback_rules_trace: &empty,
            harness_transport: "cli",
            harness_server_url: &no_server_url,
            execution_backend_name: None,
            execution_backend_metadata: &no_backend_metadata,
            acpx_enabled: false,
            native_acp_enabled: false,
            v2: false,
            plan_duration_ms: None,
            request: &request,
            verbose,
            plan_path: std::path::Path::new("plan.md"),
            prompt_md_path: std::path::Path::new("prompt.md"),
            context_md_path: std::path::Path::new("context.md"),
            trajectory_path: std::path::Path::new("trajectory.jsonl"),
            workspace_dir: std::path::Path::new("run"),
        })
        .expect("typed response")
    };
    let slim: Value = serde_json::from_str(&response(false)).expect("slim response JSON");
    let verbose: Value = serde_json::from_str(&response(true)).expect("verbose response JSON");
    assert_eq!(
        slim["tool_access"]["allowed_mcp_servers"],
        json!(["profile-mcp"])
    );
    assert!(slim.get("profile").is_none());
    assert!(verbose["profile"].is_object());
    assert!(verbose["identity_receipt"].is_object());
    assert_eq!(slim["agent"], "codex");
    assert_eq!(slim["selected_profile"], "typed-profile");
    assert_eq!(slim["route_explanation"], json!(["typed assignment route"]));
    assert_eq!(slim["fallback_chain"], json!(["typed-fallback"]));
    assert_eq!(verbose["identity_receipt"], json!({"planned": "typed"}));
    assert_eq!(slim["issue_ref"], "#1817");
    assert_eq!(slim["flow_id"], "missing-flow");
}

fn response_for(
    request: &tachi_params::StaffAssignmentRequest,
    assignment: &tachi_params::ResolvedStaffAssignment,
    profile: &crate::dispatch_profile::ResolvedDispatchProfile,
    verbose: bool,
) -> String {
    let authority = json!({"authority": "typed"});
    let reports = Vec::new();
    let empty = Value::Null;
    let no_server_url = None;
    let no_backend_metadata = None;
    let profile_payload = json!({"profile": "typed-profile"});
    build_dispatch_response(DispatchResponseInputs {
        dispatch_id: "typed-dispatch",
        assignment,
        profile_payload: &profile_payload,
        resolved_profile: profile,
        authority: &authority,
        credential_reports_json: &reports,
        capability_bundle_card: &empty,
        capability_bundle_file: "bundle.json",
        feedback_rules_trace: &empty,
        harness_transport: "cli",
        harness_server_url: &no_server_url,
        execution_backend_name: None,
        execution_backend_metadata: &no_backend_metadata,
        acpx_enabled: false,
        native_acp_enabled: false,
        v2: false,
        plan_duration_ms: None,
        request,
        verbose,
        plan_path: std::path::Path::new("plan.md"),
        prompt_md_path: std::path::Path::new("prompt.md"),
        context_md_path: std::path::Path::new("context.md"),
        trajectory_path: std::path::Path::new("trajectory.jsonl"),
        workspace_dir: std::path::Path::new("run"),
    })
    .expect("typed response")
}

#[test]
fn p2_response_slim_and_verbose_serialization_are_exact_goldens() {
    let (request, assignment, _grant, profile, _skills) = typed_context();
    let slim = response_for(&request, &assignment, &profile, false);
    let verbose = response_for(&request, &assignment, &profile, true);

    assert_eq!(
        slim,
        r##"{"acp_native":null,"acpx":null,"agent":"codex","authority":{"authority":"typed"},"auto_capability_bundle":true,"capability_bundle":null,"capability_bundle_file":"bundle.json","context_file":"context.md","credentials":[],"dispatch_id":"typed-dispatch","duration_ms_plan":null,"execution_backend":null,"fallback_chain":["typed-fallback"],"feedback_rules":null,"flow_id":"missing-flow","harness_server_url":null,"harness_transport":"cli","host_adapter":null,"issue_ref":"#1817","message":"Task dispatched to background. You are unblocked. Use tachi_task(action='board') to check status.","plan_file":"plan.md","plan_review_status":"n/a","pr_ref":"#1817","prompt_file":"prompt.md","route_explanation":["typed assignment route"],"run_dir":"run","selected_profile":"typed-profile","state":"TASK_STATE_WORKING","suggested_complete_command":{"arguments":{"action":"complete","agent":"codex","diff_present":null,"dispatch_id":"typed-dispatch","evidence_refs":[],"flow_id":"missing-flow","issue_ref":"#1817","outcome":"success|failure|partial|aborted","pr_ref":"#1817","profile":"typed-profile","task":"typed prompt lifecycle task","tests_run":[]},"tool":"tachi_task"},"task":{"id":"typed-dispatch","status":{"state":"TASK_STATE_WORKING"}},"tool_access":{"allowed_facades":["typed-facade"],"allowed_mcp_servers":["profile-mcp"],"fallback":"typed-fallback","github_read":true,"inject_hub_mcps":false,"inject_tachi_mcp":false,"issue_refs":["#1817"],"pr_refs":["#1817"],"write_actions":false},"trajectory_file":"trajectory.jsonl","v2":false,"verbose":false}"##
    );
    assert_eq!(
        verbose,
        r##"{"acp_native":null,"acpx":null,"agent":"codex","authority":{"authority":"typed"},"auto_capability_bundle":true,"capability_bundle":null,"capability_bundle_file":"bundle.json","context_file":"context.md","credentials":[],"dispatch_id":"typed-dispatch","dispatch_profile":{"card":"typed-profile"},"duration_ms_plan":null,"execution_backend":null,"fallback_chain":["typed-fallback"],"feedback_rules":null,"flow_id":"missing-flow","harness_server_url":null,"harness_transport":"cli","host_adapter":null,"identity_receipt":{"planned":"typed"},"issue_ref":"#1817","message":"Task dispatched to background. You are unblocked. Use tachi_task(action='board') to check status.","plan_file":"plan.md","plan_review_status":"n/a","pr_ref":"#1817","profile":{"profile":"typed-profile"},"prompt_file":"prompt.md","route_explanation":["typed assignment route"],"run_dir":"run","selected_profile":"typed-profile","state":"TASK_STATE_WORKING","suggested_complete_command":{"arguments":{"action":"complete","agent":"codex","diff_present":null,"dispatch_id":"typed-dispatch","evidence_refs":[],"flow_id":"missing-flow","issue_ref":"#1817","outcome":"success|failure|partial|aborted","pr_ref":"#1817","profile":"typed-profile","task":"typed prompt lifecycle task","tests_run":[]},"tool":"tachi_task"},"task":{"id":"typed-dispatch","status":{"state":"TASK_STATE_WORKING"}},"tool_access":{"allowed_facades":["typed-facade"],"allowed_mcp_servers":["profile-mcp"],"fallback":"typed-fallback","github_read":true,"inject_hub_mcps":false,"inject_tachi_mcp":false,"issue_refs":["#1817"],"pr_refs":["#1817"],"write_actions":false},"trajectory_file":"trajectory.jsonl","v2":false,"verbose":true}"##
    );
}

#[tokio::test]
async fn p2_request_and_assignment_outputs_are_one_owner_discriminators() {
    let server = crate::tests::make_server();
    let (request, assignment, grant, profile, skills) = typed_context();

    struct RequestCase {
        name: &'static str,
        mutate: fn(&mut tachi_params::StaffAssignmentRequest),
        pointer: &'static str,
        expected: &'static str,
        sibling: &'static str,
    }
    let request_cases = [
        RequestCase {
            name: "task",
            mutate: |v| v.task = "request-task-only".into(),
            pointer: "/suggested_complete_command/arguments/task",
            expected: "request-task-only",
            sibling: "typed prompt lifecycle task",
        },
        RequestCase {
            name: "issue",
            mutate: |v| v.issue_ref = Some("#request-issue-only".into()),
            pointer: "/issue_ref",
            expected: "#request-issue-only",
            sibling: "#1817",
        },
        RequestCase {
            name: "pr",
            mutate: |v| v.pr_ref = Some("#request-pr-only".into()),
            pointer: "/pr_ref",
            expected: "#request-pr-only",
            sibling: "#1817",
        },
        RequestCase {
            name: "flow",
            mutate: |v| v.flow_id = Some("request-flow-only".into()),
            pointer: "/flow_id",
            expected: "request-flow-only",
            sibling: "missing-flow",
        },
    ];
    for case in request_cases {
        let mut mutant = request.clone();
        (case.mutate)(&mut mutant);
        let response: Value =
            serde_json::from_str(&response_for(&mutant, &assignment, &profile, false))
                .expect(case.name);
        assert_eq!(
            response.pointer(case.pointer),
            Some(&json!(case.expected)),
            "{} must own {}",
            case.name,
            case.pointer
        );
        assert_ne!(
            response.pointer(case.pointer),
            Some(&json!(case.sibling)),
            "{} must reject its baseline sibling",
            case.name
        );
    }

    let baseline_prompt =
        typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;
    let mut stage = request.clone();
    stage.stage = Some("request-stage-only".into());
    let stage_prompt = typed_prompt(&server, &stage, &assignment, &grant, &profile, &skills).await;
    assert_ne!(stage_prompt.prompt, baseline_prompt.prompt);
    assert!(stage_prompt.prompt.contains("- stage: request-stage-only"));
    assert!(!stage_prompt.prompt.contains("- stage: implementation"));

    struct AssignmentCase {
        name: &'static str,
        mutate: fn(&mut tachi_params::ResolvedStaffAssignment),
        pointer: &'static str,
        expected: Value,
        sibling: Value,
    }
    let assignment_cases = [
        AssignmentCase {
            name: "backend",
            mutate: |v| v.selected_backend = "assignment-backend-only".into(),
            pointer: "/agent",
            expected: json!("assignment-backend-only"),
            sibling: json!("codex"),
        },
        AssignmentCase {
            name: "profile",
            mutate: |v| v.selected_profile = Some("assignment-profile-only".into()),
            pointer: "/selected_profile",
            expected: json!("assignment-profile-only"),
            sibling: json!("typed-profile"),
        },
        AssignmentCase {
            name: "route",
            mutate: |v| v.route_explanation = vec!["assignment-route-only".into()],
            pointer: "/route_explanation",
            expected: json!(["assignment-route-only"]),
            sibling: json!(["typed assignment route"]),
        },
        AssignmentCase {
            name: "fallback",
            mutate: |v| v.fallback_chain = vec!["assignment-fallback-only".into()],
            pointer: "/fallback_chain",
            expected: json!(["assignment-fallback-only"]),
            sibling: json!(["typed-fallback"]),
        },
        AssignmentCase {
            name: "identity",
            mutate: |v| v.identity_receipt = json!({"identity": "assignment-only"}),
            pointer: "/identity_receipt",
            expected: json!({"identity": "assignment-only"}),
            sibling: json!({"planned": "typed"}),
        },
        AssignmentCase {
            name: "host",
            mutate: |v| v.host_adapter = Some("assignment-host-only".into()),
            pointer: "/host_adapter",
            expected: json!("assignment-host-only"),
            sibling: Value::Null,
        },
    ];
    for case in assignment_cases {
        let mut mutant = assignment.clone();
        (case.mutate)(&mut mutant);
        let response: Value = serde_json::from_str(&response_for(
            &request,
            &mutant,
            &profile,
            case.name == "identity",
        ))
        .expect(case.name);
        assert_eq!(
            response.pointer(case.pointer),
            Some(&case.expected),
            "{} must own {}",
            case.name,
            case.pointer
        );
        assert_ne!(
            response.pointer(case.pointer),
            Some(&case.sibling),
            "{} must reject its baseline sibling",
            case.name
        );
    }
}

#[tokio::test]
async fn p2_grant_and_private_profile_prompt_matrix_is_one_owner() {
    let server = crate::tests::make_server();
    let (request, assignment, grant, profile, skills) = typed_context();
    let baseline = typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;

    let mut launch_mcp = grant.clone();
    launch_mcp.mcp_access = Some(mcp_access(&["grant-launch-only"], true));
    let launch = typed_prompt(
        &server,
        &request,
        &assignment,
        &launch_mcp,
        &profile,
        &skills,
    )
    .await;
    assert_ne!(launch.prompt, baseline.prompt);
    assert!(launch.prompt.contains("grant-launch-only"));
    assert!(!launch.prompt.contains("launch-mcp"));

    let mut filtered_profile = profile.clone();
    filtered_profile.auto_capability_bundle = false;
    let filtered = typed_prompt(
        &server,
        &request,
        &assignment,
        &grant,
        &filtered_profile,
        &[],
    )
    .await;
    assert!(!filtered.prompt.contains("typed-skill"));
    assert_eq!(filtered.capability_bundle["requested"], json!(false));
    assert_eq!(filtered.capability_bundle["source"], json!("params"));

    let mut private = profile.clone();
    private.tool_profile = Some("private-tool-only".into());
    private.mcp_access = mcp_access(&["private-profile-mcp-only"], false);
    private.profile_card = Some(json!({"card": "private-card-only"}));
    private.role = Some("private-profile-role-only".into());
    private.auto_capability_bundle = false;
    let mut request_stage_only = request.clone();
    request_stage_only.stage = Some("request-stage-only".into());
    let mut selected_model_only = assignment.clone();
    selected_model_only.selected_model = Some("selected-model-only".into());
    let private_prompt = typed_prompt(
        &server,
        &request_stage_only,
        &selected_model_only,
        &grant,
        &private,
        &skills,
    )
    .await;
    assert_ne!(private_prompt.prompt, baseline.prompt);
    assert!(private_prompt.prompt.contains("private-tool-only"));
    assert!(private_prompt.prompt.contains("private-profile-mcp-only"));
    assert!(!private_prompt.prompt.contains("typed-tool-profile"));
    assert!(
        private_prompt
            .prompt
            .contains("- role: private-profile-role-only"),
        "the role clause must come from the private resolved profile: {}",
        private_prompt.prompt
    );
    assert!(
        !private_prompt.prompt.contains("- role: request-stage-only\\n")
            && !private_prompt.prompt.contains("- role: selected-model-only"),
        "request.stage and assignment.selected_model are semantically distinct from profile.role: {}",
        private_prompt.prompt
    );
    assert_eq!(private_prompt.capability_bundle["requested"], json!(false));

    let private_response: Value =
        serde_json::from_str(&response_for(&request, &assignment, &private, true))
            .expect("private response");
    assert_eq!(
        private_response["dispatch_profile"],
        json!({"card": "private-card-only"})
    );
    assert_eq!(
        private_response["tool_access"]["allowed_mcp_servers"],
        json!(["private-profile-mcp-only"])
    );
}

fn normalize_dynamic_bytes(bytes: String, root: &str) -> String {
    bytes.replace(root, "<RUN>")
}

#[tokio::test]
async fn p2_artifact_flow_and_kanban_metadata_are_exact_after_named_normalization() {
    let server = crate::tests::make_server();
    let (request, assignment, grant, mut profile, skills) = typed_context();
    profile.auto_capability_bundle = false;
    let assembly = typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;
    let workspace = tempfile::tempdir().expect("typed lifecycle workspace");
    let trajectory = workspace.path().join("trajectory.jsonl");
    append_trajectory_event(
        &trajectory,
        json!({"event": "dispatch_received", "dispatch_id": "typed-dispatch", "timestamp": "FIXED"}),
    );
    let artifacts = write_dispatch_artifacts(DispatchArtifactInputs {
        workspace_dir: workspace.path(),
        dispatch_id: "typed-dispatch",
        request: &request,
        assignment: &assignment,
        grant: &grant,
        profile: &profile,
        base_prompt: &assembly.prompt,
        prompt_assembly: &assembly,
        effective_skills_for_files: &skills,
        v2: false,
    })
    .await
    .expect("typed artifacts write");

    let root = workspace.path().to_string_lossy();
    assert_eq!(
        std::fs::read_to_string(&artifacts.plan_path).expect("plan bytes"),
        assembly.prompt
    );
    assert_eq!(
        std::fs::read_to_string(&artifacts.prompt_md_path).expect("prompt bytes"),
        assembly.prompt
    );
    let context = normalize_dynamic_bytes(
        std::fs::read_to_string(&artifacts.context_md_path).expect("context bytes"),
        &root,
    );
    let expected_context = format!(
        "# Dispatch Context: typed-dispatch\n\nAgent: codex\n\nDispatch profile: typed-profile\n\nTool profile: typed-tool-profile\n\nFlow: missing-flow\n\nIssue: #1817\n\nPR: #1817\n\nStage: implementation\n\nV2: false\n\nSkills: [\"typed-skill\"]\n\nCapability bundle: status=disabled requested=false injected=false artifact=<RUN>/capability_bundle.json\n\n\n\n{}",
        assembly.prompt,
    );
    assert_eq!(context, expected_context);

    let bundle = normalize_dynamic_bytes(
        std::fs::read_to_string(workspace.path().join("capability_bundle.json"))
            .expect("bundle bytes"),
        &root,
    );
    assert_eq!(
        bundle,
        r#"{
  "activation_steps": [],
  "disabled": true,
  "error": null,
  "feedback_rules": {
    "count": 0,
    "rules": [],
    "status": "none"
  },
  "host": "codex",
  "host_tools": [],
  "injected": false,
  "packs": [],
  "primary_skill": null,
  "query": "typed prompt lifecycle task",
  "rationale": null,
  "reason": "auto_capability_bundle=false",
  "requested": false,
  "section": null,
  "source": "params",
  "status": "disabled",
  "supporting_capabilities": []
}"#
    );

    init_kanban_and_flow(FlowSetupInputs {
        server: &server,
        dispatch_id: "typed-dispatch",
        request: &request,
        assignment: &assignment,
        grant: &grant,
        resolved_profile: &profile,
        plan_path: &artifacts.plan_path,
        workspace_dir: workspace.path(),
        prompt_md_path: &artifacts.prompt_md_path,
        context_md_path: &artifacts.context_md_path,
        trajectory_path: &artifacts.trajectory_path,
        capability_bundle_card: &artifacts.capability_bundle_card,
        capability_bundle_file: &artifacts.capability_bundle_file,
    })
    .await
    .expect("board-first initialization");
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, "typed-dispatch")
            .await
            .as_deref(),
        Some("TASK_STATE_WORKING")
    );

    let mut events = std::fs::read_to_string(&artifacts.trajectory_path)
        .expect("trajectory")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("trajectory JSON"))
        .collect::<Vec<_>>();
    for event in &mut events {
        if event["timestamp"] != json!("FIXED") {
            event["timestamp"] = json!("<TIMESTAMP>");
        }
    }
    let events = events
        .into_iter()
        .map(|event| {
            normalize_dynamic_bytes(
                serde_json::to_string(&event).expect("event serialize"),
                &root,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        events,
        r##"{"dispatch_id":"typed-dispatch","event":"dispatch_received","timestamp":"FIXED"}
{"agent":"codex","allowed_mcp_servers":["launch-mcp"],"auto_capability_bundle":false,"capability_bundle":{"artifact_file":"<RUN>/capability_bundle.json","disabled":true,"error":null,"host":"codex","host_tools_count":0,"injected":false,"packs_count":0,"primary_skill":null,"query":"typed prompt lifecycle task","reason":"auto_capability_bundle=false","requested":false,"source":"params","status":"disabled","supporting_capabilities_count":0},"dispatch_id":"typed-dispatch","event":"dispatch_started","feedback_rules":{"count":0,"rules":[],"status":"none"},"flow_id":"missing-flow","issue_ref":"#1817","mcp_access":{"allowed_facades":["typed-facade"],"allowed_mcp_servers":["profile-mcp"],"fallback":"typed-fallback","github_read":true,"inject_hub_mcps":false,"inject_tachi_mcp":false,"issue_refs":["#1817"],"pr_refs":["#1817"],"write_actions":false},"pr_ref":"#1817","profile":"typed-profile","stage":"implementation","timestamp":"<TIMESTAMP>","tool_profile":"typed-tool-profile","v2":false}
{"dispatch_id":"typed-dispatch","error":"Invalid flow_id: 'missing-flow'. Expected a safe id starting with 'flow_' and containing only ASCII letters, numbers, '_' or '-'. Example: flow_20260609T014037Z_tachi_dispatch_ux_smoke","event":"flow_dispatch_marker_failed","flow_id":"missing-flow","timestamp":"<TIMESTAMP>"}"##
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-wide planner and run-root env fixtures
async fn p2_real_plan_stage_matrix_is_board_first_and_receipt_bound() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let prior_review = std::env::var_os("DISPATCH_V2_PLAN_REVIEW");
    let prior_timeout = std::env::var_os("DISPATCH_V2_PLAN_TIMEOUT_SECS");
    struct Case {
        name: &'static str,
        v2: bool,
        review: Option<&'static str>,
        timeout: Option<&'static str>,
        planner: Option<crate::dispatch_ops::dispatch_v2::PlanStageTestOverride>,
        expected_state: &'static str,
        expected_error: Option<&'static str>,
        early: bool,
    }
    let cases = [
        Case {
            name: "v1",
            v2: false,
            review: None,
            timeout: None,
            planner: None,
            expected_state: "TASK_STATE_WORKING",
            expected_error: None,
            early: false,
        },
        Case {
            name: "v2_auto_approved",
            v2: true,
            review: None,
            timeout: None,
            planner: Some(
                crate::dispatch_ops::dispatch_v2::PlanStageTestOverride::Success {
                    plan_md: "## Goal\nplanner success".into(),
                    duration_ms: 7,
                },
            ),
            expected_state: "TASK_STATE_WORKING",
            expected_error: None,
            early: false,
        },
        Case {
            name: "pending_review",
            v2: true,
            review: Some("true"),
            timeout: None,
            planner: Some(
                crate::dispatch_ops::dispatch_v2::PlanStageTestOverride::Success {
                    plan_md: "## Goal\npending review".into(),
                    duration_ms: 8,
                },
            ),
            expected_state: "TASK_STATE_INPUT_REQUIRED",
            expected_error: None,
            early: true,
        },
        Case {
            name: "planner_failure",
            v2: true,
            review: None,
            timeout: None,
            planner: Some(
                crate::dispatch_ops::dispatch_v2::PlanStageTestOverride::Failure(
                    "planner-failure-only".into(),
                ),
            ),
            expected_state: "TASK_STATE_FAILED",
            expected_error: Some("planner-failure-only"),
            early: false,
        },
        Case {
            name: "timeout",
            v2: true,
            review: None,
            timeout: Some("0"),
            planner: Some(crate::dispatch_ops::dispatch_v2::PlanStageTestOverride::Pending),
            expected_state: "TASK_STATE_FAILED",
            expected_error: Some("timed out after 0s"),
            early: false,
        },
    ];
    for case in cases {
        match case.review {
            Some(value) => std::env::set_var("DISPATCH_V2_PLAN_REVIEW", value),
            None => std::env::remove_var("DISPATCH_V2_PLAN_REVIEW"),
        };
        match case.timeout {
            Some(value) => std::env::set_var("DISPATCH_V2_PLAN_TIMEOUT_SECS", value),
            None => std::env::remove_var("DISPATCH_V2_PLAN_TIMEOUT_SECS"),
        };
        crate::dispatch_ops::dispatch_v2::set_plan_stage_test_override(case.planner.clone());
        let server = crate::tests::make_server();
        let (request, assignment, grant, mut profile, skills) = typed_context();
        profile.auto_capability_bundle = false;
        let assembly =
            typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;
        let workspace = tempfile::tempdir().expect(case.name);
        let artifacts = write_dispatch_artifacts(DispatchArtifactInputs {
            workspace_dir: workspace.path(),
            dispatch_id: case.name,
            request: &request,
            assignment: &assignment,
            grant: &grant,
            profile: &profile,
            base_prompt: &assembly.prompt,
            prompt_assembly: &assembly,
            effective_skills_for_files: &skills,
            v2: case.v2,
        })
        .await
        .expect(case.name);
        init_kanban_and_flow(FlowSetupInputs {
            server: &server,
            dispatch_id: case.name,
            request: &request,
            assignment: &assignment,
            grant: &grant,
            resolved_profile: &profile,
            plan_path: &artifacts.plan_path,
            workspace_dir: workspace.path(),
            prompt_md_path: &artifacts.prompt_md_path,
            context_md_path: &artifacts.context_md_path,
            trajectory_path: &artifacts.trajectory_path,
            capability_bundle_card: &artifacts.capability_bundle_card,
            capability_bundle_file: &artifacts.capability_bundle_file,
        })
        .await
        .expect(case.name);
        assert_eq!(
            crate::dispatch_ops::get_kanban_state(&server, case.name)
                .await
                .as_deref(),
            Some("TASK_STATE_WORKING"),
            "{} must seed board before planning",
            case.name
        );
        let profile_payload = json!({"profile": "typed-profile"});
        let outcome = super::super::plan_stage::run_v2_plan_stage(
            super::super::plan_stage::PlanStageInputs {
                server: &server,
                request: &request,
                dispatch_id: case.name,
                assignment: &assignment,
                resolved_profile: &profile,
                profile_payload: &profile_payload,
                base_prompt: &assembly.prompt,
                plan_path: &artifacts.plan_path,
                prompt_md_path: &artifacts.prompt_md_path,
                context_md_path: &artifacts.context_md_path,
                trajectory_path: &artifacts.trajectory_path,
                workspace_dir: workspace.path(),
                capability_bundle_card: &artifacts.capability_bundle_card,
                capability_bundle_file: &artifacts.capability_bundle_file,
                feedback_rules_trace: &artifacts.feedback_rules_trace,
                v2_decision: if case.v2 {
                    crate::dispatch_ops::dispatch_v2::V2Decision::Enabled
                } else {
                    crate::dispatch_ops::dispatch_v2::V2Decision::Disabled
                },
            },
        )
        .await;
        match case.expected_error {
            Some(expected) => match outcome {
                Err(error) => assert!(error.contains(expected), "{}", case.name),
                Ok(_) => panic!("{} unexpectedly succeeded", case.name),
            },
            None => {
                let outcome = outcome.expect(case.name);
                assert_eq!(
                    outcome.early_response.is_some(),
                    case.early,
                    "{}",
                    case.name
                );
                if case.v2 && !case.early {
                    assert!(
                        outcome.prompt.contains("## Plan (from Stage 1)"),
                        "{}",
                        case.name
                    );
                }
                if case.early {
                    let response: Value =
                        serde_json::from_str(outcome.early_response.as_deref().expect(case.name))
                            .expect(case.name);
                    assert_eq!(
                        response["task"]["status"]["state"],
                        json!("TASK_STATE_INPUT_REQUIRED")
                    );
                }
            }
        }
        assert_eq!(
            crate::dispatch_ops::get_kanban_state(&server, case.name)
                .await
                .as_deref(),
            Some(case.expected_state),
            "{}",
            case.name
        );
        let trace = std::fs::read_to_string(&artifacts.trajectory_path).expect(case.name);
        assert!(
            trace
                .lines()
                .next()
                .expect(case.name)
                .contains("dispatch_started"),
            "{} artifact receipt must predate planner",
            case.name
        );
    }
    crate::dispatch_ops::dispatch_v2::set_plan_stage_test_override(None);
    match prior_review {
        Some(value) => std::env::set_var("DISPATCH_V2_PLAN_REVIEW", value),
        None => std::env::remove_var("DISPATCH_V2_PLAN_REVIEW"),
    };
    match prior_timeout {
        Some(value) => std::env::set_var("DISPATCH_V2_PLAN_TIMEOUT_SECS", value),
        None => std::env::remove_var("DISPATCH_V2_PLAN_TIMEOUT_SECS"),
    };
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes the process-wide TACHI_RUN_ROOT fixture
async fn p2_flow_card_preserves_assignment_evidence_without_legacy_projection() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let run_root = tempfile::tempdir().expect("flow root");
    let prior = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", run_root.path());
    let server = crate::tests::make_server();
    let (mut request, mut assignment, grant, mut profile, skills) = typed_context();
    request.flow_id = Some("flow_20260822T000000Z_prompt_evidence".into());
    assignment.evidence_required = vec!["evidence-only-a".into(), "evidence-only-b".into()];
    profile.auto_capability_bundle = false;
    let assembly = typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;
    let workspace = tempfile::tempdir().expect("workspace");
    let artifacts = write_dispatch_artifacts(DispatchArtifactInputs {
        workspace_dir: workspace.path(),
        dispatch_id: "evidence-dispatch",
        request: &request,
        assignment: &assignment,
        grant: &grant,
        profile: &profile,
        base_prompt: &assembly.prompt,
        prompt_assembly: &assembly,
        effective_skills_for_files: &skills,
        v2: false,
    })
    .await
    .expect("artifacts");
    init_kanban_and_flow(FlowSetupInputs {
        server: &server,
        dispatch_id: "evidence-dispatch",
        request: &request,
        assignment: &assignment,
        grant: &grant,
        resolved_profile: &profile,
        plan_path: &artifacts.plan_path,
        workspace_dir: workspace.path(),
        prompt_md_path: &artifacts.prompt_md_path,
        context_md_path: &artifacts.context_md_path,
        trajectory_path: &artifacts.trajectory_path,
        capability_bundle_card: &artifacts.capability_bundle_card,
        capability_bundle_file: &artifacts.capability_bundle_file,
    })
    .await
    .expect("flow marker");
    let card_path =
        crate::task_lifecycle::run_dir_for_flow_id(request.flow_id.as_deref().expect("flow"))
            .expect("flow dir")
            .join("artifacts/dispatch-evidence-dispatch.json");
    let card: Value =
        serde_json::from_str(&std::fs::read_to_string(card_path).expect("card bytes"))
            .expect("card json");
    assert_eq!(
        card["evidence_required"],
        json!(["evidence-only-a", "evidence-only-b"])
    );
    if let Some(value) = prior {
        std::env::set_var("TACHI_RUN_ROOT", value);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn p2_model_and_private_role_select_the_matching_vaccination_overlay() {
    let server = crate::tests::make_server();
    for (vendor, clause) in [
        ("codex", "falsified_ci_report"),
        ("claude", "assertion_weakening"),
    ] {
        crate::signature_evidence::record_signature(
            &server,
            &crate::signature_evidence::SignatureRecord {
                vendor: vendor.into(),
                role: "implementer".into(),
                signature: clause.into(),
                severity: Some(tachi_dispatch::Severity::High),
                evidence_ref: Some(format!("{vendor}-evidence")),
                resolved: false,
                recorded_at: chrono::Utc::now(),
                identity_receipt: None,
                attribution_basis: "observed".into(),
                vendor_explicit: true,
            },
        )
        .expect("signature evidence");
    }
    let (request, mut assignment, grant, mut profile, skills) = typed_context();
    assignment.selected_backend.clear();
    profile.agent.clear();
    profile.role = Some("implementer".into());
    profile.auto_capability_bundle = false;
    assignment.selected_model = Some("gpt-5.6-terra".into());
    let codex = typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;
    assignment.selected_model = Some("claude-4".into());
    let claude = typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;
    assert!(
        codex.prompt.contains("- lane: implementer/codex"),
        "{0}",
        codex.prompt
    );
    assert!(
        codex.prompt.contains("falsified_ci_report"),
        "{0}",
        codex.prompt
    );
    assert!(
        !codex.prompt.contains("assertion_weakening"),
        "{0}",
        codex.prompt
    );
    assert!(
        claude.prompt.contains("- lane: implementer/claude"),
        "{0}",
        claude.prompt
    );
    assert!(
        claude.prompt.contains("assertion_weakening"),
        "{0}",
        claude.prompt
    );
    assert!(
        !claude.prompt.contains("falsified_ci_report"),
        "{0}",
        claude.prompt
    );
}

#[tokio::test]
async fn p2_project_selects_only_the_seeded_project_context() {
    let (server, _project_db) = crate::tests::make_server_with_project_fixture("typed-project");
    crate::memory_search_ops::handle_save_memory(
        &server,
        crate::tool_params::SaveMemoryParams {
            text: "typed context query project-context-only".into(),
            summary: "project-context-only".into(),
            path: "/prompt-lifecycle/project-context".into(),
            importance: 1.0,
            category: "fact".into(),
            topic: "prompt-lifecycle".into(),
            keywords: vec!["typed".into(), "context".into()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "project".into(),
            vector: None,
            id: None,
            force: true,
            auto_link: false,
            project: Some("typed-project".into()),
            project_explicit: true,
            retention_policy: None,
            domain: Some("test".into()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
        },
    )
    .await
    .expect("seed named project context");
    let (request, assignment, grant, profile, skills) = typed_context();
    let scoped = typed_prompt(&server, &request, &assignment, &grant, &profile, &skills).await;
    let mut wrong_project = request.clone();
    wrong_project.project = Some("other-project".into());
    let unscoped = typed_prompt(
        &server,
        &wrong_project,
        &assignment,
        &grant,
        &profile,
        &skills,
    )
    .await;
    assert!(
        scoped.prompt.contains("project-context-only"),
        "{}",
        scoped.prompt
    );
    assert!(
        !unscoped.prompt.contains("project-context-only"),
        "{}",
        unscoped.prompt
    );
}
