use super::handlers::{handle_vault_init, handle_vault_lock, handle_vault_unlock};
use super::params::{VaultInitParams, VaultUnlockParams};
use super::session::{read_unlock_password_fifo, with_vault_key};
use crate::server_state::MemoryServer;
use crate::test_support::EnvRestore;
use std::time::{Duration, Instant};

#[tokio::test]
async fn with_vault_key_drops_vault_lock_before_running_work() {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-lock-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        v.unlock_time = Some(Instant::now());
    }

    with_vault_key(&server, |cached_key| {
        assert_eq!(cached_key, &key);
        assert!(
            server.vault.try_write().is_ok(),
            "vault lock should not be held while user work runs"
        );
        Ok(())
    })
    .expect("vault key should be available");
}

// Discrimination test (a) for #400: auto-lock must clear the vault master key
// but PRESERVE provider secrets. Provider secrets are materialized from the
// vault at unlock time and have a lifecycle independent of the master key;
// auto-lock is "don't keep the key in memory long-term", NOT "stop the service".
//
// RED before fix: the old `with_vault_key` auto-lock path called
// `clear_provider_secrets()` + `re_materialize_provider_secrets_after_auto_lock()`,
// wiping the materialized key. On a headless Linux router (no Keychain) the
// re-materialize failed silently, leaving zero keys → the assertion
// `== Some("cached")` would fail because the value was gone.
#[tokio::test]
async fn auto_lock_clears_key_but_preserves_provider_secrets() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-auto-lock-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        v.unlock_time = Some(Instant::now() - Duration::from_secs(60));
        v.auto_lock_after_secs = 30;
    }
    assert!(server.llm.set_provider_secret("OPENAI_API_KEY", "cached"));

    let err = with_vault_key(&server, |_| Ok(())).expect_err("expired key should auto-lock");

    assert!(err.contains("Vault auto-locked"), "{err}");
    {
        let v = server.vault_read();
        assert!(v.key.is_none());
        assert!(v.unlock_time.is_none());
    }
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("cached"),
        "auto-lock must NOT clear provider secrets — they have a lifecycle independent of the vault master key"
    );
}

#[tokio::test]
async fn vault_unlock_rejects_password_and_fifo_path_together() {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-unlock-fifo-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "correct-password".to_string(),
        },
    )
    .await
    .expect("vault init should succeed");
    handle_vault_lock(&server)
        .await
        .expect("vault lock should succeed");

    let err = handle_vault_unlock(
        &server,
        VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: Some("/tmp/tachi-unlock-test.fifo".to_string()),
            use_keychain: false,
        },
    )
    .await
    .expect_err("mixed password transports should be rejected");

    assert!(
        err.contains("exactly one of password, password_fifo_path, or use_keychain"),
        "expected mixed-transport rejection, got: {err}"
    );
}

// codex 3.3 (fix-round): the mutual-exclusion check (`sources_given > 1`)
// covers all three pairwise combinations of password/password_fifo_path/
// use_keychain, but only the password+fifo pair had a discriminating test.
// These two cover the remaining pairs. Both must be rejected BEFORE any
// actual Keychain/FIFO read is attempted (the `sources_given` check runs
// first in `handle_vault_unlock`), so neither test needs a working FIFO or
// a Keychain injection seam — a nonexistent FIFO path and no
// `TACHI_TEST_KEYCHAIN_PASSWORD` override are both fine, since the mutex
// check must short-circuit before either is touched.
#[tokio::test]
async fn vault_unlock_rejects_password_and_keychain_together() {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-unlock-keychain-password-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "correct-password".to_string(),
        },
    )
    .await
    .expect("vault init should succeed");
    handle_vault_lock(&server)
        .await
        .expect("vault lock should succeed");

    let err = handle_vault_unlock(
        &server,
        VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
            use_keychain: true,
        },
    )
    .await
    .expect_err("mixed password transports should be rejected");

    assert!(
        err.contains("exactly one of password, password_fifo_path, or use_keychain"),
        "expected mixed-transport rejection, got: {err}"
    );
    let v = server.vault_read();
    assert!(
        v.key.is_none(),
        "vault must stay locked when the request is rejected as mixed-transport"
    );
}

#[tokio::test]
async fn vault_unlock_rejects_fifo_and_keychain_together() {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-unlock-keychain-fifo-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "correct-password".to_string(),
        },
    )
    .await
    .expect("vault init should succeed");
    handle_vault_lock(&server)
        .await
        .expect("vault lock should succeed");

    let err = handle_vault_unlock(
        &server,
        VaultUnlockParams {
            password: String::new(),
            password_fifo_path: Some("/tmp/tachi-unlock-test-fifo-keychain.fifo".to_string()),
            use_keychain: true,
        },
    )
    .await
    .expect_err("mixed password transports should be rejected");

    assert!(
        err.contains("exactly one of password, password_fifo_path, or use_keychain"),
        "expected mixed-transport rejection, got: {err}"
    );
    let v = server.vault_read();
    assert!(
        v.key.is_none(),
        "vault must stay locked when the request is rejected as mixed-transport"
    );
}

// tachi#1187 fix-round (attack-pass finding A1; leader adjudication,
// Option B): the pure-seam tests in `vault_crypto.rs`
// (`seam_rejects_algorithm_mismatch_as_typed_error_not_wrong_password`,
// `seam_algorithm_mismatch_does_not_feed_lockout_counter`) prove
// `derive_verified_key_from_stored_config` itself never routes
// algorithm-axis drift into a wrong-password outcome — but that guarantee
// was hollow as a production safety claim, because the LIVE
// `handle_vault_unlock` RPC handler did not call the seam at all: it
// re-implemented parse+derive+verify inline and never read
// `config.kdf_algorithm`. This test drives the real handler end-to-end (not
// a synthetic match) and asserts both that the error is the typed
// algorithm-mismatch message (never a generic wrong-password/lockout
// message) AND that `record_vault_unlock_failure`'s counter
// (`vault_read().failed_attempts.0`, the actual production lockout sink,
// `session.rs:156-168`) stays at zero.
//
// RED before the handler was wired to the seam: `handle_vault_unlock` never
// read `config.kdf_algorithm`, so it derived with Argon2id regardless of
// the stored label. This fixture keeps a legitimate Argon2id verifier (the
// real-world drift shape: a fork's `{m,t,p}` param shape matches, only the
// label differs), so `verify_password` returned `Ok(false)`, the handler
// matched that as a plain wrong-password miss, and `record_vault_unlock_failure`
// incremented the counter to 1 — feeding the brute-force lockout for a
// config-integrity problem, not a password problem.
#[tokio::test]
async fn vault_unlock_kdf_algorithm_mismatch_does_not_feed_lockout_counter() {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-unlock-kdf-algorithm-mismatch-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "correct-password".to_string(),
        },
    )
    .await
    .expect("vault init should succeed");
    handle_vault_lock(&server)
        .await
        .expect("vault lock should succeed");

    // Simulate algorithm-axis drift: rewrite the stored config's
    // `kdf_algorithm` to a value this build does not implement, keeping
    // salt/kdf_params/verifier exactly as `handle_vault_init` wrote them.
    let mut config = server
        .with_global_store(|store| store.vault_get_config().map_err(|e| e.to_string()))
        .expect("read vault config")
        .expect("vault config must exist after init");
    config.kdf_algorithm = "argon2i".to_string();
    server
        .with_global_store(|store| store.vault_set_config(&config).map_err(|e| e.to_string()))
        .expect("rewrite vault config with mismatched kdf_algorithm");

    let err = handle_vault_unlock(
        &server,
        VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
            use_keychain: false,
        },
    )
    .await
    .expect_err(
        "kdf_algorithm drift must fail the live unlock handler even with the correct password",
    );

    assert!(
        err.contains("kdf_algorithm") && err.contains("not a password error"),
        "expected the typed KdfAlgorithmMismatch message, got: {err}"
    );
    assert!(
        !err.contains("Wrong password") && !err.contains("Too many failed"),
        "algorithm-axis drift must never surface as a wrong-password/lockout message; got: {err}"
    );
    assert_eq!(
        server.vault_read().failed_attempts.0,
        0,
        "algorithm-axis drift must not increment the live vault unlock lockout counter"
    );
    assert!(
        server.vault_read().key.is_none(),
        "vault must stay locked when the stored kdf_algorithm is unsupported"
    );
}

// tachi#1175: `use_keychain` walks the same shared low-level primitive as
// the CLI's `tachi vault unlock --keychain`
// (`vault_crypto::read_password_from_macos_keychain`) so an agent never has
// to put a plaintext password in the tool call or shell. These two tests
// drive that primitive through its `TACHI_TEST_KEYCHAIN_PASSWORD` /
// `TACHI_TEST_FORCE_KEYCHAIN_MISSING` injection seam rather than the real
// macOS Keychain (a unit test must never depend on — or mutate — whatever
// `tachi-vault`/`default` Keychain entry happens to exist on the machine
// running the suite). `global_test_lock` serializes them against every other
// test that mutates process-global env vars (see
// `auto_lock_clears_key_but_preserves_provider_secrets` above for the same
// pattern).
// Plain `#[test]` + `block_on` (not `#[tokio::test]`), matching the
// `global_test_lock` convention used elsewhere in this crate (e.g.
// `dispatch_ops/prompt.rs`, `bootstrap::serve::stdio::tests`): the guard
// serializes the process-wide `TACHI_TEST_KEYCHAIN_PASSWORD` env var against
// other tests, so it must stay held across the whole init/lock/unlock
// sequence including its internal awaits — `block_on` runs that future to
// completion synchronously on this thread, so there is no `.await`
// expression in scope for clippy's `await_holding_lock` lint, while the
// guard's actual coverage is unchanged.
#[test]
fn vault_unlock_use_keychain_succeeds_via_injected_password() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _keychain_env = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", "correct-password");

    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-unlock-keychain-ok-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let body = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            handle_vault_init(
                &server,
                VaultInitParams {
                    password: "correct-password".to_string(),
                },
            )
            .await
            .expect("vault init should succeed");
            handle_vault_lock(&server)
                .await
                .expect("vault lock should succeed");

            handle_vault_unlock(
                &server,
                VaultUnlockParams {
                    password: String::new(),
                    password_fifo_path: None,
                    use_keychain: true,
                },
            )
            .await
            .expect("use_keychain unlock should succeed via the injected Keychain password")
        });

    let body_json: serde_json::Value =
        serde_json::from_str(&body).expect("vault_unlock response should be JSON");
    assert_eq!(body_json["unlocked"], serde_json::json!(true));
    let v = server.vault_read();
    assert!(
        v.key.is_some(),
        "vault key should be cached after a successful use_keychain unlock"
    );
}

// Same `#[test]` + `block_on` conversion as
// `vault_unlock_use_keychain_succeeds_via_injected_password` above — the
// guard here serializes `TACHI_TEST_FORCE_KEYCHAIN_MISSING`.
#[test]
fn vault_unlock_use_keychain_reports_missing_entry_without_leaking_process_error() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _keychain_env = EnvRestore::set("TACHI_TEST_FORCE_KEYCHAIN_MISSING", "1");

    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-unlock-keychain-missing-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let err = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            handle_vault_init(
                &server,
                VaultInitParams {
                    password: "correct-password".to_string(),
                },
            )
            .await
            .expect("vault init should succeed");
            handle_vault_lock(&server)
                .await
                .expect("vault lock should succeed");

            handle_vault_unlock(
                &server,
                VaultUnlockParams {
                    password: String::new(),
                    password_fifo_path: None,
                    use_keychain: true,
                },
            )
            .await
            .expect_err("missing Keychain entry should fail, not silently succeed")
        });

    assert!(
        err.contains("use_keychain unlock failed"),
        "expected the use_keychain wrapper context, got: {err}"
    );
    assert!(
        err.contains("no vault password found in Keychain"),
        "expected the real missing-entry message, got: {err}"
    );
    let v = server.vault_read();
    assert!(
        v.key.is_none(),
        "vault must stay locked when the Keychain entry is missing"
    );
}

#[cfg(unix)]
#[test]
fn read_unlock_password_fifo_reads_secure_runtime_fifo() {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    // #1096 leaf-2a: `read_unlock_password_fifo` now takes `home` as an
    // explicit parameter instead of re-deriving it from `TACHI_HOME` env
    // internally (only env read the function body had), so this test no
    // longer needs to mutate process env or hold `global_test_lock` against
    // other env-dependent tests. NOT independently re-run here (edit-only
    // lane) — Oz must confirm this passes under parallel `cargo test`
    // before the lock removal is trusted.
    let temp = tempfile::tempdir().expect("temp tachi home");
    let unlock_dir = temp.path().join("runtime").join("vault-unlock");
    std::fs::create_dir_all(&unlock_dir).expect("unlock dir");
    std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))
        .expect("unlock dir perms");
    let fifo_path = unlock_dir.join("unlock-test.fifo");
    let c_path = CString::new(fifo_path.as_os_str().as_bytes()).expect("fifo path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

    let writer_path = fifo_path.clone();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .expect("open fifo writer");
        file.write_all(b"fifo-password").expect("write fifo");
    });

    let password =
        read_unlock_password_fifo(temp.path(), fifo_path.to_str().unwrap()).expect("read fifo");
    writer.join().expect("writer thread");
    assert_eq!(password, "fifo-password");
    assert!(!fifo_path.exists(), "daemon reader should remove FIFO");
}

#[cfg(unix)]
#[test]
fn read_unlock_password_fifo_rejects_regular_file_without_removing_it() {
    use std::os::unix::fs::PermissionsExt;

    // #1096 leaf-2a: see comment in
    // `read_unlock_password_fifo_reads_secure_runtime_fifo` above — `home` is
    // now an explicit parameter, so no env mutation / global_test_lock needed.
    let temp = tempfile::tempdir().expect("temp tachi home");
    let unlock_dir = temp.path().join("runtime").join("vault-unlock");
    std::fs::create_dir_all(&unlock_dir).expect("unlock dir");
    std::fs::set_permissions(&unlock_dir, std::fs::Permissions::from_mode(0o700))
        .expect("unlock dir perms");
    let regular_path = unlock_dir.join("not-a-fifo");
    std::fs::write(&regular_path, b"not a fifo").expect("regular file");

    let err = read_unlock_password_fifo(temp.path(), regular_path.to_str().unwrap())
        .expect_err("regular file");
    assert!(
        err.contains("must be a FIFO"),
        "expected FIFO rejection, got: {err}"
    );
    assert!(
        regular_path.exists(),
        "rejected non-FIFO input should not be removed"
    );
}

// G-B5 (updated for #400): auto-lock no longer clears provider secrets.
// The original re-materialization path (`re_materialize_provider_secrets_after_auto_lock`)
// was production dead code after the split — it had no callers outside tests —
// so both the function and the test that exercised it were removed (正本清源).
// The surviving discrimination test (a) `auto_lock_clears_key_but_preserves_provider_secrets`
// already proves auto-lock preserves provider secrets.

// Discrimination test (b) for #400: user-initiated `vault lock` must clear BOTH
// the vault master key AND provider secrets — the original full-clear semantics
// must be preserved when the split was introduced.
//
// This is a regression guard: it passes both before and after the fix by
// construction (the fix does not touch `handle_vault_lock`). Its purpose is to
// prove the split did not accidentally weaken the user-facing lock command.
#[test]
fn vault_lock_clears_key_and_provider_secrets() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-lock-full-clear-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        v.unlock_time = Some(Instant::now());
    }
    assert!(server.llm.set_provider_secret("OPENAI_API_KEY", "cached"));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create Tokio runtime")
        .block_on(handle_vault_lock(&server))
        .expect("vault lock should succeed");

    {
        let v = server.vault_read();
        assert!(v.key.is_none(), "vault lock must clear the master key");
        assert!(v.unlock_time.is_none(), "vault lock must clear unlock_time");
    }
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        None,
        "vault lock must clear provider secrets (full clear)"
    );
}

// Discrimination test (c) for #400: TACHI_VAULT_AUTOLOCK_SECS=0 disables
// auto-lock entirely — the key survives even past the normal timeout.
//
// RED before fix: there was no env knob; `auto_lock_after_secs` was hardcoded
// to 1800. The expired key would auto-lock regardless of any env value, and the
// assertion `key.is_some()` would fail.
#[tokio::test]
async fn autolock_disabled_when_env_zero() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::set("TACHI_VAULT_AUTOLOCK_SECS", "0");
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-autolock-disabled-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");

    assert_eq!(
        server.vault_read().auto_lock_after_secs,
        0,
        "TACHI_VAULT_AUTOLOCK_SECS=0 must set auto_lock_after_secs to 0"
    );

    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        // Expired well beyond any normal timeout.
        v.unlock_time = Some(Instant::now() - Duration::from_secs(9999));
    }
    assert!(server.llm.set_provider_secret("OPENAI_API_KEY", "cached"));

    // With auto-lock disabled, the key is still valid even though unlock_time
    // is far in the past.
    let result = with_vault_key(&server, |k| {
        assert_eq!(
            k, &key,
            "key must still be accessible with auto-lock disabled"
        );
        Ok(())
    });
    result.expect("vault key should be available with auto-lock disabled");

    {
        let v = server.vault_read();
        assert!(
            v.key.is_some(),
            "key must survive past timeout when auto-lock is disabled"
        );
        assert!(
            v.unlock_time.is_some(),
            "unlock_time must not be cleared when auto-lock is disabled"
        );
    }
}

// Discrimination test (d) for #400: when auto-lock is disabled
// (`TACHI_VAULT_AUTOLOCK_SECS=0`), the runtime status surface must report
// `vault.unlocked: true` even if `unlock_time` is far in the past.
//
// RED before fix: `runtime_observability_json` computed `unlocked` with the
// bare predicate `elapsed <= auto_lock_after_secs`, which has no `0 = never
// expires` sentinel. With `auto_lock_after_secs == 0` and any `elapsed > 0`
// (here 9999s), `9999 <= 0` is `false`, so the status path reported
// `unlocked: false` one second after unlock — contradicting the enforcement
// path, which still held the key live and usable. A monitoring poll reading
// the runtime block would see `locked: true` while the daemon was happily
// serving with a live key.
#[tokio::test]
async fn autolock_disabled_reports_unlocked_in_runtime_status() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::set("TACHI_VAULT_AUTOLOCK_SECS", "0");
    let _openai_env = EnvRestore::remove("OPENAI_API_KEY");
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-autolock-disabled-status-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path.clone(), None).expect("create test server");

    assert_eq!(
        server.vault_read().auto_lock_after_secs,
        0,
        "TACHI_VAULT_AUTOLOCK_SECS=0 must set auto_lock_after_secs to 0"
    );

    let key = [9u8; 32];
    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(&key));
        // Expired well beyond any normal timeout.
        v.unlock_time = Some(Instant::now() - Duration::from_secs(9999));
    }

    let runtime = crate::status_ops::runtime_observability_json(
        &server,
        &db_path,
        Some(&crate::status_ops::DaemonStatus::None),
        false,
    );
    let vault = &runtime["vault"];
    assert_eq!(
        vault["unlocked"],
        serde_json::json!(true),
        "with auto-lock disabled, runtime status must report unlocked: true even past the normal timeout (status path must match the enforcement path)"
    );
    assert_eq!(
        vault["auto_lock_after_seconds"],
        serde_json::json!(0),
        "runtime status should still surface the configured auto_lock_after_secs"
    );
}

// macOS Keychain auto-unlock integration smoke. Ignored by default because it
// touches the real developer Keychain; run explicitly with `--ignored`.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "touches real macOS Keychain; run explicitly with --ignored"]
async fn keychain_auto_unlock_smoke() {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-vault-keychain-smoke-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");

    // The keychain-availability probe and the auto-unlock path must run without
    // panicking regardless of whether a real `tachi-vault/default` entry exists.
    let available = crate::provider_config::keychain_vault_password_entry_available()
        .expect("keychain availability probe should return a Result, not panic");
    let unlocked = crate::provider_config::auto_unlock_vault_from_keychain(&server)
        .expect("auto-unlock attempt should return a Result, not panic");
    eprintln!("keychain_available={available} auto_unlocked={unlocked}");
}
