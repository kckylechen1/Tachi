use super::*;

#[tokio::test]
async fn vault_unlock_does_not_reset_failed_attempts_before_password_verification() {
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

    server.vault_write().failed_attempts = (5, Some(Instant::now() - Duration::from_secs(1)));

    let err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "still-wrong-after-expiry".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        }))
        .await
        .expect_err("wrong password after lockout expiry should relock immediately");
    assert!(
        err.contains("Too many failed vault unlock attempts"),
        "expired lockout must not grant a fresh burst before verification: {err}"
    );

    let state = server.vault_read().failed_attempts;
    assert!(state.0 >= 5);
    assert!(
        state.1.is_some_and(|until| until > Instant::now()),
        "wrong post-expiry attempt should install a new lockout window"
    );
}
