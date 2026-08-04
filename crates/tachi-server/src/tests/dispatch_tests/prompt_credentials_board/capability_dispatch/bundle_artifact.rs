use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_response_and_flow_card_link_capability_bundle_artifact() {
    let _lock = crate::utils::global_test_lock()
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
    let run_dir =
        crate::task_lifecycle::run_dir_for_flow_id(flow_id.as_str()).expect("flow run dir");
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
    params.unmanaged_cwd = Some(true);
    params.profile = Some("glm_impl".to_string());
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
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let mut params = dispatch_params(Some("custom"), "smoke disabled capability bundle artifact");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    params.profile = Some("glm_impl".to_string());
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
