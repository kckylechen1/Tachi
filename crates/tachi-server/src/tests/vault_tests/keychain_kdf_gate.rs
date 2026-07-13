//! Discrimination tests for the tachi#1080 KDF-param gate on the two
//! macOS-Keychain-backed unlock/verify paths.
//!
//! Both `provider_config::auto_unlock_vault_from_keychain` and
//! `status_health::vault::load_keychain_vault_api_key_values` read a STORED
//! `VaultConfig` and shell out to the real macOS `security` CLI to fetch the
//! keychain password. There is no in-process test seam for the keychain, so
//! these tests install a REAL (scoped, save/restored) `tachi-vault`/`default`
//! Keychain entry to reach the derive site.
//!
//! They are `#[ignore]` because they touch the developer's real Keychain (and
//! both manipulate the same global `tachi-vault`/`default` entry, so they must
//! NOT run in parallel). Run them explicitly and serialized:
//! `cargo test --ignored keychain_kdf_gate -- --test-threads=1`
//!
//! They are genuinely RED on the pre-fix code: both functions ignored
//! `vault_config.kdf_params` and derived with the compile-time constant, so the
//! CORRECT keychain password reproduced the init key, `verify_password`
//! succeeded, and the function returned `Ok(_)` (unlock or empty/loaded Vec).
//! After the fix, `parse_stored_kdf_params` rejects the unsupported value
//! before `verify_password`, so the function returns the loud, versioned error.

use super::*;

/// RAII guard that installs a `tachi-vault`/`default` Keychain entry for the
/// test, capturing any pre-existing credential and restoring it (or removing
/// the entry) on drop so the developer's real Keychain is never clobbered.
struct ScopedKeychainEntry {
    /// The pre-existing password, if any. Restored verbatim on drop.
    saved: Option<String>,
}

impl ScopedKeychainEntry {
    fn install(password: &str) -> Self {
        // Capture any existing entry so it can be restored (never clobber a
        // developer's real tachi-vault credential).
        let saved = match std::process::Command::new("security")
            .args(["find-generic-password", "-s", "tachi-vault", "-a", "default", "-w"])
            .output()
        {
            Ok(out) if out.status.success() => {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            }
            _ => None,
        };

        let add = std::process::Command::new("security")
            .args([
                "add-generic-password",
                "-s",
                "tachi-vault",
                "-a",
                "default",
                "-w",
                password,
                "-U",
            ])
            .output()
            .expect("security add-generic-password should run");
        assert!(
            add.status.success(),
            "security add-generic-password failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );

        Self { saved }
    }
}

impl Drop for ScopedKeychainEntry {
    fn drop(&mut self) {
        match &self.saved {
            Some(pw) => {
                // Restore the original credential verbatim.
                let _ = std::process::Command::new("security")
                    .args([
                        "add-generic-password",
                        "-s",
                        "tachi-vault",
                        "-a",
                        "default",
                        "-w",
                        pw,
                        "-U",
                    ])
                    .output();
            }
            None => {
                // No prior entry; remove the one we added (best-effort).
                let _ = std::process::Command::new("security")
                    .args([
                        "delete-generic-password",
                        "-s",
                        "tachi-vault",
                        "-a",
                        "default",
                    ])
                    .output();
            }
        }
    }
}

/// RAII guard that sets an env var for the duration of a test and restores the
/// prior value on drop (mirrors `vault_ops::tests::EnvGuard`, which is private
/// to that module).
struct ScopedEnv {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prior }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Discrimination test for tachi#1080 on the in-process Keychain auto-unlock
/// path (`provider_config::auto_unlock_vault_from_keychain`) — the most safety-
/// critical of the six newly-gated sites, because it unlocks the live server.
///
/// RED before the fix: auto-unlock ignored `vault_config.kdf_params` and
/// derived with the compile-time constant, so with the CORRECT keychain
/// password the derived key matched the stored verifier and it returned
/// `Ok(true)` (installing the key into the live server) — this `expect_err`
/// would fail. After the fix, `parse_stored_kdf_params` rejects the unsupported
/// value before Argon2 runs, so auto-unlock returns the loud, versioned error
/// instead of silently unlocking with a compile-time-derived key.
#[tokio::test]
#[ignore = "touches the real macOS Keychain; run with `cargo test --ignored --test-threads=1`"]
async fn keychain_auto_unlock_rejects_unsupported_kdf_params() {
    // The #[cfg(test)] gate in auto_unlock_vault_from_keychain only runs the
    // real keychain path when this env var is present.
    let _env = ScopedEnv::set("TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK", "1");
    let _kc = ScopedKeychainEntry::install("correct-keychain-pw");

    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: "correct-keychain-pw".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    // Forge a valid-JSON but unsupported kdf_params directly in the stored
    // config (same with_global_store pattern as lifecycle/unsupported_kdf_params.rs).
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

    let err = crate::provider_config::auto_unlock_vault_from_keychain(&server)
        .expect_err("an unsupported kdf_params must fail keychain auto-unlock loudly, not unlock");

    assert!(
        !err.to_lowercase().contains("wrong password")
            && !err.to_lowercase().contains("does not match"),
        "an unsupported kdf_params is a format/version error, not a password mismatch: {err}"
    );
    assert!(
        err.contains("kdf_params") && err.contains("supported"),
        "the versioned error must name the stored kdf_params and the supported set: {err}"
    );
}

/// Discrimination test for tachi#1080 on the status-health Keychain loader
/// (`status_health::vault::load_keychain_vault_api_key_values`).
///
/// RED before the fix: the loader ignored `vault_config.kdf_params` and derived
/// with the compile-time constant, so with the CORRECT keychain password the
/// derived key matched the stored verifier and it returned `Ok(_)` (decrypting
/// entries, or an empty Vec if none matched the provider-key filter) — this
/// `expect_err` would fail. After the fix, `parse_stored_kdf_params` rejects
/// the unsupported value before `verify_password`, so the loader returns the
/// loud, versioned error instead of silently degrading to an empty Vec.
#[tokio::test]
#[ignore = "touches the real macOS Keychain; run with `cargo test --ignored --test-threads=1`"]
async fn status_health_keychain_loader_rejects_unsupported_kdf_params() {
    let _kc = ScopedKeychainEntry::install("correct-keychain-pw");

    let db_path = std::env::temp_dir().join(format!(
        "memory-server-status-health-kdf-gate-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    {
        let server = crate::server_state::MemoryServer::new(db_path.clone(), None)
            .expect("create test server");
        server
            .vault_init(Parameters(VaultInitParams {
                password: "correct-keychain-pw".to_string(),
            }))
            .await
            .expect("vault_init should succeed");
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
        // Drop the server so its SQLite connection closes before the
        // status-health loader opens the same DB read-only.
        drop(server);
    }

    let err = crate::status_ops::status_health::load_keychain_vault_api_key_values(&db_path)
        .expect_err("an unsupported kdf_params must fail the status-health loader loudly, not silently degrade");
    let msg = err.to_string();
    assert!(
        msg.contains("kdf_params") && msg.contains("supported"),
        "the versioned error must name the stored kdf_params and the supported set: {msg}"
    );

    let _ = std::fs::remove_file(&db_path);
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = db_path.clone().into_os_string();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(sidecar));
    }
}
