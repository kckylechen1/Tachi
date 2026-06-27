use super::*;

#[tokio::test]
async fn tachi_task_ux_matrix_without_flow_is_read_only_starting_checklist() {
    let server = make_server();
    let mut params = task_params("ux_matrix");
    params.task = Some("Review a new Tachi feature request".to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should work before intake flow exists");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("ux_matrix"));
    assert_eq!(parsed["flow_id"], Value::Null);
    assert_eq!(parsed["ux_matrix_path"], Value::Null);
    assert_eq!(parsed["overall"], json!("needs_action"));
    assert!(parsed["matrix"].as_array().is_some_and(|matrix| matrix
        .iter()
        .any(|step| step["id"] == json!("intake") && step["status"] == json!("ready"))));
    assert!(parsed["matrix"].as_array().is_some_and(|matrix| matrix
        .iter()
        .any(|step| step["id"] == json!("canonical_docs") && step["status"] == json!("pending"))));
}
