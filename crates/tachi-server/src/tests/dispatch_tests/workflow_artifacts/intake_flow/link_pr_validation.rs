use super::*;

#[tokio::test]
async fn tachi_gh_link_pr_requires_flow_id_before_github_access() {
    let server = make_server();
    // Field bag for the shared lifecycle handler (action is ignored by handler).
    let mut params = task_params("status");
    params.pr_ref = Some("kckylechen1/tachi#229".to_string());
    let err = crate::task_lifecycle::handle_task_link_pr(&server, &params)
        .await
        .expect_err("missing flow_id should fail before GitHub access");
    assert_eq!(err, "flow_id is required for link_pr");
}
