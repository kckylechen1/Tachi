use super::*;

/// Phase 1: the write-back verb. dry_run must preview the comment body and
/// NOT touch the network (returns before `gh` is ever invoked).
#[tokio::test]
async fn gh_issue_comment_dry_run_previews_without_posting() {
    let server = make_server();
    let resp = crate::gh_ops::handle_gh_comment(
        &server,
        "issue",
        crate::tool_params::GhCommentParams {
            repo: "owner/repo".to_string(),
            number: 42,
            body: Some("hello from close_loop".to_string()),
            dry_run: true,
        },
    )
    .await
    .expect("dry-run comment should succeed offline");
    let json: Value = serde_json::from_str(&resp).expect("json");
    assert_eq!(json["dry_run"], json!(true));
    assert_eq!(json["number"], json!(42));
    assert_eq!(json["preview_body"], json!("hello from close_loop"));
    assert_eq!(json["tool"], json!("tachi_gh_issue_comment"));
}

/// A comment with no body is a usage error, surfaced before any gh call.
#[tokio::test]
async fn gh_comment_rejects_empty_body() {
    let server = make_server();
    let err = crate::gh_ops::handle_gh_comment(
        &server,
        "pr",
        crate::tool_params::GhCommentParams {
            repo: "owner/repo".to_string(),
            number: 7,
            body: None,
            dry_run: true,
        },
    )
    .await
    .unwrap_err();
    assert!(err.contains("non-empty 'body'"), "got: {err}");
}
