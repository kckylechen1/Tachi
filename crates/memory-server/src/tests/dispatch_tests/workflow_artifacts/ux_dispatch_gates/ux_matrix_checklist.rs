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

/// F2 (#495/#913): GH PR lifecycle steps must coach `tachi_gh`, not the
/// deprecated `tachi_task` dual entry.
#[tokio::test]
async fn f2_ux_matrix_gh_lifecycle_tools_point_at_tachi_gh() {
    let server = make_server();
    let mut params = task_params("ux_matrix");
    params.task = Some("Ship a feature with PR lifecycle".to_string());
    params.issue_ref = Some("kckylechen1/tachi#918".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should work");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix JSON");
    let matrix = parsed["matrix"].as_array().expect("matrix array");

    for (id, expected_tool) in [
        ("link_pr", "tachi_gh(action='link_pr')"),
        ("pr_status", "tachi_gh(action='pr_status')"),
        ("release_note", "tachi_gh(action='release_note')"),
    ] {
        let step = matrix
            .iter()
            .find(|step| step["id"] == json!(id))
            .unwrap_or_else(|| panic!("ux_matrix missing step {id}"));
        assert_eq!(
            step["tool"].as_str(),
            Some(expected_tool),
            "ux_matrix step {id} must coach {expected_tool}, got {:?}",
            step["tool"]
        );
        let tool = step["tool"].as_str().unwrap_or("");
        assert!(
            !tool.starts_with("tachi_task(action='link_pr'")
                && !tool.starts_with("tachi_task(action='pr_status'")
                && !tool.starts_with("tachi_task(action='pr_handoff'")
                && !tool.starts_with("tachi_task(action='release_note'"),
            "ux_matrix step {id} must not re-advertise deprecated tachi_task lifecycle tool: {tool}"
        );
    }

    // Full payload must not reintroduce the deprecated dual entry anywhere.
    let full = serde_json::to_string(&parsed).expect("serialize");
    for needle in [
        "tachi_task(action='link_pr'",
        "tachi_task(action='pr_status'",
        "tachi_task(action='pr_handoff'",
        "tachi_task(action='release_note'",
    ] {
        assert!(
            !full.contains(needle),
            "ux_matrix payload must not contain deprecated coaching {needle}"
        );
    }
}
