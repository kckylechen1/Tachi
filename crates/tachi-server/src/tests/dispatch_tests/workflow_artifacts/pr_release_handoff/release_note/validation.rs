use super::*;

#[tokio::test]
async fn lifecycle_release_note_requires_flow_or_pr_ref_before_github_access() {
    let server = make_server();
    let params = task_params("status");
    let err = crate::task_lifecycle::handle_task_release_note(&server, &params)
        .await
        .expect_err("missing release note target should fail before GitHub access");
    assert_eq!(
        err,
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}

#[tokio::test]
async fn tachi_gh_release_note_uses_lifecycle_validation_without_repo() {
    let server = make_server();
    let params = crate::tool_params::TachiGhParams {
        action: "release_note".to_string(),
        ..Default::default()
    };
    let err = server
        .tachi_gh(Parameters(params))
        .await
        .expect_err("missing release note target should fail before requiring repo");
    assert_eq!(
        err,
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}
