use super::*;

/// A lockout that has ended starts the count over, so an owner who waited
/// it out gets the whole budget again instead of one attempt per window.
#[tokio::test]
async fn vault_unlock_starts_the_count_over_once_a_lockout_has_ended() {
    let server = make_server();

    server
        .seed_vault("correct-password")
        .expect("seed unlocked vault");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    server.vault_write().failed_attempts = (5, Some(Instant::now() - Duration::from_secs(1)));

    let err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "wrong-after-expiry".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        }))
        .await
        .expect_err("wrong password after lockout expiry should fail");
    assert!(
        err.contains("Wrong password"),
        "the first attempt after a lockout counts again; it does not lock at once: {err}"
    );
    let state = server.vault_read().failed_attempts;
    assert_eq!(state.0, 1, "the count started over");
    assert!(
        state.1.is_none(),
        "a first miss after a lockout installs no new window"
    );

    // The next lockout again needs the whole budget.
    for attempt in 2..5 {
        let err = server
            .vault_unlock(Parameters(VaultUnlockParams {
                password: format!("wrong-password-{attempt}"),
                password_fifo_path: None,
                use_keychain: false,
            }))
            .await
            .expect_err("wrong password should fail");
        assert!(
            err.contains("Wrong password"),
            "expected wrong password error on attempt {attempt}, got: {err}"
        );
    }
    let err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "still-wrong".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        }))
        .await
        .expect_err("fifth failed attempt should trigger lockout");
    assert!(
        err.contains("Too many failed vault unlock attempts"),
        "expected lockout error, got: {err}"
    );
    let state = server.vault_read().failed_attempts;
    assert!(
        state.1.is_some_and(|until| until > Instant::now()),
        "the fifth attempt installs a new lockout window"
    );
}
