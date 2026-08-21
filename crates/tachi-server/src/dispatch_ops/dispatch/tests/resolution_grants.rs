use super::*;
use crate::test_support::EnvRestore;
use chrono::Utc;
use serde_json::{json, Value};

/// Releases the fake Claude subprocess even when an assertion panics. The
/// real handler owns generated MCP-config cleanup, so the explicit path
/// asserts cleanup while Drop only performs the bounded best-effort wait.
struct FakeClaudeCleanup {
    release: std::path::PathBuf,
    mcp_tmp_dir: std::path::PathBuf,
}

impl FakeClaudeCleanup {
    fn new(release: std::path::PathBuf, mcp_tmp_dir: std::path::PathBuf) -> Self {
        Self {
            release,
            mcp_tmp_dir,
        }
    }

    fn release_and_wait(&self) {
        std::fs::write(&self.release, b"release").expect("release fake claude");
        assert!(
            self.wait_for_cleanup(),
            "fake claude completion must clean the generated MCP config"
        );
    }

    fn wait_for_cleanup(&self) -> bool {
        for _ in 0..480 {
            let pending_mcp_config = std::fs::read_dir(&self.mcp_tmp_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.file_name().to_str().is_some_and(|name| {
                        name.starts_with("dispatch-") && name.ends_with("-mcp.json")
                    })
                });
            if !pending_mcp_config {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        false
    }
}

impl Drop for FakeClaudeCleanup {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, b"release");
        let _ = self.wait_for_cleanup();
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
    assert_assignment_legacy_projection(
        &params,
        &start.resolved_assignment,
        &start.resolved_profile,
    )
    .expect("typed assignment and temporary legacy projection agree");
    let mut profile_mutant = start.resolved_assignment.clone();
    profile_mutant.selected_profile = Some("mutant".to_string());
    assert!(
        assert_assignment_legacy_projection(&params, &profile_mutant, &start.resolved_profile)
            .is_err(),
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
fn projection_guards_reject_each_owned_field_family_mutant() {
    let server = crate::tests::make_server();
    let mut assignment_params = test_dispatch_params(Some("custom"), "assignment mutant matrix");
    let start = resolve_dispatch_start(
        &server,
        &mut assignment_params,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    )
    .expect("assignment baseline");
    let assignment = start.resolved_assignment.clone();
    macro_rules! assignment_mutant {
        ($name:literal, $body:expr) => {{
            let mut mutant = assignment.clone();
            $body(&mut mutant);
            assert!(
                assert_assignment_legacy_projection(
                    &assignment_params,
                    &mutant,
                    &start.resolved_profile
                )
                .is_err(),
                "assignment mutant must fail: {}",
                $name
            );
        }};
    }
    assignment_mutant!("reason", |m: &mut tachi_params::ResolvedStaffAssignment| {
        m.staffing_reason = tachi_params::TachiDispatchReason::DurableCrossSession
    });
    assignment_mutant!(
        "backend",
        |m: &mut tachi_params::ResolvedStaffAssignment| m.selected_backend = "mutant".to_string()
    );
    assignment_mutant!("worker", |m: &mut tachi_params::ResolvedStaffAssignment| {
        m.selected_worker = "mutant".to_string()
    });
    assignment_mutant!("model", |m: &mut tachi_params::ResolvedStaffAssignment| m
        .selected_model =
        Some("mutant".to_string()));
    assignment_mutant!(
        "execution",
        |m: &mut tachi_params::ResolvedStaffAssignment| m.execution_level =
            Some(tachi_params::ExecutionLevel::L2)
    );
    assignment_mutant!(
        "recommendation",
        |m: &mut tachi_params::ResolvedStaffAssignment| m.recommendation_ref =
            Some("mutant".to_string())
    );
    assignment_mutant!(
        "host_adapter",
        |m: &mut tachi_params::ResolvedStaffAssignment| m.host_adapter = Some("mutant".to_string())
    );
    assignment_mutant!(
        "evidence_required",
        |m: &mut tachi_params::ResolvedStaffAssignment| m
            .evidence_required
            .push("mutant".to_string())
    );
    assignment_mutant!(
        "fallback_chain",
        |m: &mut tachi_params::ResolvedStaffAssignment| m.fallback_chain.push("mutant".to_string())
    );
    assignment_mutant!(
        "route_explanation",
        |m: &mut tachi_params::ResolvedStaffAssignment| m
            .route_explanation
            .push("mutant".to_string())
    );
    assignment_mutant!(
        "identity_receipt",
        |m: &mut tachi_params::ResolvedStaffAssignment| m.identity_receipt =
            json!({"mutant": true})
    );

    let mut params = test_dispatch_params(Some("custom"), "grant mutant matrix");
    params.inject_tachi_mcp = Some(true);
    params.inject_hub_mcps = Some(true);
    params.allowed_mcp_servers = vec!["top".to_string()];
    params.allowed_tools = vec!["Read".to_string()];
    params.permission_profile = Some("default".to_string());
    params.sandbox = Some("workspace-write".to_string());
    params.max_turns = Some(3);
    params.timeout_secs = 4;
    params.credential_profiles = vec!["cred".to_string()];
    params.mcp_access = Some(tachi_params::DispatchMcpAccessParams {
        inject_tachi_mcp: Some(false),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["facade".to_string()],
        allowed_mcp_servers: vec!["nested".to_string()],
        github_read: Some(false),
        write_actions: Some(false),
        issue_refs: vec!["issue".to_string()],
        pr_refs: vec!["pr".to_string()],
        fallback: Some("fallback".to_string()),
    });
    let env = crate::exec_env_ops::EnvResolution::Unmanaged {
        cwd: "/tmp".to_string(),
    };
    let grant =
        mint_execution_grant(&mut params, "grant-mutant-matrix", &env).expect("grant baseline");
    macro_rules! grant_mutant {
        ($name:literal, $body:expr) => {{
            let mut mutant = grant.clone();
            $body(&mut mutant);
            assert!(
                assert_grant_legacy_projection(&params, &mutant, &env).is_err(),
                "grant mutant must fail: {}",
                $name
            );
        }};
    }
    grant_mutant!("env_id", |m: &mut tachi_params::ExecutionGrant| m.env_id =
        Some("mutant".to_string()));
    grant_mutant!(
        "credential_profiles",
        |m: &mut tachi_params::ExecutionGrant| m.credential_profiles.clear()
    );
    grant_mutant!("tools", |m: &mut tachi_params::ExecutionGrant| m
        .allowed_tools
        .push("Write".to_string()));
    grant_mutant!("sandbox", |m: &mut tachi_params::ExecutionGrant| m
        .sandbox =
        Some("read-only".to_string()));
    grant_mutant!("max_turns", |m: &mut tachi_params::ExecutionGrant| m
        .max_turns =
        Some(4));
    grant_mutant!("timeout", |m: &mut tachi_params::ExecutionGrant| m
        .timeout_secs =
        5);
    let mut top_tachi = params.clone();
    top_tachi.inject_tachi_mcp = Some(false);
    assert!(assert_grant_legacy_projection(&top_tachi, &grant, &env).is_err());
    let mut top_hub = params.clone();
    top_hub.inject_hub_mcps = Some(false);
    assert!(assert_grant_legacy_projection(&top_hub, &grant, &env).is_err());
    macro_rules! nested_mutant {
        ($body:expr) => {{
            let mut p = assignment_params.clone();
            $body(p.mcp_access.as_mut().unwrap());
            assert!(assert_nested_mcp_profile_projection(&p, &start.resolved_profile).is_err());
        }};
    }
    nested_mutant!(|m: &mut tachi_params::DispatchMcpAccessParams| m.inject_tachi_mcp = Some(true));
    nested_mutant!(|m: &mut tachi_params::DispatchMcpAccessParams| m.inject_hub_mcps = Some(true));
    nested_mutant!(|m: &mut tachi_params::DispatchMcpAccessParams| m
        .allowed_mcp_servers
        .push("x".to_string()));
    nested_mutant!(|m: &mut tachi_params::DispatchMcpAccessParams| m
        .allowed_facades
        .push("x".to_string()));
    nested_mutant!(|m: &mut tachi_params::DispatchMcpAccessParams| m.github_read = Some(true));
    nested_mutant!(|m: &mut tachi_params::DispatchMcpAccessParams| m.write_actions = Some(true));
    nested_mutant!(|m: &mut tachi_params::DispatchMcpAccessParams| m
        .issue_refs
        .push("x".to_string()));
    nested_mutant!(|m: &mut tachi_params::DispatchMcpAccessParams| m.pr_refs.push("x".to_string()));
    nested_mutant!(
        |m: &mut tachi_params::DispatchMcpAccessParams| m.fallback = Some("x".to_string())
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
    let cleanup =
        FakeClaudeCleanup::new(release.clone(), temp_home.path().join(".tachi").join("tmp"));
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
    for _ in 0..480 {
        if captured_config.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        captured_config.exists(),
        "fake launcher must capture the generated MCP config before evidence is read"
    );
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

    // Capture all real-handler evidence before releasing the fake subprocess;
    // this is also the explicit cleanup assertion, while Drop covers panic.
    cleanup.release_and_wait();

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
    params.cwd = Some("   ".to_string());
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
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status receipt exists"),
    )
    .expect("status receipt JSON");
    assert_eq!(
        status["cwd"],
        json!("   "),
        "status receipt retains base raw whitespace cwd while grant stays canonical: {status}"
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
    let mut empty_params = test_dispatch_params(Some("custom"), "empty nested MCP cardinality");
    empty_params.mcp_access = Some(tachi_params::DispatchMcpAccessParams {
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        allowed_facades: Vec::new(),
        allowed_mcp_servers: Vec::new(),
        github_read: None,
        write_actions: None,
        issue_refs: Vec::new(),
        pr_refs: Vec::new(),
        fallback: None,
    });
    let empty_grant = mint_execution_grant(
        &mut empty_params,
        "empty-nested-mcp",
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("Some(empty) nested MCP mints Some grant metadata");
    let mut empty_drop_mutant = empty_grant.clone();
    empty_drop_mutant.mcp_access = None;
    assert!(
        assert_grant_legacy_projection(
            &empty_params,
            &empty_drop_mutant,
            &crate::exec_env_ops::EnvResolution::Default,
        )
        .is_err(),
        "Some(empty) nested MCP must not be collapsed to None"
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

#[test]
fn valid_profile_resolves_conflicting_caller_hints_with_exact_legacy_projection() {
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(Some("claude"), "conflicting caller route hints");
    params.profile = Some("glm_impl".to_string());
    params.model = Some("zhipuai-coding-plan/glm-5.2@2026-07-13".to_string());

    let start = resolve_dispatch_start(
        &server,
        &mut params,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    )
    .expect("same-lineage model and explicit agent override resolve through the profile policy");

    assert_eq!(
        start.resolved_assignment.selected_profile.as_deref(),
        Some("glm_impl")
    );
    assert_eq!(start.resolved_assignment.selected_worker, "claude");
    assert_eq!(start.resolved_assignment.selected_backend, "claude");
    assert_eq!(
        start.resolved_assignment.selected_model.as_deref(),
        Some("zhipuai-coding-plan/glm-5.2@2026-07-13")
    );
    assert!(start
        .resolved_profile
        .route_explanation
        .iter()
        .any(|line| line.contains("explicit agent 'claude' overrides profile backend 'custom'")));
    assert!(start
        .resolved_profile
        .route_explanation
        .iter()
        .any(|line| line.contains("explicit model 'zhipuai-coding-plan/glm-5.2@2026-07-13'")));
    assert_assignment_legacy_projection(
        &params,
        &start.resolved_assignment,
        &start.resolved_profile,
    )
    .expect("policy-resolved typed assignment exactly projects to legacy ingress");
}

#[test]
fn host_authorization_default_and_refusal_guard_typed_resolution() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(Some("custom"), "host authorization default");

    let default_host = EnvRestore::remove(crate::host_profile::HOST_PROFILE_ENV);
    let (host, effective_level) = crate::host_profile::authorize_dispatch(params.execution_level)
        .expect("missing host profile authorizes the documented L1 default");
    assert_eq!(host, crate::host_profile::HostProfile::Development);
    assert_eq!(effective_level, tachi_params::ExecutionLevel::L1);
    let start = resolve_dispatch_start(&server, &mut params, Utc::now(), effective_level)
        .expect("only the actual host-authorized level reaches typed resolution");
    assert_eq!(
        start.resolved_assignment.execution_level,
        Some(tachi_params::ExecutionLevel::L1)
    );
    assert_eq!(
        params.execution_level,
        Some(tachi_params::ExecutionLevel::L1)
    );
    assert_assignment_legacy_projection(
        &params,
        &start.resolved_assignment,
        &start.resolved_profile,
    )
    .expect("host-default effective level exactly projects to legacy ingress");

    drop(default_host);
    let _release_host = EnvRestore::set(crate::host_profile::HOST_PROFILE_ENV, "release");
    let refusal = crate::host_profile::authorize_dispatch(Some(tachi_params::ExecutionLevel::L2))
        .expect_err("release host must refuse L2 rather than silently narrowing it");
    assert!(refusal.contains("host_profile_mismatch"), "{refusal}");
    assert!(refusal.contains("permits through L1"), "{refusal}");
}
