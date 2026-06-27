use super::*;

#[tokio::test]
async fn tachi_shell_rejects_invalid_action() {
    let server = make_server();
    let err = crate::shell_ops::handle_tachi_shell(&server, shell_params("launch"))
        .await
        .expect_err("invalid shell action should fail");
    assert!(err.contains("Invalid action"), "unexpected error: {err}");
}

#[tokio::test]
async fn tachi_shell_status_rejects_invalid_flow_id() {
    let server = make_server();
    let mut params = shell_params("status");
    params.flow_id = Some("../flow_escape".to_string());

    let err = crate::shell_ops::handle_tachi_shell(&server, params)
        .await
        .expect_err("invalid flow id should fail");
    assert!(err.contains("Invalid flow_id"), "unexpected error: {err}");
}
