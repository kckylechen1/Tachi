use super::*;

#[tokio::test]
async fn tachi_task_doc_index_returns_layered_authority_groups() {
    let server = make_server();
    let mut params = task_params("doc_index");
    params.format = Some("json".to_string());
    params.task = Some(
        "Implement issue-driven docs flow docs/engineering/architecture/subagent-eval-system.md"
            .to_string(),
    );
    params.issue_ref = Some("kckylechen1/tachi#363".to_string());
    params.pr_ref = Some("kckylechen1/tachi#364".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()];

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("doc_index should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("doc_index JSON");

    assert_eq!(parsed["kind"], json!("doc_index"));
    assert!(parsed["project_work_record"]
        .as_array()
        .is_some_and(|records| records.iter().any(|record| {
            record["kind"] == json!("github_issue")
                && record["ref"] == json!("kckylechen1/tachi#363")
        })));
    assert!(parsed["doc_index"]["groups"]
        .as_array()
        .is_some_and(|groups| groups.iter().any(|group| {
            group["name"] == json!("canonical_docs")
                && group["authority"] == json!("canonical")
                && group["count"].as_u64().unwrap_or(0) > 0
        })));
}
