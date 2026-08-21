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

fn typed_context() -> (
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

async fn typed_prompt(
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
        None,
        Some("typed context query"),
        true,
    )
    .await
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

    // An explicit ingress value changes only the raw diagnostic projection;
    // the private profile remains the effective bundle decision.
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
    assert_eq!(
        explicit_false.capability_bundle["requested_raw"],
        json!(false)
    );
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
