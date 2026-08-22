use super::*;
use crate::test_support::EnvRestore;
use chrono::Utc;
use serde_json::{json, Value};

#[test]
fn p3_downstream_production_consumers_cannot_reintroduce_flat_dispatch_params() {
    let rejects_flat_params = |source: &str| source.contains("TachiDispatchParams");
    let contains_identifier = |source: &str, identifier: &str| {
        source
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .any(|token| token == identifier)
    };
    let flat_fixture_free_helpers = [
        ("backend", include_str!("../backend.rs")),
        ("backend_failure", include_str!("../backend_failure.rs")),
        ("credential_apply", include_str!("../credential_apply.rs")),
        ("credentials", include_str!("../credentials.rs")),
        ("harness_preflight", include_str!("../harness_preflight.rs")),
        ("acpx_spec", include_str!("../../acpx/spec.rs")),
        (
            "acp_native_session",
            include_str!("../../acp_native/session.rs"),
        ),
    ];
    for (name, source) in flat_fixture_free_helpers {
        assert!(
            !rejects_flat_params(source),
            "{name} must consume request, assignment, grant, or private adapter mechanics, never TachiDispatchParams"
        );
        assert!(
            rejects_flat_params(&format!("{source}\nTachiDispatchParams deliberate_mutant;")),
            "{name} checker must reject a deliberate forbidden-type mutant"
        );
    }

    let production_adapter = |name: &str, source: &str, start: &str, end: &str| {
        assert!(
            source.contains(start),
            "{name} source must contain production start {start:?}"
        );
        let source = source
            .split_once(end)
            .unwrap_or_else(|| panic!("{name} source must contain production end {end:?}"))
            .0;
        assert!(
            !rejects_flat_params(source),
            "{name} production adapter must consume typed request/assignment/grant, never flat params"
        );
        assert!(
            rejects_flat_params(&format!("{source}\nTachiDispatchParams deliberate_mutant;")),
            "{name} production-adapter checker must reject a deliberate forbidden-type mutant"
        );
    };
    production_adapter(
        "launcher",
        include_str!("../../launcher.rs"),
        "fn launch_params",
        "#[cfg(test)]\nmod tests",
    );
    production_adapter(
        "acp_native_spec",
        include_str!("../../acp_native/spec.rs"),
        "pub(in crate::dispatch_ops) fn build_native_acp_run_spec",
        "#[cfg(test)]\nmod tests",
    );
    let authority = include_str!("../authority.rs");
    for deleted_projection in [
        "assert_grant_legacy_projection",
        "apply_grant_legacy_projection",
    ] {
        assert!(
            !authority.contains(deleted_projection),
            "authority imports and production region must not restore {deleted_projection}"
        );
        assert!(
            format!("{authority}\n{deleted_projection}();").contains(deleted_projection),
            "authority source gate must reject deliberate {deleted_projection} mutant"
        );
    }
    production_adapter(
        "prompt_production_assembly",
        include_str!("../../prompt.rs"),
        "pub(crate) async fn assemble_resolved_prompt_with_trace",
        "#[cfg(test)]\npub(crate) async fn assemble_prompt_with_trace",
    );

    let dispatch = include_str!("../../dispatch.rs");
    let handler = dispatch
        .split_once("pub(crate) async fn handle_tachi_dispatch")
        .expect("dispatch source contains the real handler")
        .1;
    let after_grant = handler
        .split_once("let execution_grant = mint_execution_grant(")
        .expect("handler contains the grant mint marker")
        .1;
    let post_grant_handler = after_grant;
    let has_post_grant_params = |source: &str| {
        contains_identifier(source, "params") || rejects_flat_params(source)
    };
    assert!(
        !has_post_grant_params(post_grant_handler),
        "post-grant handler code must read assignment/grant, never TachiDispatchParams"
    );
    assert!(
        has_post_grant_params(&format!("{post_grant_handler}\nconsume(&params);")),
        "source gate itself must fail when a post-grant params read is introduced"
    );
}

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

    async fn release_and_wait(&self) {
        std::fs::write(&self.release, b"release").expect("release fake claude");
        let cleaned = tokio::time::timeout(std::time::Duration::from_secs(12), async {
            loop {
                if !self.pending_mcp_config() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .is_ok();
        assert!(
            cleaned,
            "fake claude completion must clean the generated MCP config"
        );
    }

    fn pending_mcp_config(&self) -> bool {
        std::fs::read_dir(&self.mcp_tmp_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with("dispatch-") && name.ends_with("-mcp.json")
                })
            })
    }

    fn wait_for_cleanup_on_drop(&self) {
        for _ in 0..480 {
            if !self.pending_mcp_config() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

impl Drop for FakeClaudeCleanup {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, b"release");
        self.wait_for_cleanup_on_drop();
    }
}

/// A custom worker may outlive a failed assertion. Keep the run directory and
/// process-wide test environment alive until its terminal receipt exists.
struct TerminalWorkerCleanup {
    run_dir: std::path::PathBuf,
}

impl TerminalWorkerCleanup {
    fn new(run_dir: std::path::PathBuf) -> Self {
        Self { run_dir }
    }

    fn terminal(&self) -> bool {
        std::fs::read_to_string(self.run_dir.join("status.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .is_some_and(|status| {
                status["result_written"] == json!(true)
                    && matches!(
                        status["state"].as_str(),
                        Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED")
                    )
            })
    }

    async fn wait_for_terminal(&self) -> Value {
        for _ in 0..120 {
            if let Ok(raw) = tokio::fs::read_to_string(self.run_dir.join("status.json")).await {
                if let Ok(status) = serde_json::from_str::<Value>(&raw) {
                    if status["result_written"] == json!(true)
                        && matches!(
                            status["state"].as_str(),
                            Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED")
                        )
                    {
                        return status;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("custom worker must write a terminal status receipt");
    }
}

impl Drop for TerminalWorkerCleanup {
    fn drop(&mut self) {
        for _ in 0..480 {
            if self.terminal() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
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
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; open('launcher-cwd', 'w').write(os.getcwd())".to_string(),
    ];
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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn opencode_builder_profile_default_reaches_credential_failure_evidence() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(None, "profile-only credential evidence");
    params.profile = Some("opencode_builder".to_string());
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; open('launcher-cwd', 'w').write(os.getcwd())".to_string(),
    ];

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("profile default credential is materialized and reports its selector");
    assert!(err.contains("opencode_shared"), "{err}");
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
        json!(["opencode_shared"]),
        "{event}"
    );
    assert_eq!(
        event["host_adapter"],
        json!("opencode"),
        "credential failure must report the admitted assignment host adapter: {event}"
    );
}

#[test]
fn execution_grant_records_admitted_owner_fields_independently_of_raw_ingress() {
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
    let expected = tachi_params::ExecutionGrant {
        grant_id: "dispatch-grant".to_string(),
        env_id: None,
        unmanaged_cwd_allowed: true,
        allowed_cwd: Some(std::path::PathBuf::from("/workspace/tachi")),
        credential_profiles: vec!["dispatch-token".to_string()],
        mcp_access: None,
        allowed_tools: vec!["Read".to_string(), "Write".to_string()],
        permission_profile: Some("allowlist".to_string()),
        sandbox: Some("workspace-write".to_string()),
        max_turns: Some(7),
        timeout_secs: 42,
    };
    assert_eq!(grant, expected, "P1/P2 grant baseline is literal typed authority");
    assert_eq!(grant.env_id, None, "unmanaged authority has no lease id");
    assert!(grant.unmanaged_cwd_allowed);
    assert_eq!(
        grant.allowed_cwd.as_deref(),
        Some(std::path::Path::new("/workspace/tachi"))
    );
    assert_eq!(grant.credential_profiles, vec!["dispatch-token"]);
    assert_eq!(grant.allowed_tools, vec!["Read", "Write"]);
    assert_eq!(grant.permission_profile.as_deref(), Some("allowlist"));
    assert_eq!(grant.sandbox.as_deref(), Some("workspace-write"));
    assert_eq!(grant.max_turns, Some(7));
    assert_eq!(grant.timeout_secs, 42);

    macro_rules! grant_mutant {
        ($name:literal, $body:expr) => {{
            let mut mutant = expected.clone();
            $body(&mut mutant);
            assert_ne!(mutant, expected, "P1/P2 grant mutant must fail: {}", $name);
        }};
    }
    grant_mutant!("env_id", |m: &mut tachi_params::ExecutionGrant| m.env_id =
        Some("mutant".to_string()));
    grant_mutant!("unmanaged_cwd", |m: &mut tachi_params::ExecutionGrant| m
        .unmanaged_cwd_allowed = false);
    grant_mutant!("allowed_cwd", |m: &mut tachi_params::ExecutionGrant| m.allowed_cwd = None);
    grant_mutant!("credential_profiles", |m: &mut tachi_params::ExecutionGrant| m
        .credential_profiles
        .clear());
    grant_mutant!("mcp_access", |m: &mut tachi_params::ExecutionGrant| m.mcp_access =
        Some(tachi_params::DispatchMcpAccessParams {
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            allowed_facades: Vec::new(),
            allowed_mcp_servers: Vec::new(),
            github_read: None,
            write_actions: None,
            issue_refs: Vec::new(),
            pr_refs: Vec::new(),
            fallback: None,
        }));
    grant_mutant!("allowed_tools", |m: &mut tachi_params::ExecutionGrant| m
        .allowed_tools
        .push("Execute".to_string()));
    grant_mutant!("permission_profile", |m: &mut tachi_params::ExecutionGrant| m
        .permission_profile = Some("default".to_string()));
    grant_mutant!("sandbox", |m: &mut tachi_params::ExecutionGrant| m.sandbox =
        Some("read-only".to_string()));
    grant_mutant!("max_turns", |m: &mut tachi_params::ExecutionGrant| m.max_turns = Some(8));
    grant_mutant!("timeout_secs", |m: &mut tachi_params::ExecutionGrant| m.timeout_secs = 43);
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
    assert_eq!(
        start.request.stage.as_deref(),
        Some("execute"),
        "a named profile's omitted stage must reach the typed request before default skills and prompt bytes are derived"
    );
    assert_eq!(
        start.request.profile.as_deref(),
        Some("glm_51_impl"),
        "the pre-projection request keeps the caller's raw alias for diagnostics and replay"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn raw_profile_alias_keeps_completion_diagnostics_raw_while_assignment_is_canonical() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let cwd = tempfile::tempdir().expect("dispatch cwd");
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(None, "raw alias completion diagnostics");
    params.profile = Some("glm_51_impl".to_string());
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "from pathlib import Path; Path('launcher-cwd').write_text(str(Path.cwd()))".to_string(),
    ];
    params.cwd = Some(cwd.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    params.verbose = Some(true);

    let mut resolved_params = params.clone();
    let start = resolve_dispatch_start(
        &server,
        &mut resolved_params,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    )
    .expect("raw alias resolves before the compatibility projection");
    assert_eq!(start.request.profile.as_deref(), Some("glm_51_impl"));
    assert_eq!(
        start.resolved_assignment.selected_profile.as_deref(),
        Some("glm_impl")
    );
    assert_eq!(resolved_params.profile.as_deref(), Some("glm_impl"));

    let raw = handle_tachi_dispatch(&server, params)
        .await
        .expect("alias dispatch starts");
    let response: Value = serde_json::from_str(&raw).expect("response JSON");
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run dir"));
    let cleanup = TerminalWorkerCleanup::new(run_dir.clone());
    assert_eq!(
        response["selected_profile"],
        json!("glm_impl"),
        "{response}"
    );
    let started: Value = std::fs::read_to_string(run_dir.join("trajectory.jsonl"))
        .expect("trajectory")
        .lines()
        .map(|line| serde_json::from_str(line).expect("event JSON"))
        .find(|event: &Value| event["event"] == "dispatch_started")
        .expect("dispatch_started receipt");
    assert_eq!(started["profile"], json!("glm_51_impl"), "{started}");
    let completion = &response["suggested_complete_command"]["arguments"];
    assert_eq!(completion["profile"], json!("glm_51_impl"), "{completion}");
    assert_eq!(
        response["profile"]["identity_receipt"]["requested"]["profile"],
        json!("glm_51_impl"),
        "verbose raw diagnostics must retain the caller spelling: {response}"
    );
    let terminal_status = cleanup.wait_for_terminal().await;
    assert_eq!(terminal_status["state"], json!("TASK_STATE_COMPLETED"));
    assert_eq!(
        std::fs::canonicalize(
            std::fs::read_to_string(cwd.path().join("launcher-cwd"))
                .expect("launcher cwd record")
                .trim()
        )
        .expect("launched cwd canonicalizes"),
        std::fs::canonicalize(cwd.path()).expect("requested cwd canonicalizes"),
        "custom worker must observe the resolved unmanaged cwd"
    );
    drop(cleanup);
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
fn canonical_ingress_refuses_cross_lineage_model_without_partial_legacy_projection() {
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(Some("claude"), "cross-lineage profile refusal");
    params.profile = Some("glm_impl".to_string());
    params.model = Some("openai/gpt-5.6".to_string());
    let legacy_before = format!("{params:?}");

    let err = match resolve_dispatch_start(
        &server,
        &mut params,
        Utc::now(),
        tachi_params::ExecutionLevel::L1,
    ) {
        Ok(_) => {
            panic!("cross-lineage caller model must be refused before assignment or projection")
        }
        Err(err) => err,
    };

    assert!(
        err.contains("model override 'openai/gpt-5.6' crosses profile 'glm_impl' lineage"),
        "{err}"
    );
    assert!(
        err.contains("without explicit profile authorization"),
        "{err}"
    );
    assert_eq!(
        format!("{params:?}"),
        legacy_before,
        "canonical ingress must not partially apply profile resolution to the legacy projection"
    );
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
        !grant
            .mcp_access
            .as_ref()
            .expect("grant keeps admitted MCP authority")
            .allowed_mcp_servers
            .contains(&"mutant".to_string()),
        "grant remains immutable when raw launch input mutates"
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

    let grant = mint_execution_grant(
        &mut params,
        "profile-payload-grant",
        &crate::exec_env_ops::EnvResolution::Default,
    )
    .expect("launch authority mints");
    let mcp = grant.mcp_access.expect("launch MCP authority");
    assert_eq!(
        mcp.inject_tachi_mcp,
        Some(true),
        "launch input keeps top-level true"
    );
    assert_eq!(
        mcp.inject_hub_mcps,
        Some(false),
        "launch input keeps top-level false"
    );
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
        assignment: &start.resolved_assignment,
        profile_payload: &start.profile_payload,
        resolved_profile: &start.resolved_profile,
        authority: &authority,
        credential_reports_json: &empty_reports,
        capability_bundle_card: &empty_value,
        capability_bundle_file: "",
        feedback_rules_trace: &empty_value,
        harness_transport: "cli",
        harness_server_url: &no_server_url,
        execution_backend_name: None,
        execution_backend_metadata: &no_backend_metadata,
        acpx_enabled: false,
        native_acp_enabled: false,
        v2: false,
        plan_duration_ms: None,
        request: &start.request,
        verbose: false,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    cleanup.release_and_wait().await;
    let worker_result = wait_for_result(&run_dir).await;

    assert!(
        !worker_result.trim().is_empty(),
        "fake Claude must reach a terminal result before the test returns"
    );
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
    let observed_cwd = temp_home.path().join("whitespace-launcher-cwd");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        format!("import os; open({observed_cwd:?}, 'w').write(os.getcwd())"),
    ];

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
    let cleanup = TerminalWorkerCleanup::new(run_dir.clone());
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
    let terminal_status = cleanup.wait_for_terminal().await;
    assert_eq!(terminal_status["state"], json!("TASK_STATE_COMPLETED"));
    let launched_cwd =
        std::fs::read_to_string(&observed_cwd).expect("whitespace launcher cwd record");
    assert_eq!(
        std::fs::canonicalize(launched_cwd.trim()).expect("default launcher cwd canonicalizes"),
        std::fs::canonicalize(std::env::current_dir().expect("test cwd exists"))
            .expect("test cwd canonicalizes"),
        "whitespace raw receipt must not become the launcher's actual default cwd"
    );
    drop(cleanup);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn managed_env_status_cwd_uses_the_authoritative_lease_path() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let managed_cwd = tempfile::tempdir().expect("managed cwd");
    let server = crate::tests::make_server();
    server
        .with_global_store(|store| {
            memcore::insert_exec_env(
                store.connection(),
                &memcore::NewExecEnvLease {
                    env_id: "env-status-cwd".to_string(),
                    kind: "worktree".to_string(),
                    path: managed_cwd.path().to_string_lossy().to_string(),
                    repo_root: "/repo".to_string(),
                    branch: "tachi/1819/status-cwd".to_string(),
                    base_sha: "base".to_string(),
                    dispatch_id: None,
                    env_class: memcore::EnvClass::EditOnly,
                    created_at: String::new(),
                },
            )
            .map_err(|error| error.to_string())
        })
        .expect("seed managed execution environment");
    let mut params = test_dispatch_params(Some("custom"), "managed status cwd");
    params.env_id = Some("env-status-cwd".to_string());
    params.command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "pwd > launcher-cwd".to_string(),
    ];

    let raw = handle_tachi_dispatch(&server, params)
        .await
        .expect("managed dispatch starts");
    let response: Value = serde_json::from_str(&raw).expect("response JSON");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(response["run_dir"].as_str().expect("run dir"))
                .join("status.json"),
        )
        .expect("status receipt"),
    )
    .expect("status JSON");
    assert_eq!(
        status["cwd"],
        json!(managed_cwd.path().to_string_lossy()),
        "managed lease path, not raw caller spelling, is receipt authority: {status}"
    );
    let run_dir = std::path::PathBuf::from(response["run_dir"].as_str().expect("run dir"));
    let _worker_result = wait_for_result(&run_dir).await;
    let terminal_status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("terminal status receipt"),
    )
    .expect("terminal status JSON");
    assert!(
        terminal_status["result_written"] == json!(true)
            && matches!(
                terminal_status["state"].as_str(),
                Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED")
            ),
        "managed dispatch must reach a terminal worker/watchdog status before temp cleanup: {terminal_status}"
    );
    let launched_cwd = std::fs::read_to_string(managed_cwd.path().join("launcher-cwd"))
        .expect("launcher records its actual working directory");
    assert_eq!(
        std::fs::canonicalize(launched_cwd.trim()).expect("launched cwd canonicalizes"),
        std::fs::canonicalize(managed_cwd.path()).expect("managed cwd canonicalizes"),
        "the launched process must run in the managed lease cwd"
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
    assert!(
        grant.mcp_access.is_some(),
        "grant preserves MCP cardinality"
    );
    let mut dropped_mcp = grant.clone();
    dropped_mcp.mcp_access = None;
    assert!(dropped_mcp.mcp_access.is_none());
    assert!(
        grant.mcp_access.is_some(),
        "MCP-drop mutant changes the owner field"
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
    assert!(empty_grant.mcp_access.is_some());
    assert!(empty_drop_mutant.mcp_access.is_none());
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
    assert_eq!(managed_env_mutant.allowed_cwd, None);
    assert_eq!(
        grant.allowed_cwd.as_deref(),
        Some(std::path::Path::new("/canonical/worktree"))
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
    assert!(default_env_mutant.unmanaged_cwd_allowed);
    assert!(!grant.unmanaged_cwd_allowed);
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
        &omitted_start.request,
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
        &verify_start.request,
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
    let mut full_mutant = grant.clone();
    full_mutant.permission_profile = Some("full".to_string());
    assert_eq!(full_mutant.permission_profile.as_deref(), Some("full"));
    assert_eq!(grant.permission_profile.as_deref(), Some("verify"));
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
