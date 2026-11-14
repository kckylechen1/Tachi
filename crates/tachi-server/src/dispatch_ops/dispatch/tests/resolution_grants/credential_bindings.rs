use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn retained_opencode_credential_selector_reaches_failure_evidence() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp_home.path().join(".tachi"));
    let server = crate::tests::make_server();
    let mut params = test_dispatch_params(None, "operator credential evidence");
    params.profile = Some("glm_impl".to_string());
    params.credential_profiles = vec!["opencode_shared".to_string()];
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "import os; open('launcher-cwd', 'w').write(os.getcwd())".to_string(),
    ];

    let err = handle_tachi_dispatch(&server, params)
        .await
        .expect_err("admitted operator credential reports its selector on failure");
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
