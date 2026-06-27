use super::*;

#[tokio::test]
async fn tachi_task_cycle_status_requires_flow_issue_or_pr_target() {
    let server = make_server();
    let params = task_params("cycle_status");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing target should fail before GitHub access");
    assert_eq!(
        err,
        "cycle_status requires flow_id, issue_ref='owner/repo#123', or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}
