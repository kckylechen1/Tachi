use super::*;

#[tokio::test]
async fn tachi_task_intake_requires_issue_target_before_github_access() {
    let server = make_server();
    let params = task_params("intake");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing issue target should fail before GitHub access");
    assert_eq!(
        err,
        "intake requires either repo+number or issue_ref='owner/repo#123' / GitHub issue URL"
    );
}
