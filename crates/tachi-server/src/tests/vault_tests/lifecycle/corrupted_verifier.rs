use super::*;

/// Discriminating test for the tachi#1080 three-state `verify_password` fix:
/// a corrupted `vault_config.verifier` (malformed base64, not a wrong
/// password) must fail `vault_unlock` with an error that is NOT counted
/// against the brute-force lockout counter. Built the same way
/// `lockout_reset.rs`/`lockout_preservation.rs` exercise the counter, but
/// corrupting the stored verifier directly (à la
/// `rotation/decrypt_failure.rs`'s `with_global_store` pattern) instead of
/// supplying a wrong password.
#[tokio::test]
async fn vault_unlock_with_corrupted_verifier_errs_without_counting_against_lockout() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "correct-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    server
        .with_global_store(|store| {
            let mut config = store
                .vault_get_config()
                .map_err(|e| e.to_string())?
                .expect("vault_config should exist after init");
            config.verifier = "not-valid-base64!!!:also-not-valid-base64!!!".to_string();
            store.vault_set_config(&config).map_err(|e| e.to_string())
        })
        .expect("corrupt vault_config.verifier");

    let err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("a corrupted verifier must fail unlock");
    assert!(
        !err.contains("Wrong password"),
        "a corrupted verifier is vault corruption, not a wrong password: {err}"
    );

    let state = server.vault_read().failed_attempts;
    assert_eq!(
        state.0, 0,
        "corruption must not be counted against the brute-force lockout counter \
         (verify_password's malformed-input Err short-circuits before \
         record_vault_unlock_failure runs)"
    );
    assert!(
        state.1.is_none(),
        "corruption alone must not install a lockout window"
    );
}
