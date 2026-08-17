use super::*;

/// #1690 C3 re-anchor: the capability-bundle artifact/card linkage is retired
/// end-to-end. What this test originally guarded — the dispatch response and
/// the flow card linking the same run artifacts, plus the feedback-rules trace
/// surviving on the receipt — is re-anchored onto the artifacts that survive:
/// prompt_file / context_file / trajectory_file and feedback_rules.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_response_and_flow_card_link_run_artifacts() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let flow_id = format!(
        "flow_20260609T000003Z_artifact_card_{}",
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

    let mut params = dispatch_params(Some("custom"), "smoke run artifact linkage");
    params.command = vec!["python3".to_string(), "-c".to_string(), "pass".to_string()];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    params.profile = Some("glm_impl".to_string());
    params.flow_id = Some(flow_id.clone());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    assert_eq!(response["feedback_rules"]["status"], json!("none"));
    let prompt_file = response["prompt_file"].as_str().expect("prompt file");
    let context_file = response["context_file"].as_str().expect("context file");
    let trajectory_file = response["trajectory_file"].as_str().expect("trajectory file");
    assert!(std::path::Path::new(prompt_file).exists(), "prompt file exists");
    assert!(
        std::path::Path::new(context_file).exists(),
        "context file exists"
    );
    assert!(
        std::path::Path::new(trajectory_file).exists(),
        "trajectory file exists"
    );
    // The retired capability-bundle receipt fields must no longer exist on the
    // response (discriminator: the echo pipeline is gone end-to-end).
    assert!(
        response.get("capability_bundle").is_none(),
        "retired capability_bundle receipt field must be absent"
    );
    assert!(
        response.get("capability_bundle_file").is_none(),
        "retired capability_bundle_file receipt field must be absent"
    );

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(
        card["prompt_file"].as_str(),
        Some(prompt_file),
        "flow card should link the same prompt artifact"
    );
    assert_eq!(
        card["trajectory_file"].as_str(),
        Some(trajectory_file),
        "flow card should link the same trajectory artifact"
    );
    assert_eq!(
        card["context_file"].as_str(),
        Some(context_file),
        "flow card should link the same context artifact"
    );
}

/// #1690 C3 discriminator (5b, dispatch layer): a caller that still passes the
/// retired `auto_capability_bundle` key on the wire gets a dispatch whose
/// prompt.md contains NO capability-bundle section — the injection pipeline is
/// gone, and serde silently ignores the unknown key on the struct.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dispatch_with_retired_bundle_key_writes_prompt_without_bundle_section() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    // The retired key is carried on the wire JSON; the struct no longer has the
    // field, so serde ignores it (asserted at the dispatch layer via the
    // prompt.md artifact the spawned run is actually fed).
    let params: TachiDispatchParams = serde_json::from_value(json!({
        "agent": "custom",
        "profile": "glm_impl",
        "task": "smoke retired bundle key",
        "staffing_reason": "explicit_user_request",
        "command": ["python3", "-c", "pass"],
        "cwd": tmp.path().to_string_lossy(),
        "unmanaged_cwd": true,
        "auto_capability_bundle": true,
        "include_capability_bundle": true,
    }))
    .expect("retired bundle keys must be ignored, not rejected");

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let prompt_file = response["prompt_file"].as_str().expect("prompt file");
    let prompt = std::fs::read_to_string(prompt_file).expect("prompt");
    assert!(
        !prompt.contains("## Capability Bundle"),
        "dispatch with retired bundle key must not inject bundle section: {prompt}"
    );
    assert!(
        !prompt.contains("capability_bundle"),
        "dispatch with retired bundle key must not leak bundle trace into prompt: {prompt}"
    );
}
