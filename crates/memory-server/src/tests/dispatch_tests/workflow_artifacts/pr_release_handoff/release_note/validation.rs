use super::*;

#[tokio::test]
async fn tachi_task_release_note_requires_flow_or_pr_ref_before_github_access() {
    let server = make_server();
    let params = task_params("release_note");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing release note target should fail before GitHub access");
    assert_eq!(
        err,
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}
