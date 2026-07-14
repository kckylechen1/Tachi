use super::*;

/// Compatibility guard for the tachi#1080 wiring (unlock now reads + enforces
/// `vault_config.kdf_params` instead of deriving with a compile-time
/// constant): a vault initialized with the DEFAULT parameter set must still
/// unlock.
///
/// This is intentionally a NON-regression guard, not a red→green
/// discrimination test — it is green BOTH before and after the fix. Before
/// the fix, unlock ignores `kdf_params` and derives with the compile-time
/// constant (which is exactly what init wrote), so it already succeeds; the
/// reason it cannot be a red→green test is that "still unlocks" is the
/// day-one-compat invariant itself, and being green before is precisely the
/// point. It exists to catch a regression where the new enforcement rejects a
/// vault the product created with its own active profile.
#[tokio::test]
async fn vault_unlock_succeeds_for_default_kdf_params_after_wiring() {
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
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect("vault_unlock must still succeed for the default kdf_params");
}

/// Discrimination test for tachi#1080: a stored `kdf_params` that is VALID
/// JSON but OUTSIDE the supported set must fail `vault_unlock` with a loud,
/// versioned error.
///
/// RED before the fix: unlock ignored `kdf_params` and derived with the
/// compile-time constant, so with the CORRECT password the derived key
/// matched the stored verifier and unlock SUCCEEDED — this `expect_err` would
/// fail. After the fix, `from_stored_json` rejects the value as Unsupported
/// before Argon2 even runs, so unlock returns the versioned error.
///
/// `{"m":1,"t":1,"p":1}` is chosen because it is distinct from BOTH the
/// production profile {65536,3,4} and the in-test-build active profile
/// {64,1,1}, so it is unsupported in EVERY build mode — the assertion holds
/// under both production and test-support builds. The error must NOT be
/// phrased as a password error and must NOT touch the brute-force lockout
/// counter (the parse fails before `verify_password`, mirroring the
/// corrupted-verifier discipline).
#[tokio::test]
async fn vault_unlock_rejects_unsupported_kdf_params_with_versioned_error() {
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

    // Forge valid-JSON but unsupported kdf_params directly in the stored
    // config (same `with_global_store` pattern as corrupted_verifier.rs).
    server
        .with_global_store(|store| {
            let mut config = store
                .vault_get_config()
                .map_err(|e| e.to_string())?
                .expect("vault_config should exist after init");
            config.kdf_params = r#"{"m":1,"t":1,"p":1}"#.to_string();
            store.vault_set_config(&config).map_err(|e| e.to_string())
        })
        .expect("forge unsupported kdf_params");

    let err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("an unsupported kdf_params must fail unlock");

    assert!(
        !err.to_lowercase().contains("wrong password"),
        "an unsupported kdf_params is a format/version error, not a wrong password: {err}"
    );
    assert!(
        err.contains("kdf_params") && err.contains("supported"),
        "the versioned error must name the stored kdf_params and the supported set: {err}"
    );

    let state = server.vault_read().failed_attempts;
    assert_eq!(
        state.0, 0,
        "an unsupported kdf_params must not count against the brute-force lockout counter \
         (from_stored_json rejects before verify_password runs)"
    );
    assert!(
        state.1.is_none(),
        "a format error alone must not install a lockout window"
    );
}

/// Discrimination test for tachi#1080: a stored `kdf_params` that is
/// MALFORMED JSON must fail `vault_unlock` with the same kind of versioned
/// error as an unsupported value.
///
/// RED before the fix: unlock ignored `kdf_params` entirely and derived with
/// the compile-time constant; with the correct password the key matched the
/// verifier, so unlock SUCCEEDED — this `expect_err` would fail. After the
/// fix, `from_stored_json` classifies the blob as Malformed (deny_unknown_fields
/// + serde) and unlock returns the versioned error before reaching
/// `verify_password`, so it is neither a "wrong password" nor a lockout event.
#[tokio::test]
async fn vault_unlock_rejects_malformed_kdf_params_json_with_versioned_error() {
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
            config.kdf_params = "not-valid-json".to_string();
            store.vault_set_config(&config).map_err(|e| e.to_string())
        })
        .expect("forge malformed kdf_params");

    let err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("a malformed kdf_params must fail unlock");

    assert!(
        !err.to_lowercase().contains("wrong password"),
        "malformed kdf_params is a format error, not a wrong password: {err}"
    );
    assert!(
        err.contains("kdf_params") && err.contains("supported"),
        "the versioned error must name the stored kdf_params and the supported set: {err}"
    );

    let state = server.vault_read().failed_attempts;
    assert_eq!(
        state.0, 0,
        "a malformed kdf_params must not count against the brute-force lockout counter"
    );
    assert!(
        state.1.is_none(),
        "a format error alone must not install a lockout window"
    );
}
