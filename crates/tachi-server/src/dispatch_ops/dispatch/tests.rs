use super::*;
// Shared OpenCode OpenAPI fixture lives in `crate::test_support`; the local
// `spawn_auth_gated_opencode_doc_server` below is a distinct (auth-gated)
// variant that uses it.
use crate::test_support::{EnvRestore, OPENCODE_DOC_FIXTURE};

fn test_dispatch_params(agent: Option<&str>, task: &str) -> TachiDispatchParams {
    TachiDispatchParams {
        staffing_reason: tachi_params::TachiDispatchReason::ExplicitUserRequest,
        agent: agent.map(str::to_string),
        profile: None,
        task: task.to_string(),
        execution_level: None,
        cwd: None,
        env_id: None,
        unmanaged_cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: 5,
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
        project: None,
        stage: None,
        credential_profiles: Vec::new(),
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        tool_profile: None,
        auto_capability_bundle: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        verbose: None,
        inject_card: None,
    }
}

#[test]
fn dispatch_resolution_mints_typed_assignment_with_exact_legacy_projection() {
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(Some("custom"), "resolve typed assignment");
    params.model = Some("gpt-5.6-terra".to_string());
    params.execution_level = Some(tachi_params::ExecutionLevel::L2);

    let start = resolve_dispatch_start(
        &server,
        &mut params,
        Utc::now(),
        tachi_params::ExecutionLevel::L2,
    )
    .expect("profile-less custom dispatch resolves");

    assert_eq!(start.resolved_assignment.assignment_id, start.dispatch_id);
    assert_eq!(start.resolved_assignment.selected_worker, "custom");
    assert_eq!(
        start.resolved_assignment.selected_backend,
        params
            .agent
            .as_deref()
            .expect("canonical backend")
            .to_string()
    );
    assert_eq!(
        start.resolved_assignment.selected_model,
        params.model.clone()
    );
    assert_eq!(
        start.resolved_assignment.execution_level,
        Some(tachi_params::ExecutionLevel::L2)
    );
    assert_assignment_legacy_projection(&params, &start.resolved_assignment)
        .expect("typed assignment and temporary legacy projection agree");
    let mut profile_mutant = start.resolved_assignment.clone();
    profile_mutant.selected_profile = Some("mutant".to_string());
    assert!(
        assert_assignment_legacy_projection(&params, &profile_mutant).is_err(),
        "one-sided selected-profile mutation must be observable"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn raw_credential_profile_spelling_reaches_failure_trajectory() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(Some("custom"), "raw credential trajectory");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.credential_profiles = vec![" missing-profile ".to_string()];

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("missing credential profile fails after receipt creation");
    assert!(err.contains("missing-profile"), "{err}");
    let trajectory =
        std::fs::read_to_string(single_run_dir(&dispatch_runs_root()).join("trajectory.jsonl"))
            .expect("credential failure trajectory exists");
    let event: Value = trajectory
        .lines()
        .map(|line| serde_json::from_str(line).expect("trajectory event JSON"))
        .find(|event: &Value| event["event"] == "credentials_materialization_failed")
        .expect("credential failure event");
    assert_eq!(
        event["credential_profiles"],
        json!([" missing-profile "]),
        "{event}"
    );
}

#[test]
fn execution_grant_detects_a_one_sided_legacy_projection_mutant() {
    let mut params = test_dispatch_params(Some("custom"), "mint typed grant");
    params.env_id = Some("managed-env".to_string());
    params.cwd = Some("/workspace/tachi".to_string());
    params.unmanaged_cwd = Some(true);
    params.credential_profiles = vec!["dispatch-token".to_string()];
    params.allowed_tools = vec!["Read".to_string(), "Write".to_string()];
    params.permission_profile = Some("allowlist".to_string());
    params.sandbox = Some("workspace-write".to_string());
    params.max_turns = Some(7);
    params.timeout_secs = 42;

    let env_resolution = crate::exec_env_ops::EnvResolution::Unmanaged {
        cwd: "/workspace/tachi".to_string(),
    };
    let grant = mint_execution_grant(&mut params, "dispatch-grant", &env_resolution)
        .expect("typed grant exactly projects legacy authority");
    assert_grant_legacy_projection(&params, &grant, &env_resolution)
        .expect("matching grant remains compatible");

    // Deliberate one-sided mutant: the grant stays authoritative while the
    // untouched P3 legacy projection changes. The ingress guard must reject it.
    params.timeout_secs = 43;
    assert!(
        assert_grant_legacy_projection(&params, &grant, &env_resolution).is_err(),
        "a one-sided compatibility mutation must be rejected"
    );
}

#[test]
fn explicit_profile_assignment_is_authoritative_before_legacy_projection() {
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(None, "resolve an explicit profile");
    params.profile = Some("glm_51_impl".to_string());

    let start = resolve_dispatch_start(
        &server,
        &mut params,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    )
    .expect("profile alias resolves before legacy projection");

    assert_eq!(
        start.resolved_assignment.selected_profile.as_deref(),
        Some("glm_impl")
    );
    assert_eq!(
        params.profile, start.resolved_assignment.selected_profile,
        "legacy profile is only the exact typed assignment projection"
    );
    assert_eq!(
        params.agent.as_deref(),
        Some(start.resolved_assignment.selected_backend.as_str())
    );
    assert_eq!(
        start.resolved_assignment.host_adapter,
        start.resolved_profile.host_adapter
    );
    assert_eq!(
        start.resolved_assignment.evidence_required,
        start.resolved_profile.evidence_required
    );
    assert_eq!(
        start.resolved_assignment.fallback_chain,
        start.resolved_profile.fallback_chain
    );
    assert_eq!(
        start.resolved_assignment.route_explanation,
        start.resolved_profile.route_explanation
    );
    assert_ne!(start.resolved_assignment.identity_receipt, Value::Null);
}

#[test]
fn typed_ingress_equivalence_matrix_covers_missing_and_effective_values() {
    struct Case {
        name: &'static str,
        model: Option<&'static str>,
        level: tachi_params::ExecutionLevel,
        max_turns: Option<u32>,
        timeout_secs: u64,
        populated: bool,
    }

    let cases = [
        Case {
            name: "missing optional ingress values",
            model: None,
            level: tachi_params::ExecutionLevel::L1,
            max_turns: None,
            timeout_secs: 5,
            populated: false,
        },
        Case {
            name: "effective populated ingress values",
            model: Some("gpt-5.6-terra"),
            level: tachi_params::ExecutionLevel::L2,
            max_turns: Some(7),
            timeout_secs: 42,
            populated: true,
        },
        Case {
            name: "explicit zero ingress budgets",
            model: None,
            level: tachi_params::ExecutionLevel::L1,
            max_turns: Some(0),
            timeout_secs: 0,
            populated: false,
        },
    ];
    let server = crate::tests::make_server();

    for case in cases {
        let mut params = test_dispatch_params(Some("custom"), case.name);
        params.model = case.model.map(str::to_string);
        params.max_turns = case.max_turns;
        params.timeout_secs = case.timeout_secs;
        if case.populated {
            params.skills = vec!["skill:review".to_string()];
            params.allowed_tools = vec!["Read".to_string()];
            params.credential_profiles = vec!["token".to_string()];
        }

        let start =
            resolve_dispatch_start(&server, &mut params, Utc::now(), case.level).expect(case.name);
        assert_eq!(
            start.resolved_assignment.selected_worker, "custom",
            "{}",
            case.name
        );
        assert_eq!(
            start.resolved_assignment.selected_model, params.model,
            "{}",
            case.name
        );
        assert_eq!(
            start.resolved_assignment.execution_level,
            Some(case.level),
            "{}",
            case.name
        );
        assert!(
            start.resolved_assignment.recommendation_ref.is_none(),
            "semantic ingress has no recommendation source in P1: {}",
            case.name
        );

        let grant = mint_execution_grant(
            &mut params,
            format!("matrix-{}", case.timeout_secs),
            &crate::exec_env_ops::EnvResolution::Default,
        )
        .expect(case.name);
        assert_eq!(grant.max_turns, case.max_turns, "{}", case.name);
        assert_eq!(grant.timeout_secs, case.timeout_secs, "{}", case.name);
        assert_eq!(params.max_turns, case.max_turns, "{}", case.name);
        assert_eq!(params.timeout_secs, case.timeout_secs, "{}", case.name);
        assert_eq!(grant.allowed_tools, params.allowed_tools, "{}", case.name);
        assert_eq!(
            grant.credential_profiles, params.credential_profiles,
            "{}",
            case.name
        );
    }
}

#[test]
fn invalid_profile_is_refused_before_typed_assignment_or_launch() {
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(None, "invalid profile refusal");
    params.profile = Some("not-a-dispatch-profile".to_string());

    let err = match resolve_dispatch_start(
        &server,
        &mut params,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    ) {
        Ok(_) => panic!("unknown profile must be refused before typed assignment or launch"),
        Err(err) => err,
    };
    assert!(err.contains("Unknown dispatch profile"), "{err}");
}

#[test]
fn execution_grant_detects_a_populated_mcp_one_sided_mutant() {
    let mut params = test_dispatch_params(Some("custom"), "mint populated MCP grant");
    params.mcp_access = Some(tachi_params::DispatchMcpAccessParams {
        inject_tachi_mcp: Some(true),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["search".to_string()],
        allowed_mcp_servers: vec!["context7".to_string()],
        github_read: Some(true),
        write_actions: Some(false),
        issue_refs: vec!["kckylechen1/tachi#1815".to_string()],
        pr_refs: Vec::new(),
        fallback: Some("report unavailable".to_string()),
    });
    params.inject_tachi_mcp = Some(true);
    params.inject_hub_mcps = Some(false);
    params.allowed_mcp_servers = vec!["context7".to_string()];
    let env_resolution = crate::exec_env_ops::EnvResolution::Default;
    let grant = mint_execution_grant(&mut params, "mcp-grant", &env_resolution)
        .expect("typed grant projects populated MCP access");

    params.allowed_mcp_servers.push("mutant".to_string());
    assert!(
        assert_grant_legacy_projection(
            &params,
            &grant,
            &crate::exec_env_ops::EnvResolution::Default,
        )
        .is_err(),
        "a populated MCP projection must reject one-sided drift"
    );
}

#[test]
fn execution_grant_preserves_top_level_mcp_precedence_over_nested_conflicts() {
    let mut params = test_dispatch_params(Some("custom"), "canonical MCP precedence");
    params.inject_tachi_mcp = Some(true);
    params.inject_hub_mcps = Some(false);
    params.allowed_mcp_servers = vec!["top-level-server".to_string()];
    params.mcp_access = Some(tachi_params::DispatchMcpAccessParams {
        inject_tachi_mcp: Some(false),
        inject_hub_mcps: Some(true),
        allowed_facades: Vec::new(),
        allowed_mcp_servers: vec!["nested-server".to_string()],
        github_read: Some(false),
        write_actions: Some(false),
        issue_refs: Vec::new(),
        pr_refs: Vec::new(),
        fallback: None,
    });

    let grant = mint_execution_grant(
        &mut params,
        "mcp-conflict-grant",
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("grant preserves the existing top-level launch authority");
    let mcp = grant.mcp_access.clone().expect("canonical MCP access");
    assert_eq!(mcp.inject_tachi_mcp, Some(true));
    assert_eq!(mcp.inject_hub_mcps, Some(false));
    assert_eq!(
        mcp.allowed_mcp_servers,
        vec!["top-level-server".to_string()]
    );
    assert_grant_legacy_projection(
        &params,
        &grant,
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("launch-facing top-level fields and grant stay identical");
    assert_eq!(
        params
            .mcp_access
            .as_ref()
            .expect("nested profile MCP metadata remains present")
            .allowed_mcp_servers,
        vec!["nested-server".to_string()],
        "grant projection must not overwrite the nested profile MCP metadata"
    );
}

#[test]
fn profile_payload_preserves_nested_mcp_while_grant_uses_launch_authority() {
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(Some("claude"), "compose MCP authority");
    params.inject_tachi_mcp = Some(true);
    params.inject_hub_mcps = Some(false);
    params.allowed_mcp_servers = vec!["top-level-server".to_string()];
    params.mcp_access = Some(tachi_params::DispatchMcpAccessParams {
        inject_tachi_mcp: Some(false),
        inject_hub_mcps: Some(true),
        allowed_facades: Vec::new(),
        allowed_mcp_servers: vec!["nested-server".to_string()],
        github_read: Some(false),
        write_actions: Some(false),
        issue_refs: Vec::new(),
        pr_refs: Vec::new(),
        fallback: None,
    });
    let start = resolve_dispatch_start(
        &server,
        &mut params,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    )
    .expect("composed MCP profile resolves");

    assert!(start.inject_tachi, "launch input keeps top-level true");
    assert!(!start.inject_hub, "launch input keeps top-level false");
    assert_eq!(
        start.profile_payload["mcp_access"]["inject_tachi_mcp"],
        json!(false),
        "response/plan profile payload must retain the nested profile authority"
    );
    assert_eq!(
        start.profile_payload["mcp_access"]["allowed_mcp_servers"],
        json!(["nested-server"]),
        "response/plan profile payload must retain the nested profile allowlist"
    );
    assert_eq!(
        params
            .mcp_access
            .as_ref()
            .expect("legacy projection")
            .allowed_mcp_servers,
        vec!["nested-server".to_string()]
    );

    let authority = Value::Null;
    let empty_reports = Vec::new();
    let empty_value = Value::Null;
    let no_server_url = None;
    let no_backend_metadata = None;
    let response = build_dispatch_response(DispatchResponseInputs {
        dispatch_id: &start.dispatch_id,
        agent_norm: &start.agent_norm,
        profile_payload: &start.profile_payload,
        resolved_profile: &start.resolved_profile,
        authority: &authority,
        credential_reports_json: &empty_reports,
        capability_bundle_card: &empty_value,
        capability_bundle_file: "",
        feedback_rules_trace: &empty_value,
        harness_transport: "cli",
        harness_server_url: &no_server_url,
        host_adapter: &start.host_adapter,
        execution_backend_name: None,
        execution_backend_metadata: &no_backend_metadata,
        acpx_enabled: false,
        native_acp_enabled: false,
        v2: false,
        plan_duration_ms: None,
        params: &params,
        plan_path: std::path::Path::new("plan.md"),
        prompt_md_path: std::path::Path::new("prompt.md"),
        context_md_path: std::path::Path::new("context.md"),
        trajectory_path: std::path::Path::new("trajectory.jsonl"),
        workspace_dir: std::path::Path::new("run"),
    })
    .expect("composed dispatch response");
    let response: Value = serde_json::from_str(&response).expect("response JSON");
    assert_eq!(
        response["tool_access"]["allowed_mcp_servers"],
        json!(["nested-server"]),
        "accepted response must retain the nested profile allowlist"
    );
    let grant = mint_execution_grant(
        &mut params,
        "profile-launch-mcp-grant",
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("grant preserves top-level launch authority");
    assert_eq!(
        grant
            .mcp_access
            .as_ref()
            .expect("grant MCP access")
            .allowed_mcp_servers,
        vec!["top-level-server".to_string()],
        "grant must not inherit the nested response/profile allowlist"
    );
    assert_eq!(
        params
            .mcp_access
            .as_ref()
            .expect("nested profile projection")
            .allowed_mcp_servers,
        vec!["nested-server".to_string()],
        "grant projection must leave profile-facing nested metadata unchanged"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn composed_mcp_authority_reaches_real_dispatch_config_and_response() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let cwd = tempfile::tempdir().expect("dispatch cwd");
    let fake_bin = tempfile::tempdir().expect("fake claude bin");
    let release = cwd.path().join("release-fake-claude");
    let captured_config = cwd.path().join("captured-mcp.json");
    let fake_claude = fake_bin.path().join("claude");
    std::fs::write(
        &fake_claude,
        format!(
            "#!/bin/sh\nconfig=\nprev=\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"--mcp-config\" ]; then config=\"$arg\"; fi\n  prev=\"$arg\"\ndone\nif [ -n \"$config\" ]; then cp \"$config\" '{}'; fi\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf fake-claude\\n",
            captured_config.display(),
            release.display()
        ),
    )
    .expect("write fake claude");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o700))
            .expect("make fake claude executable");
    }
    let mut path_entries = vec![fake_bin.path().to_path_buf()];
    path_entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is set for dispatch test"),
    ));
    let path = std::env::join_paths(path_entries).expect("join test PATH");
    let _path = EnvRestore::set_os("PATH", &path);
    let server = crate::tests::make_server();
    let canonical_hub = crate::tests::make_mcp_capability("mcp:canonical-server", 1);
    let disallowed_hub = crate::tests::make_mcp_capability("mcp:disallowed-server", 1);
    server
        .with_global_store(|store| {
            store
                .hub_register(&canonical_hub)
                .and_then(|_| store.hub_register(&disallowed_hub))
                .map_err(|error| format!("register test Hub MCP capability: {error}"))
        })
        .expect("seed Hub MCP capabilities for generated launch config");
    let mut params = test_dispatch_params(Some("claude"), "real composed MCP dispatch");
    params.inject_tachi_mcp = Some(true);
    params.inject_hub_mcps = Some(true);
    params.allowed_mcp_servers = vec!["canonical-server".to_string()];
    params.mcp_access = Some(tachi_params::DispatchMcpAccessParams {
        inject_tachi_mcp: Some(false),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["tachi_search".to_string()],
        allowed_mcp_servers: vec!["nested-server".to_string()],
        github_read: Some(false),
        write_actions: Some(false),
        issue_refs: Vec::new(),
        pr_refs: Vec::new(),
        fallback: None,
    });
    params.verbose = Some(true);
    params.cwd = Some(cwd.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);

    let raw = handle_tachi_dispatch(&server, params)
        .await
        .expect("real dispatch reaches the accepted response");
    let response: Value = serde_json::from_str(&raw).expect("response JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
    let config_path = temp_home
        .path()
        .join(".tachi")
        .join("tmp")
        .join(format!("dispatch-{dispatch_id}-mcp.json"));
    for _ in 0..120 {
        if captured_config.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(&captured_config)
            .expect("fake launcher captured generated MCP config"),
    )
    .expect("MCP config JSON");
    let run_dir = std::path::PathBuf::from(
        response["run_dir"]
            .as_str()
            .expect("accepted response carries run directory"),
    );
    let prompt = std::fs::read_to_string(
        response["prompt_file"]
            .as_str()
            .expect("accepted response carries prompt path"),
    )
    .expect("real handler writes prompt artifact");
    let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl"))
        .expect("real handler writes trajectory artifact");
    let progress = std::fs::read_to_string(run_dir.join("progress.jsonl"))
        .expect("real handler writes progress artifact");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
    let kanban = server
        .with_global_store(|store| {
            store
                .list_by_path(&format!("/kanban/tasks/{dispatch_id}"), 1, false)
                .map_err(|error| format!("read real-handler kanban metadata: {error}"))
        })
        .expect("real handler seeds kanban entry")
        .into_iter()
        .next()
        .expect("one real-handler kanban entry");

    assert!(
        config["mcpServers"].get("tachi").is_some(),
        "generated launch config must use top-level inject_tachi=true: {config}"
    );
    assert!(
        config["mcpServers"].get("canonical-server").is_some(),
        "generated launch config must receive the canonical Hub allowlist: {config}"
    );
    assert!(
        config["mcpServers"].get("nested-server").is_none()
            && config["mcpServers"].get("disallowed-server").is_none(),
        "generated launch config must exclude stale and disallowed Hub MCPs: {config}"
    );
    assert_eq!(
        config["mcpServers"]
            .as_object()
            .expect("MCP server object")
            .len(),
        2,
        "generated launch config must contain exactly Tachi and the canonical Hub server: {config}"
    );
    assert_eq!(
        response["tool_access"]["inject_tachi_mcp"],
        json!(false),
        "accepted response must retain the nested profile authority: {response}"
    );
    assert_eq!(
        response["tool_access"]["allowed_mcp_servers"],
        json!(["nested-server"]),
        "accepted response must retain the nested profile allowlist: {response}"
    );
    assert_eq!(
        response["profile"]["mcp_access"]["inject_tachi_mcp"],
        json!(false),
        "verbose planning profile must retain the nested authority: {response}"
    );
    assert_eq!(
        response["profile"]["mcp_access"]["allowed_mcp_servers"],
        json!(["nested-server"]),
        "verbose planning profile must retain the nested allowlist: {response}"
    );
    assert!(
        prompt.contains("\"allowed_mcp_servers\":[\"nested-server\"]")
            && prompt.contains("allowed_mcp_servers: canonical-server"),
        "prompt must retain nested profile metadata alongside the independent launch allowlist: {prompt}"
    );
    for (name, stream) in [("trajectory", &trajectory), ("progress", &progress)] {
        let event: Value = stream
            .lines()
            .map(|line| serde_json::from_str(line).expect("event JSON"))
            .find(|event: &Value| event["event"] == "dispatch_started")
            .unwrap_or_else(|| panic!("{name} contains dispatch_started"));
        assert_eq!(
            event["mcp_access"]["allowed_mcp_servers"],
            json!(["nested-server"]),
            "{name} must retain nested profile MCP metadata: {event}"
        );
    }
    assert_eq!(
        kanban.metadata["mcp_access"]["allowed_mcp_servers"],
        json!(["nested-server"]),
        "kanban must retain nested profile MCP metadata: {}",
        kanban.metadata
    );
    std::fs::write(&release, b"release").expect("release fake claude");
    for _ in 0..120 {
        if !config_path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("fake claude completion must clean the generated MCP config");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn whitespace_profile_preserves_legacy_response_and_artifact_spelling() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(Some("custom"), "preserve whitespace profile spelling");
    params.profile = Some("   ".to_string());
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let raw = handle_tachi_dispatch(&server, params)
        .await
        .expect("whitespace profile dispatch is accepted");
    let response: Value = serde_json::from_str(&raw).expect("response JSON");
    assert!(
        response["selected_profile"].is_null(),
        "typed assignment may canonicalize whitespace-only profile to None: {response}"
    );
    assert_eq!(
        response["suggested_complete_command"]["arguments"]["profile"],
        json!("   "),
        "completion payload must retain the base legacy profile spelling: {response}"
    );
    let run_dir = std::path::PathBuf::from(
        response["run_dir"]
            .as_str()
            .expect("response carries run directory"),
    );
    let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl"))
        .expect("trajectory artifact exists");
    let started: Value = trajectory
        .lines()
        .map(|line| serde_json::from_str(line).expect("trajectory event JSON"))
        .find(|event: &Value| event["event"] == "dispatch_started")
        .expect("trajectory contains dispatch_started");
    assert_eq!(started["profile"], json!("   "), "{started}");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
    let kanban = server
        .with_global_store(|store| {
            store
                .list_by_path(&format!("/kanban/tasks/{dispatch_id}"), 1, false)
                .map_err(|error| format!("read whitespace-profile kanban metadata: {error}"))
        })
        .expect("whitespace profile kanban entry")
        .into_iter()
        .next()
        .expect("one whitespace-profile kanban entry");
    assert_eq!(
        kanban.metadata["profile"],
        json!("   "),
        "{}",
        kanban.metadata
    );
}

#[test]
fn profile_context_and_grant_canonicalize_credential_profiles() {
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(Some("custom"), "canonical credentials");
    params.credential_profiles = vec![
        " primary ".to_string(),
        "".to_string(),
        "primary".to_string(),
        " secondary ".to_string(),
    ];
    let start = resolve_dispatch_start(
        &server,
        &mut params,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    )
    .expect("credential profile context resolves");
    let expected = vec!["primary".to_string(), "secondary".to_string()];
    assert_eq!(start.resolved_profile.credential_profiles, expected);
    assert_eq!(
        params.credential_profiles,
        vec![
            " primary ".to_string(),
            "".to_string(),
            "primary".to_string(),
            " secondary ".to_string(),
        ],
        "legacy ingress retains raw credential selectors"
    );

    let grant = mint_execution_grant(
        &mut params,
        "credential-grant",
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("grant uses canonical credential profiles");
    assert_eq!(grant.credential_profiles, expected);
    assert_grant_legacy_projection(
        &params,
        &grant,
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("materializer-facing legacy projection equals the grant");
    let mut dropped_mcp = grant.clone();
    dropped_mcp.mcp_access = None;
    assert!(
        assert_grant_legacy_projection(
            &params,
            &dropped_mcp,
            &crate::exec_env_ops::EnvResolution::Default,
        )
        .is_err(),
        "dropping ordinary resolved nested MCP metadata must be observable"
    );
}

#[test]
fn execution_grant_uses_canonical_env_resolution_not_raw_env_input() {
    let mut padded = test_dispatch_params(Some("custom"), "canonical managed env");
    padded.env_id = Some("  env-canonical  ".to_string());
    let lease = memcore::ExecEnvLease {
        env_id: "env-canonical".to_string(),
        kind: "worktree".to_string(),
        path: "/canonical/worktree".to_string(),
        repo_root: "/repo".to_string(),
        branch: "branch".to_string(),
        base_sha: "base".to_string(),
        dispatch_id: None,
        agent_identity_id: None,
        claim_id: None,
        env_class: Default::default(),
        state: memcore::ExecEnvState::Active,
        reclaim_reason: None,
        schema_version: 1,
        created_at: "2026-08-21T00:00:00Z".to_string(),
        reclaimed_at: None,
    };
    let managed = crate::exec_env_ops::resolve_env_binding(
        padded.env_id.as_deref(),
        None,
        false,
        Some(&lease),
    )
    .expect("padded env id resolves through the real env gate");
    let grant = mint_execution_grant(&mut padded, "managed-grant", &managed)
        .expect("canonical managed resolution mints a grant");
    assert_eq!(grant.env_id.as_deref(), Some("env-canonical"));
    assert_eq!(
        grant.allowed_cwd.as_deref(),
        Some(std::path::Path::new("/canonical/worktree"))
    );
    assert_eq!(
        padded.env_id.as_deref(),
        Some("  env-canonical  "),
        "legacy env id retains raw spelling while the grant is canonical"
    );
    let mut managed_env_mutant = grant.clone();
    managed_env_mutant.allowed_cwd = None;
    assert!(
        assert_grant_legacy_projection(&padded, &managed_env_mutant, &managed).is_err(),
        "managed env grant must retain the authoritative resolved cwd"
    );

    let mut whitespace = test_dispatch_params(Some("custom"), "canonical default env");
    whitespace.env_id = Some(" \t ".to_string());
    let default =
        crate::exec_env_ops::resolve_env_binding(whitespace.env_id.as_deref(), None, false, None)
            .expect("whitespace-only env id resolves through the real env gate");
    let grant = mint_execution_grant(&mut whitespace, "default-grant", &default)
        .expect("whitespace-only env id resolves to the daemon default");
    assert_eq!(grant.env_id, None);
    assert_eq!(grant.allowed_cwd, None);
    assert_eq!(
        whitespace.env_id.as_deref(),
        Some(" \t "),
        "legacy whitespace input remains untouched while the grant uses the default"
    );
    let mut default_env_mutant = grant.clone();
    default_env_mutant.unmanaged_cwd_allowed = true;
    assert!(
        assert_grant_legacy_projection(&whitespace, &default_env_mutant, &default).is_err(),
        "default env grant must not become unmanaged"
    );
}

#[test]
fn compiled_permission_projection_preserves_verify_headless_spelling() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _full = EnvRestore::remove("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE");
    let _verify = EnvRestore::set("TACHI_DISPATCH_VERIFY_HEADLESS", "true");
    let server = crate::tests::make_server();

    let mut omitted = test_dispatch_params(Some("claude"), "default permission projection");
    let omitted_start = resolve_dispatch_start(
        &server,
        &mut omitted,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    )
    .expect("omitted permission profile resolves");
    compile_dispatch_contract(
        &mut omitted,
        &omitted_start.agent_norm,
        "cli",
        &omitted_start.resolved_profile,
        tachi_dispatch::PROVIDER_QUALIFICATIONS,
        None,
    )
    .expect("default permission authority compiles");
    assert_eq!(omitted.permission_profile.as_deref(), Some("default"));

    let mut verify = test_dispatch_params(Some("claude"), "verify permission projection");
    verify.permission_profile = Some("verify".to_string());
    let verify_start = resolve_dispatch_start(
        &server,
        &mut verify,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    )
    .expect("verify permission profile resolves");
    compile_dispatch_contract(
        &mut verify,
        &verify_start.agent_norm,
        "cli",
        &verify_start.resolved_profile,
        tachi_dispatch::PROVIDER_QUALIFICATIONS,
        None,
    )
    .expect("verify-headless authority compiles without the full opt-in");
    assert_eq!(
        verify.permission_profile.as_deref(),
        Some("verify"),
        "the launcher must replay the admitted verify spelling, not full"
    );
    let grant = mint_execution_grant(
        &mut verify,
        "verify-headless-grant",
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("verify-headless grant minting preserves the admitted spelling");
    assert_eq!(
        grant.permission_profile.as_deref(),
        Some("verify"),
        "grant must preserve replay-safe verify rather than rewrite it to full"
    );
    assert_grant_legacy_projection(
        &verify,
        &grant,
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("grant projection must retain the replay-safe verify spelling");
    let mut full_mutant = grant.clone();
    full_mutant.permission_profile = Some("full".to_string());
    assert!(
        assert_grant_legacy_projection(
            &verify,
            &full_mutant,
            &crate::exec_env_ops::EnvResolution::Default,
        )
        .is_err(),
        "verify-to-full grant mutant must be observable before a different env gate can run"
    );
}

fn spawn_auth_gated_opencode_doc_server() -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept probe");
            let mut buf = [0_u8; 2048];
            let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let is_doc = request.starts_with("GET /doc ");
            let has_auth = request.contains("Authorization: Basic ");
            let (status, body) = if is_doc && !has_auth {
                ("401 Unauthorized", "")
            } else if is_doc {
                ("200 OK", OPENCODE_DOC_FIXTURE)
            } else {
                ("200 OK", "<title>OpenCode</title>")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
        }
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

fn single_run_dir(run_root: &std::path::Path) -> std::path::PathBuf {
    let runs = std::fs::read_dir(run_root)
        .expect("read run root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected one run dir, got {runs:?}");
    runs.into_iter().next().expect("one run dir")
}

async fn wait_for_result(run_dir: &std::path::Path) -> String {
    let result_path = run_dir.join("result.md");
    for _ in 0..120 {
        if let Ok(result) = tokio::fs::read_to_string(&result_path).await {
            return result;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("dispatch result was not written: {}", result_path.display());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn generate_mcp_config_sets_owner_only_permissions() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let server = crate::tests::make_server();
    let path = generate_mcp_config(&server, "test-perms", true, false, None, None, &[])
        .await
        .expect("generate mcp config")
        .expect("config path");

    assert!(path.exists());
    let temp_leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("config parent"))
        .expect("read config parent")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("dispatch-test-perms-mcp.json.tmp."))
        .collect();
    assert!(
        temp_leftovers.is_empty(),
        "MCP config atomic write should not leave temp files: {temp_leftovers:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "MCP config mode should be 0o600, got {:#o}",
            mode
        );
    }

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

// Dispatch identity is lane-specific, not profile-derived. Two workers may
// share a capability profile but must retain distinct `TACHI_AGENT_SEAT`
// values for claims and runtime attribution.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cp2_two_dispatches_on_same_profile_get_distinct_agent_seats() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let server = crate::tests::make_server();

    // Simulate two workers dispatched on the identical `profile` name
    // ("codex_55_review") — the exact scenario codex's CP2 finding named.
    // In the real handler, `agent_seat` is `Some(dispatch_id.as_str())`
    // (dispatch.rs); each dispatch call gets its own freshly generated
    // `dispatch_id` (new_dispatch_id embeds a uuid suffix), never the shared
    // `profile` string. Two distinct dispatch_ids stand in for that here.
    let dispatch_id_a = new_dispatch_id(Utc::now(), "codex");
    let dispatch_id_b = new_dispatch_id(Utc::now(), "codex");
    assert_ne!(
        dispatch_id_a, dispatch_id_b,
        "two dispatch calls must get distinct dispatch_ids"
    );

    let path_a = generate_mcp_config(
        &server,
        &dispatch_id_a,
        true,
        false,
        Some("codex_55_review"),
        Some(&dispatch_id_a),
        &[],
    )
    .await
    .expect("generate mcp config a")
    .expect("config path a");
    let path_b = generate_mcp_config(
        &server,
        &dispatch_id_b,
        true,
        false,
        Some("codex_55_review"),
        Some(&dispatch_id_b),
        &[],
    )
    .await
    .expect("generate mcp config b")
    .expect("config path b");

    let seat_of = |path: &std::path::Path| -> String {
        let raw = std::fs::read_to_string(path).expect("read mcp config");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse mcp config json");
        json["mcpServers"]["tachi"]["env"]["TACHI_AGENT_SEAT"]
            .as_str()
            .expect("TACHI_AGENT_SEAT present in generated config")
            .to_string()
    };
    let seat_a = seat_of(&path_a);
    let seat_b = seat_of(&path_b);

    assert_eq!(
        seat_a, dispatch_id_a,
        "seat must be the dispatch id, not the shared profile"
    );
    assert_eq!(
        seat_b, dispatch_id_b,
        "seat must be the dispatch id, not the shared profile"
    );
    assert_ne!(
        seat_a, seat_b,
        "two workers on the SAME profile must get DISTINCT seats — this is the CP2 regression"
    );
    assert_ne!(
        seat_a, "codex_55_review",
        "seat must never equal the shared profile name"
    );
    assert_ne!(
        seat_b, "codex_55_review",
        "seat must never equal the shared profile name"
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

// ─── #1251: child recursion-depth env stamp (child = parent + 1) ──────────
//
// The generated worker `tachi serve` env must carry TACHI_DISPATCH_DEPTH =
// dispatching-session depth + 1. This is the parent end of the recursion rail:
// the child proxy reads this env back and re-emits it as the
// X-Tachi-Dispatch-Depth header on every daemon call, so the daemon's
// recursion gate sees the SESSION depth, never the daemon's own (always-0)
// process env.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_depth_child_env_is_parent_plus_one() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _home = EnvRestore::set_path("HOME", temp_home.path());
    let _tachi_home = EnvRestore::remove("TACHI_HOME");

    let server = crate::tests::make_server();

    let depth_of = |path: &std::path::Path| -> String {
        let raw = std::fs::read_to_string(path).expect("read mcp config");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse mcp config json");
        json["mcpServers"]["tachi"]["env"]["TACHI_DISPATCH_DEPTH"]
            .as_str()
            .expect("TACHI_DISPATCH_DEPTH present in generated config")
            .to_string()
    };

    // A leader session carries no depth marker (session_dispatch_depth None ≡
    // depth 0) → the child it dispatches is stamped depth 1.
    let leader_child = generate_mcp_config(&server, "depth-leader", true, false, None, None, &[])
        .await
        .expect("generate mcp config for leader")
        .expect("config path");
    assert_eq!(
        depth_of(&leader_child),
        "1",
        "a leader (depth 0) must stamp its child at depth 1"
    );

    // A session already at depth 1 → its child is stamped depth 2.
    server.set_session_dispatch_depth(Some("1".to_string()));
    let depth1_child = generate_mcp_config(&server, "depth-child", true, false, None, None, &[])
        .await
        .expect("generate mcp config for depth-1 session")
        .expect("config path");
    assert_eq!(
        depth_of(&depth1_child),
        "2",
        "a depth-1 session must stamp its child at depth 2"
    );

    // A malformed inbound depth marker saturates to the limit for the parent,
    // so its child is stamped one past the limit — it can only ever fail the
    // gate, never wrap back to a small allowed depth.
    server.set_session_dispatch_depth(Some("garbage".to_string()));
    let saturated_child = generate_mcp_config(&server, "depth-bad", true, false, None, None, &[])
        .await
        .expect("generate mcp config for malformed-depth session")
        .expect("config path");
    assert_eq!(
        depth_of(&saturated_child),
        (crate::session_identity::MAX_DISPATCH_DEPTH + 1).to_string(),
        "a malformed parent depth saturates to the limit and stamps limit+1 on its child"
    );
}

// ─── #1251: the gate reads the SESSION depth, not process env ─────────────
//
// A session whose identity carries depth == MAX_DISPATCH_DEPTH must be refused
// by `handle_tachi_dispatch` BEFORE any workspace/run-directory work — proving
// the depth reaches the gate via the session (the header/env-populated field),
// which is exactly the value the daemon-proxy rail delivers. The error must be
// the recursion-gate error, and it must fire before the (heavier) dispatch
// stages that would otherwise fail for unrelated reasons.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn handle_tachi_dispatch_refuses_at_max_depth_from_session() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _home = EnvRestore::set_path("HOME", temp_home.path());
    let _tachi_home = EnvRestore::remove("TACHI_HOME");
    // Belt-and-suspenders: prove the refusal comes from the SESSION field, not
    // this test process's own env — leave TACHI_DISPATCH_DEPTH unset (depth 0)
    // while the session says MAX.
    let _env_depth = EnvRestore::remove(crate::session_identity::ENV_DISPATCH_DEPTH);

    let server = crate::tests::make_server();
    server.set_session_dispatch_depth(Some(
        crate::session_identity::MAX_DISPATCH_DEPTH.to_string(),
    ));

    let params = test_dispatch_params(Some("codex"), "noop task");
    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("a session already at MAX_DISPATCH_DEPTH must be refused");
    assert!(
        err.contains("recursive dispatch depth limit reached")
            && err.contains("MAX_DISPATCH_DEPTH"),
        "expected the recursion-gate error, got: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn opencode_serve_dispatch_fails_fast_when_probe_auth_fails() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let _host_profile = EnvRestore::set("TACHI_HOST_PROFILE", "development");
    let run_root = dispatch_runs_root();
    let _password = EnvRestore::remove("OPENCODE_SERVER_PASSWORD");
    let _username = EnvRestore::remove("OPENCODE_SERVER_USERNAME");
    let server = crate::tests::make_server();
    let (server_url, probe_server) = spawn_auth_gated_opencode_doc_server();

    let mut params = test_dispatch_params(Some("custom"), "should fail before subprocess");
    params.harness_transport = Some("opencode_serve".to_string());
    params.harness_server_url = Some(server_url);
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('should-not-run')".to_string(),
    ];

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("opencode_serve dispatch should fail before spawn");
    probe_server.join().expect("probe server thread");

    assert!(
        err.contains("opencode_serve attach not ready"),
        "unexpected error: {err}"
    );
    assert!(
        err.contains("OPENCODE_SERVER_PASSWORD"),
        "error should tell the user how to fix auth: {err}"
    );
    assert!(
        !err.contains("Session not found"),
        "preflight should not surface the subprocess fallback error: {err}"
    );

    let run_dir = single_run_dir(&run_root);
    let result = std::fs::read_to_string(run_dir.join("result.md")).expect("read result");
    assert!(result.contains("HTTP 401"), "result={result}");
    assert!(
        result.contains("OPENCODE_SERVER_PASSWORD"),
        "result={result}"
    );
    assert!(!result.contains("Session not found"), "result={result}");
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status JSON");
    assert_eq!(status["state"], json!("TASK_STATE_FAILED"));
    assert_eq!(status["host_profile"], json!("development"));
    assert_eq!(status["execution_level"], json!("L1"));
    assert_eq!(
        status["identity_receipt"]["contract_id"],
        json!(tachi_dispatch::DISPATCH_IDENTITY_CONTRACT_ID),
        "receipt-first status seed must retain the dispatch identity through preflight failure"
    );
    assert_eq!(
        status["harness_server_status"]["doc_error"],
        json!("OpenCode /doc returned HTTP 401")
    );
    assert_eq!(
        status["harness_server_status"]["server_password_configured"],
        json!(false)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn opencode_serve_preflight_uses_dispatch_credential_env() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let _password = EnvRestore::remove("OPENCODE_SERVER_PASSWORD");
    let _username = EnvRestore::remove("OPENCODE_SERVER_USERNAME");
    let server = crate::tests::make_server();
    let project = tempfile::tempdir().expect("temp project");
    let credentials_dir = project.path().join(".tachi/credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    std::fs::write(
        credentials_dir.join("opencode.json"),
        r#"{
          "credential_profiles": {
            "opencode_server_auth": {
              "entries": { "password": "OPENCODE_SERVER_PASSWORD_TEST" },
              "allowed_consumers": { "agents": ["custom"] },
              "materializers": [
                { "type": "env", "source": "password", "target": "OPENCODE_SERVER_PASSWORD" }
              ]
            }
          }
        }"#,
    )
    .expect("write credential profile");
    server
        .vault_init(rmcp::handler::server::wrapper::Parameters(
            crate::vault_ops::VaultInitParams {
                password: "dispatch opencode serve auth test".to_string(),
            },
        ))
        .await
        .expect("vault_init");
    server
        .vault_set(rmcp::handler::server::wrapper::Parameters(
            crate::vault_ops::VaultSetParams {
                name: "OPENCODE_SERVER_PASSWORD_TEST".to_string(),
                value: "test123".to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "dispatch opencode serve password".to_string(),
                allowed_agents: Some(vec!["custom".to_string()]),
                enable_rotation: false,
                rotation_strategy: None,
            },
        ))
        .await
        .expect("vault_set");
    let (server_url, probe_server) = spawn_auth_gated_opencode_doc_server();

    let mut params = test_dispatch_params(Some("custom"), "should pass preflight");
    params.cwd = Some(project.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    params.harness_transport = Some("opencode_serve".to_string());
    params.harness_server_url = Some(server_url);
    params.credential_profiles = vec!["opencode_server_auth".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('credential-ok')".to_string(),
    ];

    let raw = handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start with per-dispatch opencode serve auth");
    probe_server.join().expect("probe server thread");
    assert!(
        !raw.contains("test123"),
        "dispatch response must not leak credential values: {raw}"
    );
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run_dir"));
    let result = wait_for_result(&run_dir).await;
    assert!(result.contains("credential-ok"), "result={result}");
}

/// #1174 (codex review round): `build_opencode_command`'s unit tests
/// (`dispatch_ops::launcher::tests`) prove the seam in isolation, but the
/// live wiring — `prepare_dispatch_backend` actually calling
/// `build_opencode_command` instead of `build_custom_command` for
/// `agent='opencode'` — was only exercised by hand. This drives the real
/// `handle_tachi_dispatch` entry point with `agent='opencode'` and no
/// `command`, so a regression that reverts the `"opencode" =>
/// build_opencode_command(...)` dispatch arm back to `build_custom_command`
/// fails a runtime test, not just the isolated unit test. On `origin/main`
/// (pre-fix) this is red: the error carries the internal 'custom' backend
/// name instead of the caller's own 'opencode' vocabulary.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn opencode_agent_missing_command_keeps_vocabulary_through_full_dispatch() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let server = crate::tests::make_server();

    let params = test_dispatch_params(Some("opencode"), "should fail before any spawn");

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("agent='opencode' with no command must fail end-to-end");

    assert!(
        err.contains("opencode"),
        "error must keep the caller's own 'opencode' vocabulary, got: {err}"
    );
    assert!(
        err.to_ascii_lowercase().contains("profile"),
        "error must point the caller at profile dispatch, got: {err}"
    );
    assert!(
        !err.contains("custom"),
        "error must not leak the internal 'custom' backend name, got: {err}"
    );
}

/// #894 S0 round 2 (cross-vendor review): the entry-point sandbox check must
/// fire before ANY stage/preflight/spawn work — in particular before the V2
/// plan stage's LLM call. Proven two ways without needing to mock the LLM
/// call: (1) the error text is exactly the entry-point sandbox rejection,
/// never the `"dispatch v2 stage1"` wrapper `run_plan_stage` would have
/// produced had it actually been reached; (2) no run directory is created at
/// all (workspace creation is step 1, which never runs either).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn v2_auto_stage_rejects_unsupported_sandbox_before_plan_stage_spawn() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let run_root = dispatch_runs_root();
    let server = crate::tests::make_server();

    let mut params = test_dispatch_params(
        Some("claude"),
        "should fail before any V2 plan-stage LLM call",
    );
    params.stage = Some("auto".to_string());
    params.sandbox = Some("workspace-write".to_string());

    let started = std::time::Instant::now();
    let err = handle_tachi_dispatch(&server, params).await.expect_err(
        "an unsupported sandbox on a V2/auto dispatch must be rejected before Stage 1 makes the plan-stage LLM call",
    );
    let elapsed = started.elapsed();

    assert!(
        err.contains("claude") && err.contains("has no sandbox concept"),
        "must be the entry-point sandbox rejection: {err}"
    );
    assert!(
        !err.contains("dispatch v2 stage1"),
        "must fail before ever calling run_plan_stage / the plan-stage LLM call: {err}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "entry-point rejection must be near-instant (no pool spawn, no network call); took {elapsed:?}"
    );
    let run_dir_count = std::fs::read_dir(&run_root)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(
        run_dir_count, 0,
        "no run directory should exist — workspace creation (step 1) never ran"
    );
}

/// #894 S2d discriminating test ③: a read-only REQUEST addressed to a backend
/// that no kill-test has certified to enforce read-only must be refused
/// **before spawn** — meaning before the run directory exists and therefore
/// before any credential can be materialized into it
/// (`plan_credential_materialization_with_run_dir` writes under the run dir,
/// which is created in step 1; the authority compiler runs in step 0b). The
/// receipt names the backend, the requested level, and the missing
/// qualification: a valid vendor flag is not provider qualification.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn read_only_request_to_uncertified_backend_is_refused_before_run_dir_or_credentials() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let run_root = dispatch_runs_root();
    let server = crate::tests::make_server();

    let mut params = test_dispatch_params(
        Some("claude"),
        "read-only review on a backend that cannot enforce read-only",
    );
    params.sandbox = Some("read-only".to_string());
    params.credential_profiles = vec!["opencode_shared".to_string()];

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("an uncertified backend must not receive a read-only dispatch");

    assert!(
        err.contains("claude") && err.contains("read-only"),
        "receipt must name the backend and the requested level: {err}"
    );
    assert!(err.contains("fail-closed"), "{err}");
    assert!(
        err.contains("no kill-test-certified sandbox enforcement"),
        "receipt must name the missing qualification, not just the missing flag: {err}"
    );

    let run_dirs = std::fs::read_dir(&run_root)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(
        run_dirs, 0,
        "refusal must land before step 1 (run dir) — and therefore before any credential is materialized into it"
    );
}

/// #894 S1: the fail-safe env-binding gate must fire through the real
/// `handle_tachi_dispatch` entrypoint, not just at the pure
/// `resolve_env_binding` unit level (see `exec_env_ops::tests`). A bare `cwd`
/// with neither `env_id` nor `unmanaged_cwd:true` must be rejected before any
/// run directory is created, with the exact fail-safe error text.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_rejects_bare_cwd_without_unmanaged_optin_or_env_id() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let run_root = dispatch_runs_root();
    let server = crate::tests::make_server();
    let bare_cwd = tempfile::tempdir().expect("bare cwd dir");

    let mut params = test_dispatch_params(Some("custom"), "should fail the env-binding gate");
    params.cwd = Some(bare_cwd.path().to_string_lossy().to_string());
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("a bare cwd without unmanaged_cwd:true or env_id must be rejected (#894 S1)");

    assert!(
        err.contains("a bare cwd is only accepted with explicit unmanaged_cwd:true or an env_id"),
        "must be the fail-safe env-binding gate rejection: {err}"
    );
    let run_dir_count = std::fs::read_dir(&run_root)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(
        run_dir_count, 0,
        "the gate must fire before any run directory is created"
    );
}

/// #1010: an L2 request on a development machine must be rejected before the
/// existing environment/workspace setup path can create a run directory.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_rejects_level_above_host_profile_before_workspace_creation() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let _host_profile = EnvRestore::set("TACHI_HOST_PROFILE", "development");
    let run_root = dispatch_runs_root();
    let server = crate::tests::make_server();

    let mut params = test_dispatch_params(Some("custom"), "read product diagnostics");
    params.execution_level = Some(tachi_params::ExecutionLevel::L2);
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("development profile must reject L2 before dispatch setup");
    assert!(
        err.contains("host_profile_mismatch"),
        "unexpected error: {err}"
    );
    assert!(err.contains("development"), "unexpected error: {err}");
    assert!(err.contains("L2"), "unexpected error: {err}");

    let run_dir_count = std::fs::read_dir(&run_root)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(
        run_dir_count, 0,
        "host-profile rejection must fire before any run directory exists"
    );
}

#[test]
fn mcp_cleanup_removes_temp_config_on_drop() {
    let temp_home = tempfile::tempdir().expect("temp home");
    let path = temp_home.path().join("dispatch-test-mcp.json");
    std::fs::write(&path, b"{}").expect("write temp config");
    assert!(path.exists());
    {
        let _cleanup = McpCleanup(Some(path.clone()));
    }
    assert!(!path.exists(), "MCP config should be removed on drop");
}

#[test]
fn dispatch_runs_root_uses_canonical_tachi_home_aliases() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let sigil_home = tempfile::tempdir().expect("sigil home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    let original_sigil_home = std::env::var_os("SIGIL_HOME");
    let original_app_home = std::env::var_os("TACHI_APP_HOME");

    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");
    std::env::set_var("SIGIL_HOME", sigil_home.path());
    std::env::remove_var("TACHI_APP_HOME");
    assert_eq!(dispatch_runs_root(), sigil_home.path().join("runs"));

    std::env::remove_var("SIGIL_HOME");
    std::env::set_var("TACHI_APP_HOME", "~/custom-tachi");
    assert_eq!(
        dispatch_runs_root(),
        temp_home.path().join("custom-tachi").join("runs")
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
    if let Some(value) = original_sigil_home {
        std::env::set_var("SIGIL_HOME", value);
    } else {
        std::env::remove_var("SIGIL_HOME");
    }
    if let Some(value) = original_app_home {
        std::env::set_var("TACHI_APP_HOME", value);
    } else {
        std::env::remove_var("TACHI_APP_HOME");
    }
}

/// Minimal isolated `MemoryServer` for tests that need a real DB target but
/// no project store — mirrors `complete_ops::dispatch_outcome`'s local
/// `test_server` helper.
fn test_server() -> (MemoryServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let global_db = dir.path().join("global.sqlite");
    let server = MemoryServer::new(global_db, None).expect("server");
    (server, dir)
}

/// #774 round 3 discriminator (leg 3, regression): a pre-round-3 `status.json`
/// blob (no `"project"` key at all — every run directory written before this
/// fix) must recover exactly as before: the outcome row lands in the default
/// store via `resolve_write_scope("")`, not silently break or panic on the
/// missing field. `status.get("project")` on a blob without that key is
/// `None`, so `record_terminal_failure_outcome` takes its `project: None`
/// branch same as pre-fix — this test's assertions (global-store row present)
/// are unchanged from round 2 and still pass, proving backward compatibility.
#[test]
fn recover_orphaned_dispatch_runs_marks_working_runs_failed() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let (server, _db_dir) = test_server();

    let run_dir = dispatch_runs_root().join("20260614T000000Z-claude-deadbeef");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": "20260614T000000Z-claude-deadbeef",
            "state": "TASK_STATE_WORKING",
            "agent": "claude",
        })
        .to_string(),
    )
    .expect("status");

    let recovered = recover_orphaned_dispatch_runs(&server);
    assert_eq!(
        recovered,
        vec!["20260614T000000Z-claude-deadbeef".to_string()]
    );

    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["state"], "TASK_STATE_FAILED");
    assert_eq!(status["recovery_reason"], "daemon_restart_orphan_recovery");

    // #774 round 2: recovery is a terminal path — it must also write a
    // canonical dispatch_outcomes row (previously it only rewrote
    // status.json and left the outcome ledger silent about this dispatch).
    let rows = server
        .with_global_store_read(|store| {
            memcore::list_outcomes_by_vendor_window(
                store.connection(),
                "claude",
                "1970-01-01T00:00:00Z",
                None,
                memcore::OutcomeEvidenceClass::AnyAttribution,
            )
            .map_err(|e| e.to_string())
        })
        .expect("read outcomes");
    let row = rows
        .iter()
        .find(|r| r.dispatch_id == "20260614T000000Z-claude-deadbeef")
        .expect("recovered orphan outcome row present");
    assert_eq!(row.execution_outcome, "failed");
    assert_eq!(
        row.reported_outcome, None,
        "no self-report on a recovered orphan"
    );
    assert_eq!(row.error_class.as_deref(), Some("recovered_orphan"));

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

/// #774 round 3 discriminator (leg 1): a named-project dispatch's
/// `status.json` receipt must carry `params.project` — this is the ONLY
/// place daemon-restart orphan recovery (`recover_orphaned_dispatch_runs`)
/// can read it back from after a crash, since the original
/// `TachiDispatchParams` is gone by then. Checks both the receipt-first seed
/// and the post-artifacts enrich write land the field (reading status.json
/// after `handle_tachi_dispatch` returns observes whichever write happened
/// last, since both run synchronously before the background subprocess
/// spawns).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn named_project_dispatch_receipt_carries_project_field() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let server = crate::tests::make_server();

    // The dispatch's own background completion also resolves `project` —
    // give it a real named-project store so the fast subprocess below can
    // complete cleanly instead of erroring on a missing store.
    let named_db = temp_home
        .path()
        .join(".tachi")
        .join("projects")
        .join("hyperion")
        .join("memory.db");
    std::fs::create_dir_all(named_db.parent().unwrap()).expect("named project dir");
    std::fs::write(&named_db, b"").expect("named project db placeholder");

    let mut params = test_dispatch_params(Some("custom"), "stamp project into receipt");
    params.project = Some("hyperion".to_string());
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let dispatch_response = handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start");

    // Resolve this dispatch's OWN run dir precisely from the response's
    // `run_dir` field rather than scanning `run_root` and asserting exactly
    // one entry (`single_run_dir`): that scan is environment-dependent — a
    // concurrent/leftover run dir under the same `TACHI_HOME` (e.g. from a
    // parallel test thread racing this one, since `TACHI_HOME` is a
    // process-global env var) makes the count wrong without this dispatch's
    // own receipt being at fault. Reading `run_dir` straight off the
    // dispatch's own response is exact and environment-independent.
    let response: Value = serde_json::from_str(&dispatch_response).expect("response JSON");
    let run_dir = std::path::PathBuf::from(
        response["run_dir"]
            .as_str()
            .expect("response carries run_dir"),
    );
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status JSON");
    assert_eq!(
        status["project"],
        json!("hyperion"),
        "receipt must carry the dispatch's named project so crash recovery can read it back: {status}"
    );
}

/// #894 S2d: the authority receipt must actually land in the on-disk receipt
/// and in the dispatch response — "who was stopping this agent from writing?"
/// has to be answerable from the ledger after the fact, not only in the head of
/// the code that compiled it. Uses the same fast `python3 -c pass` subprocess
/// the project-receipt test uses so no vendor CLI is spawned.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_receipt_carries_the_effective_authority_contract() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let server = crate::tests::make_server();

    let mut params = test_dispatch_params(Some("custom"), "stamp the authority receipt");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let dispatch_response = handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start");
    let response: Value = serde_json::from_str(&dispatch_response).expect("response JSON");

    assert_eq!(
        response["authority"]["workspace_authority"],
        json!("workspace-write"),
        "profile-less custom dispatch keeps the legacy default: {response}"
    );
    assert_eq!(
        response["authority"]["enforcement"]["mode"],
        json!("advisory"),
        "custom/opencode has no sandbox primitive — the receipt must say so, not imply isolation: {response}"
    );

    let run_dir = std::path::PathBuf::from(
        response["run_dir"]
            .as_str()
            .expect("response carries run_dir"),
    );
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status JSON");
    assert_eq!(
        status["authority"]["enforcement"]["mode"],
        json!("advisory"),
        "the on-disk receipt must carry the enforcement mode: {status}"
    );
}

/// #1324: a successful external-staffing start is receipt-first, and the
/// accepted response and terminal worker evidence stay in one canonical run
/// directory. This test exercises the canonical kernel directly; legacy
/// Shell/Arena projections are frozen by the separate structural inventory.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn canonical_external_staffing_start_and_terminal_receipt_share_one_run_dir() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let cwd = tempfile::tempdir().expect("dispatch cwd");
    let release_worker = cwd.path().join("release-worker");
    let run_root = dispatch_runs_root();
    let server = crate::tests::make_server();

    let mut params = test_dispatch_params(Some("custom"), "prove one staffing receipt");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import pathlib,sys,time; p=pathlib.Path(sys.argv[1]);\nwhile not p.exists(): time.sleep(0.01)\nprint('canonical staffing result')".to_string(),
        release_worker.to_string_lossy().to_string(),
    ];
    params.cwd = Some(cwd.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);

    let raw = handle_tachi_dispatch(&server, params)
        .await
        .expect("staffing start accepted");
    let response: Value = serde_json::from_str(&raw).expect("start response JSON");
    let dispatch_id = response["dispatch_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .expect("non-empty dispatch_id");
    assert_eq!(response["state"], json!("TASK_STATE_WORKING"));
    let run_dir = std::path::PathBuf::from(
        response["run_dir"]
            .as_str()
            .filter(|path| !path.is_empty())
            .expect("non-empty canonical run_dir"),
    );
    assert_eq!(run_dir, run_root.join(dispatch_id));

    let accepted_status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json"))
            .expect("status exists before start returns"),
    )
    .expect("accepted status JSON");
    assert_eq!(accepted_status["dispatch_id"], json!(dispatch_id));
    assert_eq!(accepted_status["state"], response["state"]);
    assert_eq!(accepted_status["run_dir"], response["run_dir"]);

    std::fs::write(&release_worker, b"release").expect("release custom worker");
    let result = wait_for_result(&run_dir).await;
    assert_eq!(result.trim(), "canonical staffing result");
    let result_locations = std::fs::read_dir(&run_root)
        .expect("read canonical runs root")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("result.md"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(
        result_locations,
        vec![run_dir.join("result.md")],
        "one worker result must have one canonical run location"
    );

    let mut terminal_status = None;
    for _ in 0..120 {
        let status: Value = serde_json::from_str(
            &tokio::fs::read_to_string(run_dir.join("status.json"))
                .await
                .expect("terminal status remains readable"),
        )
        .expect("terminal status JSON");
        if matches!(
            status["state"].as_str(),
            Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
        ) {
            terminal_status = Some(status);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let terminal_status = terminal_status.expect("dispatch reaches a terminal receipt");
    assert_eq!(terminal_status["dispatch_id"], json!(dispatch_id));
    assert_eq!(terminal_status["run_dir"], response["run_dir"]);
    assert_eq!(terminal_status["state"], json!("TASK_STATE_COMPLETED"));
    assert!(terminal_status["result_written"].as_bool().unwrap_or(false));

    let trajectory = tokio::fs::read_to_string(run_dir.join("trajectory.jsonl"))
        .await
        .expect("canonical trajectory exists");
    assert!(trajectory.contains("dispatch_received"), "{trajectory}");
    assert!(trajectory.contains("dispatch_finished"), "{trajectory}");
}

/// tachi#1173 item 1 discriminator: on origin/main (pre-#1173) the dispatch
/// response always embeds the full routing card (`profile` — the whole
/// `ResolvedDispatchProfile` including its own nested `mbit_card` and
/// `identity_receipt` — plus top-level `identity_receipt` and
/// `dispatch_profile` duplicating the same mbit_card again), so this
/// assertion is RED before the fix (those keys are always present) and GREEN
/// after (they're absent by default). The default receipt must still carry
/// the four fields the issue names: dispatch_id, state, run_dir,
/// suggested_complete_command.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_response_default_omits_fat_routing_card() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let server = crate::tests::make_server();

    let mut params = test_dispatch_params(Some("custom"), "slim receipt by default");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    params.profile = Some("glm_impl".to_string());

    let dispatch_response = handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start");
    let response: Value = serde_json::from_str(&dispatch_response).expect("response JSON");

    // #1182 checkpoint 3 (codex review round 2): the slimming assertions run
    // FIRST, before the `state`/`dispatch_id`/etc. shape checks below. On
    // origin/main (pre-#1173) `profile`/`identity_receipt`/`dispatch_profile`
    // are unconditionally present, so this is the assertion that actually
    // fails first when run against base — the behavioral claim this test
    // exists to discriminate. The `state` field further down is a legitimate
    // but separate addition (base only nested it at `task.status.state`);
    // ordering it after the slimming checks keeps this test's first failure
    // pointing at the fat-payload regression it's named for, not an
    // unrelated shape addition.
    assert!(
        response.get("profile").is_none(),
        "default dispatch response must not carry the full routing card: {response}"
    );
    assert!(
        response.get("identity_receipt").is_none(),
        "default dispatch response must not carry identity_receipt: {response}"
    );
    assert!(
        response.get("dispatch_profile").is_none(),
        "default dispatch response must not carry the mbit_card dispatch_profile: {response}"
    );
    assert!(
        !dispatch_response.contains("mbit_card"),
        "{dispatch_response}"
    );
    assert_eq!(response["verbose"], json!(false), "{response}");

    // The remaining fields the issue names as the default receipt shape.
    // `state` is a genuinely new top-level field this PR adds (pre-#1173 it
    // was only nested at `task.status.state`) — a separate, additive change
    // from the slimming above, asserted here rather than mixed into the
    // slimming block.
    assert!(response["dispatch_id"].is_string(), "{response}");
    assert_eq!(response["state"], json!("TASK_STATE_WORKING"), "{response}");
    assert!(response["run_dir"].is_string(), "{response}");
    assert!(
        response["suggested_complete_command"].is_object(),
        "{response}"
    );
}

/// tachi#1173 item 1 discriminator (verbose escape hatch): verbose=true must
/// restore the exact pre-#1173 full routing card so no information is lost,
/// only deferred behind an explicit opt-in.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_response_verbose_true_restores_full_routing_card() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let server = crate::tests::make_server();

    let mut params = test_dispatch_params(Some("custom"), "verbose receipt on request");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    params.profile = Some("glm_impl".to_string());
    params.verbose = Some(true);

    let dispatch_response = handle_tachi_dispatch(&server, params)
        .await
        .expect("dispatch should start");
    let response: Value = serde_json::from_str(&dispatch_response).expect("response JSON");

    assert_eq!(response["verbose"], json!(true), "{response}");
    assert!(
        response["profile"].is_object(),
        "verbose=true must carry the full routing card: {response}"
    );
    assert!(
        response["profile"]["profile_card"].is_object(),
        "verbose=true's `profile` is the full ResolvedDispatchProfile, which nests its own profile_card: {response}"
    );
    assert!(
        response["identity_receipt"].is_object(),
        "verbose=true must carry identity_receipt: {response}"
    );
    assert!(
        response["dispatch_profile"].is_object(),
        "verbose=true must carry the dispatch_profile: {response}"
    );
    assert_eq!(
        response["profile"]["profile_card"], response["dispatch_profile"],
        "self-nested profile_card copies must be the exact same value: {response}"
    );
}

/// #774 round 3 discriminator (leg 2): recovery must resolve the recovered
/// orphan's terminal outcome row to the SAME named-project store a live
/// `tachi_complete` for that dispatch would have used — reusing round 2's
/// `with_named_project_store` addressing (same directory shape
/// `with_named_project_env` sets up in `complete_ops::dispatch_outcome`'s
/// tests: `<TACHI_HOME>/projects/<name>/memory.db`) but exercised through the
/// crash-recovery path instead of a live dispatch. Before this fix,
/// `recover_orphaned_dispatch_runs` always passed `project: None`, so this
/// row would have landed in the default global store instead — reproducing
/// the exact split round 2 closed for the other three terminal-without-
/// complete call sites.
#[test]
fn recover_orphaned_dispatch_runs_honors_project_from_receipt() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let named_db = crate::path_utils::tachi_home()
        .join("projects")
        .join("hyperion")
        .join("memory.db");
    std::fs::create_dir_all(named_db.parent().unwrap()).expect("named project dir");
    std::fs::write(&named_db, b"").expect("named project db placeholder");

    let (server, _db_dir) = test_server();

    let run_dir = dispatch_runs_root().join("20260614T000000Z-codex-feedcafe");
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(
        run_dir.join("status.json"),
        json!({
            "dispatch_id": "20260614T000000Z-codex-feedcafe",
            "state": "TASK_STATE_WORKING",
            "agent": "codex",
            "project": "hyperion",
        })
        .to_string(),
    )
    .expect("status");

    let recovered = recover_orphaned_dispatch_runs(&server);
    assert_eq!(
        recovered,
        vec!["20260614T000000Z-codex-feedcafe".to_string()]
    );

    // The row must land in the NAMED project store, not the default global
    // one `test_server()` set up.
    let named_rows = server
        .with_named_project_store("hyperion", |store| {
            memcore::list_outcomes_by_vendor_window(
                store.connection(),
                "codex",
                "1970-01-01T00:00:00Z",
                None,
                memcore::OutcomeEvidenceClass::AnyAttribution,
            )
            .map_err(|e| e.to_string())
        })
        .expect("read named project outcomes");
    let row = named_rows
        .iter()
        .find(|r| r.dispatch_id == "20260614T000000Z-codex-feedcafe")
        .expect("recovered orphan outcome row present in the NAMED project store");
    assert_eq!(row.execution_outcome, "failed");
    assert_eq!(row.error_class.as_deref(), Some("recovered_orphan"));

    let global_rows = server
        .with_global_store_read(|store| {
            memcore::list_outcomes_by_vendor_window(
                store.connection(),
                "codex",
                "1970-01-01T00:00:00Z",
                None,
                memcore::OutcomeEvidenceClass::AnyAttribution,
            )
            .map_err(|e| e.to_string())
        })
        .expect("read global outcomes");
    assert!(
        !global_rows
            .iter()
            .any(|r| r.dispatch_id == "20260614T000000Z-codex-feedcafe"),
        "a named-project recovery must not ALSO land a row in the default \
         global store — that would split first-writer-wins across two stores"
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn global_dispatch_slot_blocks_duplicate_active_task_without_flow_id() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let first = reserve_global_dispatch_slot("same task without flow", "dispatch-one")
        .expect("first global reserve");
    let duplicate = reserve_global_dispatch_slot("same task without flow", "dispatch-two")
        .expect_err("duplicate active task should be blocked without flow_id");
    assert!(
        duplicate.contains("duplicate dispatch blocked"),
        "unexpected error: {duplicate}"
    );

    release_flow_dispatch_slot(Some(first));
    assert!(
        reserve_global_dispatch_slot("same task without flow", "dispatch-three").is_ok(),
        "slot should be reusable after release"
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn flow_dispatch_slot_blocks_duplicate_active_task() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let original_home = std::env::var_os("HOME");
    let original_tachi_home = std::env::var_os("TACHI_HOME");
    std::env::set_var("HOME", temp_home.path());
    std::env::remove_var("TACHI_HOME");

    let flow_id = format!(
        "flow_20260610T000000Z_duplicate_slot_{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(&flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow dir");

    let first = reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-one")
        .expect("first reserve")
        .expect("slot path");
    let duplicate = reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-two")
        .expect_err("duplicate active task should be blocked");
    assert!(
        duplicate.contains("duplicate dispatch blocked"),
        "unexpected error: {duplicate}"
    );

    release_flow_dispatch_slot(Some(first));
    assert!(
        reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-three")
            .expect("reserve after release")
            .is_some(),
        "slot should be reusable after release"
    );

    if let Some(value) = original_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(value) = original_tachi_home {
        std::env::set_var("TACHI_HOME", value);
    } else {
        std::env::remove_var("TACHI_HOME");
    }
}

#[test]
fn flow_dispatch_slot_reclaims_stale_lock_when_run_status_is_missing() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runs_root = tempfile::tempdir().expect("temp runs root");
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", runs_root.path());

    let flow_id = format!(
        "flow_20260610T000001Z_stale_slot_{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let task = "same task";
    let old_dispatch_id = "dispatch-stale-lock";
    let new_dispatch_id = "dispatch-new-lock";
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(&flow_id).expect("flow run dir");
    let lock_dir = run_dir.join(".dispatch-dedupe");
    std::fs::create_dir_all(&lock_dir).expect("create lock dir");
    let task_hash = crate::utils::stable_hash(task);
    let stale_created_at =
        (Utc::now() - chrono::Duration::seconds(DISPATCH_DEDUPE_STALE_LOCK_SECS + 1)).to_rfc3339();
    crate::utils::write_owner_only_file_atomic(
        &lock_dir.join(format!("{task_hash}.json")),
        serde_json::to_vec_pretty(&json!({
            "scope": "flow",
            "task_hash": task_hash,
            "dispatch_id": old_dispatch_id,
            "task": task,
            "flow_id": flow_id,
            "created_at": stale_created_at,
        }))
        .expect("serialize stale lock")
        .as_slice(),
    )
    .expect("write stale lock");

    let reserved = reserve_flow_dispatch_slot(Some(&flow_id), task, new_dispatch_id)
        .expect("stale lock should be reclaimed")
        .expect("slot path");
    let lock: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&reserved).expect("read lock"))
            .expect("parse lock");
    assert_eq!(lock["dispatch_id"], json!(new_dispatch_id));

    release_flow_dispatch_slot(Some(reserved));
    if let Some(value) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", value);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
