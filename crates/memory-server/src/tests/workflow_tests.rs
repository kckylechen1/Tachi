use super::*;

#[tokio::test]
async fn workflow_close_loop_writes_wiki_with_references() {
    let server = make_server();
    let resp = server
        .tachi_workflow(Parameters(TachiWorkflowParams {
            action: "close_loop".to_string(),
            issue_ref: Some("kckylechen1/tachi#150".to_string()),
            doc_paths: vec!["docs/wiki-references-spec.md".to_string()],
            related_issues: vec!["#149".to_string()],
            wiki_title: Some("Closure smoke".to_string()),
            wiki_text: Some("Closed loop lesson for issue 150.".to_string()),
            wiki_path: None,
            wiki_topic: Some("closure-smoke".to_string()),
            wiki_summary: None,
            wiki_category: None,
            wiki_keywords: vec![],
            wiki_entities: vec![],
            wiki_importance: Some(0.8),
            wiki_scope: None,
            wiki_domain: None,
            project: None,
            force: true,
        }))
        .await
        .expect("close_loop");

    let json: Value = serde_json::from_str(&resp).expect("json");
    assert_eq!(json["ok"], json!(true));
    let wiki_id = json["wiki"]["id"].as_str().expect("wiki id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: wiki_id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get");
    let entry: Value = serde_json::from_str(&fetched).expect("entry");
    let refs = entry["metadata"]["source_refs"]
        .as_array()
        .expect("refs");
    assert!(refs.len() >= 2);
}