use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_response_and_flow_card_link_capability_bundle_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let flow_id = format!(
        "flow_20260609T000003Z_capability_bundle_card_{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id.as_str()).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id.clone(),
            "status": "active",
            "dispatch_ids": [],
        }))
        .expect("serialize status"),
    )
    .expect("seed status");

    let mut params = dispatch_params(Some("custom"), "smoke capability bundle artifact");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some(flow_id.clone());
    params.auto_capability_bundle = Some(true);

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    assert_eq!(response["capability_bundle"]["requested"], json!(true));
    assert_eq!(response["capability_bundle"]["status"], json!("injected"));
    assert_eq!(response["capability_bundle"]["injected"], json!(true));
    assert_eq!(response["feedback_rules"]["status"], json!("none"));
    let artifact_file = response["capability_bundle_file"]
        .as_str()
        .expect("capability bundle file");
    let artifact: Value =
        serde_json::from_str(&std::fs::read_to_string(artifact_file).expect("artifact"))
            .expect("artifact JSON");
    assert_eq!(artifact["requested"], json!(true));
    assert_eq!(artifact["status"], json!("injected"));
    assert_eq!(artifact["injected"], json!(true));
    assert_eq!(artifact["feedback_rules"]["status"], json!("none"));

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["capability_bundle"]["requested"], json!(true));
    assert_eq!(card["capability_bundle"]["injected"], json!(true));
    assert_eq!(
        card["capability_bundle_file"].as_str(),
        Some(artifact_file),
        "flow card should link the same capability bundle artifact"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_explicit_false_writes_disabled_capability_bundle_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let mut params = dispatch_params(Some("custom"), "smoke disabled capability bundle artifact");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.auto_capability_bundle = Some(false);

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    assert_eq!(response["capability_bundle"]["status"], json!("disabled"));
    assert_eq!(response["capability_bundle"]["requested"], json!(false));
    assert_eq!(response["capability_bundle"]["disabled"], json!(true));
    assert_eq!(response["capability_bundle"]["injected"], json!(false));

    let artifact_file = response["capability_bundle_file"]
        .as_str()
        .expect("capability bundle file");
    let artifact: Value =
        serde_json::from_str(&std::fs::read_to_string(artifact_file).expect("artifact"))
            .expect("artifact JSON");
    assert_eq!(artifact["status"], json!("disabled"));
    assert_eq!(artifact["source"], json!("params"));

    let prompt_file = response["prompt_file"].as_str().expect("prompt file");
    let prompt = std::fs::read_to_string(prompt_file).expect("prompt");
    assert!(
        !prompt.contains("## Capability Bundle"),
        "disabled dispatch should not inject bundle section: {prompt}"
    );
}

#[test]
fn dispatch_ids_are_unique_within_same_second() {
    let now = chrono::Utc::now();
    let id_a = crate::dispatch_ops::new_dispatch_id(now, "my agent/here");
    let id_b = crate::dispatch_ops::new_dispatch_id(now, "my agent/here");
    assert_ne!(
        id_a, id_b,
        "same second + same agent must still produce unique IDs"
    );
    let prefix = format!("{}-my-agent-here", now.format("%Y%m%dT%H%M%SZ"));
    assert!(
        id_a.starts_with(&prefix),
        "id_a should start with expected prefix: {id_a}"
    );
    assert!(
        id_b.starts_with(&prefix),
        "id_b should start with expected prefix: {id_b}"
    );
}

#[tokio::test]
async fn dispatch_rejects_unknown_agent_with_fleet_hint() {
    let server = make_server();
    let err = crate::dispatch_ops::handle_tachi_dispatch(
        &server,
        TachiDispatchParams {
            agent: Some("gemini".to_string()),
            profile: None,
            task: "noop".to_string(),
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
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        },
    )
    .await
    .expect_err("gemini should not be in the fleet");
    assert!(err.contains("Unknown agent"), "err: {err}");
    assert!(err.contains("claude"), "err: {err}");
    assert!(err.contains("grok"), "err: {err}");
}

#[tokio::test]
async fn custom_dispatch_rejects_mcp_injection() {
    let server = make_server();
    let mut params = dispatch_params(Some("custom"), "should fail before subprocess");
    params.inject_tachi_mcp = Some(true);
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("custom backend must reject MCP injection");
    assert!(
        err.contains("custom backend"),
        "unexpected custom injection error: {err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_response_includes_suggested_complete_payload() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let mut params = dispatch_params(Some("custom"), "smoke custom dispatch completion skeleton");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some("flow-complete-skeleton".to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: serde_json::Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let suggested = &response["suggested_complete_command"];

    assert_eq!(suggested["tool"], serde_json::json!("tachi_task"));
    assert_eq!(
        suggested["arguments"]["action"],
        serde_json::json!("complete")
    );
    assert_eq!(
        suggested["arguments"]["dispatch_id"], response["dispatch_id"],
        "completion skeleton should carry dispatch_id"
    );
    assert_eq!(
        suggested["arguments"]["profile"],
        serde_json::json!("glm_51_impl")
    );
    assert_eq!(
        suggested["arguments"]["flow_id"],
        serde_json::json!("flow-complete-skeleton")
    );
    assert!(suggested["arguments"]["tests_run"].is_array());
    assert!(suggested["arguments"]["evidence_refs"].is_array());
}
