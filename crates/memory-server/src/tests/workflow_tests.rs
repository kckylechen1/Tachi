use super::make_server;
use crate::tool_params::{GetMemoryParams, TachiWorkflowParams};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

#[tokio::test]
async fn workflow_close_loop_writes_wiki_with_references() {
    let server = make_server();
    let resp = server
        .tachi_workflow(Parameters(TachiWorkflowParams {
            action: "close_loop".to_string(),
            issue_ref: Some("kckylechen1/tachi#150".to_string()),
            pr_ref: None,
            doc_paths: vec!["docs/wiki-references-spec.md".to_string()],
            spec_paths: vec![],
            related_issues: vec!["#149".to_string()],
            // Keep the smoke test offline: do not attempt a real gh comment.
            post_comment: Some(false),
            flow_id: None,
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

    // Phase 2: close_loop now fans out closure actions. With post_comment=false
    // it must NOT post, and it should still emit the spec-drift advisory.
    assert_eq!(
        json["closure_actions"]["issue_comment"]["posted"],
        json!(false)
    );
    assert_eq!(
        json["closure_actions"]["spec_advisory"]["status"],
        json!("advisory"),
        "docs referenced but no spec_paths → spec-drift advisory expected"
    );
    assert!(json["closure_actions"]["comment_body"]
        .as_str()
        .is_some_and(|body| body.contains("close_loop")));

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
    let refs: Vec<String> = entry["metadata"]["source_refs"]
        .as_array()
        .expect("refs")
        .iter()
        .map(|value| value.as_str().expect("reference string").to_string())
        .collect();
    assert_eq!(
        refs,
        vec![
            "kckylechen1/tachi#150".to_string(),
            "docs/wiki-references-spec.md".to_string(),
            "#149".to_string(),
        ]
    );
    assert_eq!(entry["metadata"]["layer"], json!("wiki"));
    assert_eq!(entry["metadata"]["scope"], json!("global"));
    assert_eq!(entry["metadata"]["authority"], json!("advisory"));
    assert_eq!(entry["metadata"]["status"], json!("active"));
    assert_eq!(
        entry["metadata"]["source_ref"],
        json!("kckylechen1/tachi#150")
    );
    assert_eq!(
        entry["metadata"]["promotion"]["decision_mode"],
        json!("explicit_invocation")
    );
    assert_eq!(
        entry["metadata"]["promotion"]["destination_layer"],
        json!("wiki")
    );
    assert_eq!(
        entry["metadata"]["promotion"]["automatic_double_write"],
        json!(false)
    );
    assert_eq!(
        entry["metadata"]["promotion"]["source_refs"],
        json!([
            "kckylechen1/tachi#150",
            "docs/wiki-references-spec.md",
            "#149"
        ])
    );
}
