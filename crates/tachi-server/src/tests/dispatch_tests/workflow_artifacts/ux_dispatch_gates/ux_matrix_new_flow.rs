use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_ux_matrix_creates_new_flow_directory() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let flow_id = "flow_20260609T000007Z_ux_matrix_new_flow";
    let mut params = task_params("ux_matrix");
    params.flow_id = Some(flow_id.to_string());
    params.task = Some("Start a new UX matrix before intake".to_string());

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should create a new flow dir");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["flow_id"], json!(flow_id));
    let path = parsed["ux_matrix_path"].as_str().expect("ux matrix path");
    assert!(path.ends_with("ux_matrix.json"), "{path}");
    assert!(std::path::Path::new(path).exists());
}
