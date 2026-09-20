use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes the process-wide PATH fixture across async Vault calls
async fn vault_auto_lock_expires_cached_key() {
    #[cfg(target_os = "macos")]
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    // The external test crate links the product Keychain reader, which would
    // otherwise see the host's background password after the forced lock.
    #[cfg(target_os = "macos")]
    let security_fixture_dir = tempfile::tempdir().expect("security fixture directory");
    #[cfg(target_os = "macos")]
    let _security_fixture_path = {
        use std::os::unix::fs::PermissionsExt;
        let security = security_fixture_dir.path().join("security");
        std::fs::write(
            &security,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$0.args\"\nexit 1\n",
        )
        .expect("write missing-entry security fixture");
        std::fs::set_permissions(&security, std::fs::Permissions::from_mode(0o700))
            .expect("executable security fixture");
        let mut paths = vec![security_fixture_dir.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let joined = std::env::join_paths(paths).expect("join security fixture PATH");
        crate::test_support::EnvRestore::set_path("PATH", std::path::Path::new(&joined))
    };
    let server = make_server();
    // Use a longer timeout than the slowest CI step between init/set so the
    // setup itself does not race the auto-lock; the test then forces
    // expiration by rewinding `vault_unlock_time` below.
    server.vault_write().auto_lock_after_secs = 30;

    server
        .vault_init(Parameters(VaultInitParams {
            password: "auto-lock-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENAI_API_KEY".to_string(),
            value: "secret-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "auto lock test secret".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set should succeed");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("secret-value"),
        "vault_set should refresh provider cache before auto-lock"
    );
    #[cfg(target_os = "macos")]
    let keychain_calls_before_lock =
        std::fs::read_to_string(security_fixture_dir.path().join("security.args"))
            .unwrap_or_default();

    server.vault_write().unlock_time = Some(Instant::now() - Duration::from_secs(60));

    let err = server
        .vault_get(Parameters(VaultGetParams {
            name: "OPENAI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect_err("vault_get should fail after auto-lock timeout");
    assert!(
        err.contains("Vault auto-locked"),
        "expected auto-lock error, got: {err}"
    );
    #[cfg(target_os = "macos")]
    {
        let invoked = std::fs::read_to_string(security_fixture_dir.path().join("security.args"))
            .expect("auto-lock must exercise the product Keychain read against the fixture");
        let new_calls = invoked
            .strip_prefix(&keychain_calls_before_lock)
            .expect("security fixture log must retain the calls made before auto-lock");
        let args: Vec<&str> = new_calls.lines().collect();
        assert!(
            !args.is_empty(),
            "auto-lock must issue a new product Keychain read"
        );
        let expected = [
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
            "-w",
        ];
        let mut requests = args.chunks_exact(expected.len());
        for request in &mut requests {
            assert_eq!(
                request,
                expected.as_slice(),
                "security fixture must receive the exact find-generic-password request"
            );
        }
        assert!(
            requests.remainder().is_empty(),
            "security fixture witness must contain whole requests: {new_calls}"
        );
    }

    let status = server
        .vault_status()
        .await
        .expect("vault_status should succeed");
    let status_json: serde_json::Value =
        serde_json::from_str(&status).expect("vault_status response should be JSON");
    assert_eq!(status_json["locked"], json!(true));
    assert_eq!(status_json["session"]["locked"], json!(true));
    assert!(matches!(
        status_json["resolver"]["state"].as_str(),
        Some("locked" | "locked_keychain_available")
    ));
    assert!(
        status_json["provider_cache"]["secret_pool_count"]
            .as_u64()
            .is_some(),
        "vault_status should report provider cache count: {status_json:#}"
    );
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .is_some(),
        "auto-lock must NOT clear provider secrets — they survive auto-lock (#400)"
    );
}
