use super::*;

#[tokio::test]
async fn tachi_task_recommend_escalates_risk_from_doc_and_spec_paths() {
    let server = make_server();
    let mut params = task_params("recommend");
    params.task = Some("plan a small documentation update".to_string());
    params.doc_paths =
        vec!["docs/engineering/architecture/agent-credential-surfaces.md".to_string()];
    params.spec_paths = vec!["crates/tachi-server/src/vault_crypto.rs".to_string()];
    params.limit = Some(10);

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("recommend should succeed with file context");
    let rec: serde_json::Value = serde_json::from_str(&raw).expect("recommend JSON");

    assert_eq!(rec["risk"], serde_json::json!("high"));
    assert!(rec["risk_reasons"]
        .as_array()
        .is_some_and(|reasons| reasons.iter().any(|reason| reason
            .as_str()
            .is_some_and(|s| s == "touches vault/secrets boundary"))));
    assert!(rec["blocked_profiles"]
        .as_array()
        .is_some_and(|profiles| profiles.iter().any(|profile| profile == "codex_53_fast")));
    assert!(
        rec["recommended_profile"] != serde_json::json!("codex_53_fast"),
        "high-risk path context must not recommend the fast lane: {rec:#}"
    );
}
