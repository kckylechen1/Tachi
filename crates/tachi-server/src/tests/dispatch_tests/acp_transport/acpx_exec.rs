use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn retired_acpx_start_is_refused_before_a_worker_can_spawn() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    for transport in ["acpx", "acp"] {
        let mut params = dispatch_params(Some("custom"), "must not launch");
        params.harness_transport = Some(transport.into());
        params.command = vec!["/bin/echo".into(), "forbidden-spawn".into()];
        params.cwd = Some(temp_home.path().to_string_lossy().into());
        params.unmanaged_cwd = Some(true);
        let error = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
            .await
            .unwrap_err();
        assert!(error.contains("retired"), "{error}");
    }
}
