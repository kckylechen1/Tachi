use super::*;

#[tokio::test]
async fn profile_rejects_non_dry_run_calls() {
    let server = make_server();
    let mut params = profile_params("render");
    params.dry_run = false;
    params.documents = vec![TachiProfileDocumentParams {
        kind: "agents".to_string(),
        path: None,
        content: "- Keep answers short.\n".to_string(),
    }];

    let err = crate::agent_profile_ops::handle_tachi_profile(&server, params)
        .await
        .expect_err("non-dry-run profile calls are not supported");
    assert!(err.contains("dry_run=false is not supported"));
}
