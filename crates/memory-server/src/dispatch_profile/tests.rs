use super::*;
use std::io::{Read, Write};

const OPENCODE_DOC_FIXTURE: &str = r#"{
    "openapi":"3.1.0",
    "info":{"title":"opencode","version":"1.0.0"},
    "paths":{
        "/api/session":{"post":{}},
        "/api/session/{sessionID}/prompt":{"post":{}},
        "/api/session/{sessionID}/wait":{"post":{}},
        "/api/model":{"get":{}},
        "/api/provider":{"get":{}}
    }
}"#;

#[test]
fn route_policy_simulation_sinks_non_finite_scores() {
    let rows = vec![
        AgentPerformanceMatrixRow {
            scope: "leader".to_string(),
            profile: Some("claude_plan".to_string()),
            role: Some("planner".to_string()),
            agent: "claude".to_string(),
            task_type: "review_request".to_string(),
            samples: 10,
            success_rate: Some(0.95),
            verification_rate: 1.0,
            avg_quality_score: Some(f64::NAN),
            ..Default::default()
        },
        AgentPerformanceMatrixRow {
            scope: "leader".to_string(),
            profile: Some("codex_55_review".to_string()),
            role: Some("reviewer".to_string()),
            agent: "codex".to_string(),
            task_type: "review_request".to_string(),
            samples: 10,
            success_rate: Some(0.90),
            verification_rate: 1.0,
            avg_quality_score: Some(0.90),
            ..Default::default()
        },
    ];

    let summary = simulate_route_policy("quality_first", &rows, None);

    assert_eq!(summary.route_choices.len(), 1);
    assert_eq!(summary.route_choices[0].profile, "codex_55_review");
    assert!(summary.route_choices[0].score.is_finite());
}

#[test]
fn compare_scores_desc_keeps_non_finite_scores_last() {
    let mut scores = [
        ("nan", f64::NAN),
        ("best", 42.0),
        ("worst_finite", -1.0),
        ("positive_inf", f64::INFINITY),
        ("negative_inf", f64::NEG_INFINITY),
    ];

    scores.sort_by(|a, b| compare_scores_desc(a.1, b.1).then(a.0.cmp(b.0)));

    assert_eq!(scores[0], ("best", 42.0));
    assert_eq!(scores[1], ("worst_finite", -1.0));
    assert!(scores[2..].iter().all(|(_, score)| !score.is_finite()));
}

fn params() -> TachiDispatchParams {
    TachiDispatchParams {
        agent: None,
        profile: Some("claude_plan".to_string()),
        task: "Plan issue #194".to_string(),
        cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: 5,
        permission_profile: None,
        allowed_tools: Vec::new(),
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
        issue_ref: Some("kckylechen1/tachi#194".to_string()),
        pr_ref: None,
        flow_id: Some("flow-194".to_string()),
        tool_profile: None,
        auto_capability_bundle: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
    }
}

fn spawn_probe_server() -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept probe");
            let mut buf = [0_u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let body = if request.starts_with("GET /doc ") {
                OPENCODE_DOC_FIXTURE
            } else {
                "<title>OpenCode</title>"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

struct EnvRestore {
    key: &'static str,
    old: Option<String>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        if let Some(old) = &self.old {
            std::env::set_var(self.key, old);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn dispatch_profile_selects_backend_and_mcp_contract() {
    let mut params = params();
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(params.agent.as_deref(), Some("claude"));
    assert_eq!(params.stage.as_deref(), Some("plan"));
    assert_eq!(params.tool_profile.as_deref(), Some("delegate"));
    assert_eq!(params.inject_tachi_mcp, Some(true));
    assert_eq!(resolved.selected_profile.as_deref(), Some("claude_plan"));
    assert_eq!(resolved.mcp_access.github_read, Some(true));
    assert_eq!(
        resolved.mcp_access.issue_refs,
        vec!["kckylechen1/tachi#194".to_string()]
    );
    assert!(resolved.auto_capability_bundle);
    assert!(params
        .skills
        .iter()
        .any(|skill| skill == SUPERPOWER_WRITING_PLANS));
    assert!(params
        .skills
        .iter()
        .any(|skill| skill == CODING_ARCHITECTURE_DECISION));
    assert_eq!(
        profile_skill_loadout_json(resolve_dispatch_profile("claude_plan").unwrap())
            ["passive_traits"][0],
        json!("plan_before_execute")
    );
    let profile_payload = profile_json(resolve_dispatch_profile("claude_plan").unwrap());
    assert_eq!(profile_payload["card_archetype"], json!("raven"));
    assert_eq!(profile_payload["mbit_card"]["archetype"], json!("raven"));
    assert_eq!(
        profile_payload["mbit_card"]["authority"]["write_code"],
        json!(false)
    );
    assert_eq!(
        profile_payload["mbit_card"]["guidance"]["superpowers"][0],
        json!(SUPERPOWER_WRITING_PLANS)
    );
    assert!(profile_payload["mbit_card"]["moves"]["waza"]
        .as_array()
        .expect("waza moves")
        .contains(&json!(WAZA_THINK)));
    assert_eq!(
        profile_payload["mbit_card"]["personality"]["risk_control"],
        profile_payload["mbit_card"]["stats"]["risk_control"]
    );
    assert_eq!(
        profile_json(resolve_dispatch_profile("glm_51_impl").unwrap())["mbit_card"]["archetype"],
        json!("scv")
    );
    assert_eq!(
        profile_json(resolve_dispatch_profile("deepseek_explore").unwrap())["mbit_card"]
            ["archetype"],
        json!("poke")
    );
}

#[test]
fn dispatch_profile_treats_blank_agent_as_missing() {
    let mut params = params();
    params.agent = Some("   ".to_string());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(params.agent.as_deref(), Some("claude"));
    assert_eq!(resolved.agent, "claude");
}

#[test]
fn credentialed_dispatch_profile_applies_default_credential_profiles() {
    let mut params = params();
    params.profile = Some("opencode_builder".to_string());
    params.issue_ref = None;
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(params.agent.as_deref(), Some("custom"));
    assert_eq!(params.stage.as_deref(), Some("execute"));
    assert_eq!(params.credential_profiles, vec!["opencode_shared"]);
    assert_eq!(
        resolved.credential_profiles,
        vec!["opencode_shared".to_string()]
    );
    assert_eq!(
        profile_json(resolve_dispatch_profile("opencode_builder").unwrap())["credential_profiles"]
            [0],
        json!("opencode_shared")
    );
}

#[test]
fn dispatch_profile_merges_default_and_explicit_credential_profiles() {
    let mut params = params();
    params.profile = Some("opencode_builder".to_string());
    params.credential_profiles = vec!["extra_project_secret".to_string()];
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(
        params.credential_profiles,
        vec!["extra_project_secret", "opencode_shared"]
    );
    assert_eq!(
        resolved.credential_profiles,
        vec![
            "extra_project_secret".to_string(),
            "opencode_shared".to_string()
        ]
    );
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("profile requires credential profile(s): opencode_shared")));
}

#[test]
fn dispatch_profile_writes_fallback_mcp_access_back_to_params() {
    let mut params = params();
    params.profile = None;
    params.agent = Some("claude".to_string());
    params.inject_tachi_mcp = Some(true);
    params.allowed_mcp_servers = vec!["github".to_string()];
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

    assert_eq!(
        params
            .mcp_access
            .as_ref()
            .map(|access| access.allowed_mcp_servers.as_slice()),
        Some(&["github".to_string()][..])
    );
    assert_eq!(
        resolved.mcp_access.allowed_mcp_servers,
        vec!["github".to_string()]
    );
}

#[test]
fn explicit_agent_can_override_profile_backend() {
    let mut params = params();
    params.agent = Some("codex".to_string());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    assert_eq!(resolved.agent, "codex");
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("overrides profile backend")));
}

#[test]
fn custom_profile_populates_opencode_command() {
    let mut params = params();
    params.profile = Some("deepseek_explore".to_string());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

    assert_eq!(resolved.agent, "custom");
    assert_eq!(
        params.command,
        vec![
            "opencode".to_string(),
            "--pure".to_string(),
            "run".to_string(),
            "--model".to_string(),
            "deepseek/deepseek-v4-flash".to_string()
        ]
    );
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("opencode custom command")));
}

#[test]
fn recommendation_transport_reports_opencode_serve_fallback() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind unused port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    let _transport = EnvRestore::set("TACHI_OPENCODE_TRANSPORT", "serve");
    let _server_url = EnvRestore::set(
        "TACHI_OPENCODE_SERVER_URL",
        &format!("http://127.0.0.1:{port}"),
    );

    let profile = resolve_dispatch_profile("opencode_builder").unwrap();
    let (transport, readiness) = recommended_transport_for_profile(profile);

    assert_eq!(transport, "opencode_cli");
    assert_eq!(readiness["requested"], json!("opencode_serve"));
    assert_eq!(readiness["fallback"], json!("opencode_cli"));
    assert_eq!(
        readiness["harness_server_status"]["reachable"],
        json!(false)
    );
}

#[test]
fn custom_profile_can_attach_to_opencode_serve() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _password = EnvRestore::set("OPENCODE_SERVER_PASSWORD", "test-password");
    let (server_url, server) = spawn_probe_server();
    let mut params = params();
    params.profile = Some("deepseek_explore".to_string());
    params.cwd = Some("/tmp/tachi-opencode-project".to_string());
    params.harness_transport = Some("opencode_serve".to_string());
    params.harness_server_url = Some(server_url.clone());
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();
    server.join().expect("probe server thread");

    assert_eq!(resolved.agent, "custom");
    assert_eq!(
        params.command,
        vec![
            "opencode".to_string(),
            "run".to_string(),
            "--attach".to_string(),
            server_url,
            "--dir".to_string(),
            "/tmp/tachi-opencode-project".to_string(),
            "--agent".to_string(),
            "explore".to_string(),
            "--model".to_string(),
            "deepseek/deepseek-v4-flash".to_string()
        ]
    );
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("opencode serve transport")));
}

#[test]
fn custom_profile_falls_back_to_cli_when_opencode_serve_is_unreachable() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind unused port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);

    let mut params = params();
    params.profile = Some("deepseek_explore".to_string());
    params.harness_transport = Some("opencode_serve".to_string());
    params.harness_server_url = Some(format!("http://127.0.0.1:{port}"));
    let resolved = resolve_and_apply_dispatch_profile(&mut params).unwrap();

    assert_eq!(resolved.agent, "custom");
    assert_eq!(params.harness_transport.as_deref(), Some("opencode_cli"));
    assert_eq!(
        params.command,
        vec![
            "opencode".to_string(),
            "--pure".to_string(),
            "run".to_string(),
            "--model".to_string(),
            "deepseek/deepseek-v4-flash".to_string()
        ]
    );
    assert!(resolved
        .route_explanation
        .iter()
        .any(|line| line.contains("falling back to opencode CLI")));
}

#[test]
fn kimi_ux_profile_is_registered_as_read_only_experience_reviewer() {
    let profile = resolve_dispatch_profile("kimi_ux").expect("kimi_ux profile");
    assert_eq!(profile.backend, "kimi");
    assert_eq!(profile.role, "ux_researcher");
    assert!(!profile.write_actions);
    assert!(profile
        .evidence_required
        .iter()
        .any(|item| item == &"ux_findings"));
    assert!(profile
        .strong_against
        .iter()
        .any(|item| item == &"tool_surface_friction"));
}

#[test]
fn risk_classifier_uses_touched_area_and_missing_verification_signals() {
    let risk = classify_dispatch_risk(
        "review changes in crates/memory-server/src/agent_eval.rs and dispatch_profile.rs; tests not run",
        None,
        &[],
    );

    assert_eq!(risk.risk, "high");
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "touched_area:eval_ledger_changes"));
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "touched_area:dispatch_refactor"));
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "missing_verification_signal"));
    assert!(risk
        .required_profiles
        .iter()
        .any(|profile| profile == "codex_55_review"));
    assert!(risk
        .blocked_profiles
        .iter()
        .any(|profile| profile == "codex_53_fast"));
}

#[test]
fn risk_classifier_preserves_low_risk_research_route() {
    let risk = classify_dispatch_risk("research low-risk documentation wording", None, &[]);

    assert_eq!(risk.risk, "low");
    assert!(risk.blocked_profiles.is_empty());
}

#[test]
fn risk_classifier_marks_regression_hints_high() {
    let risk = classify_dispatch_risk(
        "fix a regression where the worker got stuck in a retry loop",
        None,
        &[],
    );

    assert_eq!(risk.risk, "high");
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "prior_failure_or_regression_hint"));
}

#[test]
fn risk_classifier_does_not_treat_plain_override_or_profile_as_failure() {
    let override_risk = classify_dispatch_risk(
        "document the config override behavior for normal settings",
        None,
        &[],
    );
    assert_ne!(override_risk.risk, "high");
    assert!(!override_risk
        .reasons
        .iter()
        .any(|reason| reason == "prior_failure_or_regression_hint"));

    let profile_risk = classify_dispatch_risk("review user profile page wording", None, &[]);
    assert_ne!(profile_risk.risk, "high");
}

#[test]
fn risk_classifier_escalates_on_sensitive_file_paths() {
    let paths = vec![
        "docs/notes.md".to_string(),
        "crates/memory-server/src/vault_crypto.rs".to_string(),
    ];
    let risk = classify_dispatch_risk("plan a small docs update", None, &paths);

    assert_eq!(risk.risk, "high");
    assert!(risk
        .reasons
        .iter()
        .any(|reason| reason == "touches vault/secrets boundary"));
}

#[test]
fn risk_classifier_dedupes_text_and_path_signals() {
    let paths = vec!["crates/memory-server/src/dispatch_profile.rs".to_string()];
    let risk = classify_dispatch_risk("review dispatch profile changes", None, &paths);
    let count = risk
        .reasons
        .iter()
        .filter(|reason| *reason == "touches dispatch routing")
        .count();

    assert_eq!(risk.risk, "high");
    assert_eq!(count, 1);
}

fn scoring_test_server() -> MemoryServer {
    let db_path = std::env::temp_dir().join(format!(
        "dispatch-scoring-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db_path, None).expect("test memory server")
}

fn failed_eval_row(profile: &str) -> EvalRow {
    EvalRow {
        agent: "custom".to_string(),
        profile: Some(profile.to_string()),
        model: None,
        mode: None,
        task_type: crate::agent_eval::TaskType::FixRequest,
        turns: 0,
        tool_calls: 0,
        verification_present: false,
        failure_mode: Some("blocked".to_string()),
        completion_status: CompletionStatus::Blocked,
        cost_usd: None,
        cost_tokens: None,
        quality_score: None,
        latency_ms: None,
        subagents: Vec::new(),
    }
}

fn verified_eval_row(profile: &str, task_type: crate::agent_eval::TaskType) -> EvalRow {
    EvalRow {
        completion_status: CompletionStatus::Completed,
        verification_present: true,
        failure_mode: None,
        task_type,
        ..failed_eval_row(profile)
    }
}

#[test]
fn score_profile_candidate_keeps_role_correct_profile_above_role_wrong_competitor() {
    let server = scoring_test_server();

    // A fix_request: the executor role is correct, the senior reviewer is not.
    // The lone signal (dispatch_refactor) is something the reviewer is
    // strong_against but the executor is neither strong nor weak against, so
    // it isolates the eval-failure penalty as the only differentiator.
    let risk = DispatchRisk {
        task_type: "fix_request".to_string(),
        risk: "low".to_string(),
        reasons: vec!["touched_area:dispatch_refactor".to_string()],
        required_profiles: Vec::new(),
        blocked_profiles: Vec::new(),
    };

    let executor = resolve_dispatch_profile("glm_51_impl").expect("executor profile");
    let competitor = resolve_dispatch_profile("codex_55_review").expect("reviewer profile");

    // Sparse failures (3) for the role-correct executor. Under the old bare
    // -8.0-per-row penalty these alone removed -24, sinking the role bonus
    // below the role-wrong competitor; the bounded/weighted path must not.
    let rows = vec![
        failed_eval_row(executor.name),
        failed_eval_row(executor.name),
        failed_eval_row(executor.name),
    ];

    let executor_candidate =
        score_profile_candidate(&server, executor, &risk, &rows, &[], &[]).expect("executor");
    let competitor_candidate =
        score_profile_candidate(&server, competitor, &risk, &[], &[], &[]).expect("competitor");

    assert_eq!(executor_candidate.failure_count, 3);
    assert!(
        executor_candidate.score > competitor_candidate.score,
        "role-correct executor ({}) must stay above role-wrong competitor ({})",
        executor_candidate.score,
        competitor_candidate.score
    );
}

#[test]
fn research_request_prefers_read_role_over_executor_even_with_better_eval() {
    let server = scoring_test_server();

    // A read-only research task (e.g. "list files and summarize each"): the
    // explore role fits; an executor's diff/tests/files_changed evidence
    // contract is unsatisfiable. Give the EXECUTOR the better live history
    // (two verified successes) and the explorer NONE, then assert the explorer
    // still wins on role/task fit — the routing-policy gap surfaced live where
    // glm_51_impl(executor)=23.8 beat deepseek_explore(explore)=-0.2.
    let risk = DispatchRisk {
        task_type: "research_request".to_string(),
        risk: "low".to_string(),
        reasons: Vec::new(),
        required_profiles: Vec::new(),
        blocked_profiles: Vec::new(),
    };

    let executor = resolve_dispatch_profile("glm_51_impl").expect("executor profile");
    let explorer = resolve_dispatch_profile("deepseek_explore").expect("explore profile");

    let executor_rows = vec![
        verified_eval_row(executor.name, crate::agent_eval::TaskType::ResearchRequest),
        verified_eval_row(executor.name, crate::agent_eval::TaskType::ResearchRequest),
    ];

    let executor_candidate =
        score_profile_candidate(&server, executor, &risk, &executor_rows, &[], &[])
            .expect("executor");
    let explorer_candidate =
        score_profile_candidate(&server, explorer, &risk, &[], &[], &[]).expect("explorer");

    assert!(
        explorer_candidate.score > executor_candidate.score,
        "read-only research must prefer the explore role ({}) over a write-executor \
         with better eval history ({})",
        explorer_candidate.score,
        executor_candidate.score
    );

    // explain_request shares the read-only treatment (same arm).
    let explain_risk = DispatchRisk {
        task_type: "explain_request".to_string(),
        ..risk.clone()
    };
    let explain_executor = score_profile_candidate(
        &server,
        resolve_dispatch_profile("glm_51_impl").expect("executor"),
        &explain_risk,
        &[],
        &[],
        &[],
    )
    .expect("explain executor");
    let explain_explorer = score_profile_candidate(
        &server,
        resolve_dispatch_profile("deepseek_explore").expect("explorer"),
        &explain_risk,
        &[],
        &[],
        &[],
    )
    .expect("explain explorer");
    assert!(
        explain_explorer.score > explain_executor.score,
        "explain_request must also prefer the explore role ({}) over an executor ({})",
        explain_explorer.score,
        explain_executor.score
    );
}
