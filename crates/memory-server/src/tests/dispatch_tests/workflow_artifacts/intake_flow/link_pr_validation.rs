use super::*;

#[tokio::test]
async fn tachi_task_link_pr_requires_flow_id_before_github_access() {
    let server = make_server();
    let mut params = task_params("link_pr");
    params.pr_ref = Some("kckylechen1/tachi#229".to_string());
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing flow_id should fail before GitHub access");
    assert_eq!(err, "flow_id is required for link_pr");
}
