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
// Needed to rebuild an `OsString` from the raw password bytes captured by
// `security ... -w` so restore is byte-exact (the module is macOS-only, so the
// unix ffi ext is always available here).
use std::os::unix::ffi::OsStringExt;

/// RAII guard that installs a `tachi-vault`/`default` Keychain entry for the
/// test, capturing any pre-existing credential and restoring it (or removing
/// the entry) on drop so the developer's real Keychain is never clobbered.
///
/// SAFETY DISCIPLINE (tachi#1080 review):
/// - Capture is FAIL-CLOSED. If the pre-existing state cannot be determined
///   with certainty (keychain locked, permission error, anything that is not
///   "found" or "confirmed not-found"), `install` returns `None` and the test
///   is SKIPPED rather than risk overwriting/deleting a real credential.
/// - The captured password is stored and restored as RAW BYTES (via
///   `OsString::from_vec`), not `from_utf8_lossy().trim()` — that combo would
///   silently mangle a password containing non-UTF-8 bytes or leading/trailing
///   whitespace. Only the single trailing newline `security -w` appends is
///   stripped before restore.
/// - Restore/delete exit status is CHECKED; a failed cleanup panics loudly
///   instead of leaving a corrupted developer Keychain behind.
struct ScopedKeychainEntry {
    /// Raw bytes of the pre-existing password (one trailing newline already
    /// stripped), restored verbatim on drop. `None` ONLY when capture
    /// confirmed no prior entry exists.
    saved: Option<Vec<u8>>,
}

impl ScopedKeychainEntry {
    /// Install the test entry. Returns `None` when the keychain's prior state
    /// is ambiguous and the test should skip (caller returns without asserting
    /// — never clobbers).
    fn install(password: &str) -> Option<Self> {
        // Capture any existing entry so it can be restored. FAIL-CLOSED: only
        // proceed when the old state is UNAMBIGUOUS.
        let find = std::process::Command::new("security")
            .args(["find-generic-password", "-s", "tachi-vault", "-a", "default", "-w"])
            .output()
            .expect("security find-generic-password should run");
        let saved = if find.status.success() {
            // Capture raw bytes; strip ONLY the trailing newline that
            // `security -w` appends (NOT trim() — would alter a password with
            // leading/trailing spaces).
            let mut bytes = find.stdout;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            Some(bytes)
        } else {
            // Distinguish "confirmed not found" from an ambiguous failure
            // (locked keychain, auth, IO). `security` reports errSecItemNotFound
            // (-25300) when the item does not exist; anything else is ambiguous.
            let stderr = String::from_utf8_lossy(&find.stderr);
            let confirmed_not_found = stderr.contains("-25300")
                || stderr.contains("errSecItemNotFound")
                || stderr.contains("could not be found");
            if !confirmed_not_found {
                eprintln!(
                    "[keychain_kdf_gate] SKIP: keychain lookup for tachi-vault/default failed \
                     ambiguously (not a confirmed not-found); refusing to run a test that could \
                     clobber a real credential. stderr: {stderr}"
                );
                return None;
            }
            None
        };

        // Install the test entry. RESIDUAL RISK: `security add-generic-password
        // -w <password>` passes the password on the argv, where it is briefly
        // visible via `ps`. The macOS `security` CLI exposes no non-argv channel
        // for the password here. This is an ACCEPTED residual risk for this
        // synthetic test password ("correct-keychain-pw", not a real secret).
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

        Some(Self { saved })
    }
}

impl Drop for ScopedKeychainEntry {
    fn drop(&mut self) {
        let outcome: std::io::Result<std::process::Output> = match self.saved.as_ref() {
            Some(raw_bytes) => {
                // Restore the original credential's EXACT bytes via an OsString
                // built from raw bytes (handles non-UTF-8; avoids the
                // utf8_lossy+trim corruption). RESIDUAL RISK: -w on argv again
                // (see install); accepted for a restore path that only runs in
                // this ignored test.
                let pw = OsString::from_vec(raw_bytes.clone());
                std::process::Command::new("security")
                    .arg("add-generic-password")
                    .args(["-s", "tachi-vault"])
                    .args(["-a", "default"])
                    .arg("-w")
                    .arg(pw)
                    .arg("-U")
                    .output()
            }
            None => {
                // No prior entry; remove the one we added.
                std::process::Command::new("security")
                    .args([
                        "delete-generic-password",
                        "-s",
                        "tachi-vault",
                        "-a",
                        "default",
                    ])
                    .output()
            }
        };
        match outcome {
            Ok(out) if out.status.success() => { /* clean */ }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!(
                    "[keychain_kdf_gate] GUARD CLEANUP FAILED — the developer Keychain may be in \
                     a corrupted state for tachi-vault/default; manual inspection required: \
                     {stderr}"
                );
                panic!(
                    "ScopedKeychainEntry cleanup (restore/delete) failed; refusing to silently \
                     leave a corrupted developer Keychain. stderr: {stderr}"
                );
            }
            Err(e) => {
                eprintln!(
                    "[keychain_kdf_gate] GUARD CLEANUP FAILED — could not run security command: \
                     {e}; manual Keychain inspection required."
                );
                panic!("ScopedKeychainEntry cleanup could not run security: {e}");
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
    // Skip (do not clobber) if the prior keychain state is ambiguous.
    let _kc = match ScopedKeychainEntry::install("correct-keychain-pw") {
        Some(g) => g,
        None => return,
    };

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
    // Skip (do not clobber) if the prior keychain state is ambiguous.
    let _kc = match ScopedKeychainEntry::install("correct-keychain-pw") {
        Some(g) => g,
        None => return,
    };

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
